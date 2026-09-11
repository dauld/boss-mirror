// Robust entrypoint for the mocked-backend Playwright suite
// (`bun run test:mocked`, run by BOTH infra/gate.sh's web-suite check
// and .forgejo/workflows/ci.yml's `web` job).
//
// WHY THIS EXISTS — the connect-before-ready flake (backlog 63a242ca).
// The suite runs Playwright against the Bun fullstack dev-server on
// 127.0.0.1:5174. That server binds its port in milliseconds but does
// NOT serve `/` until it has bundled the whole SPA graph (136 .svelte +
// 114 .ts through bun-plugin-svelte) on the first request. Measured cold
// on this pod: ~2s idle, but ~28-33s under 2x CPU load (`Bundled page in
// 30304ms`) — the bundle competes for CPU and page cache when several
// gates build concurrently.
//
// Letting Playwright's own `webServer.url` poll wait for that is a race:
// its readiness probe (playwright-core isURLAvailable) issues an HTTP GET
// that is held open by the dev-server during the bundle, and that GET
// carries a HARDCODED 30s net-idle timeout (NET_DEFAULT_TIMEOUT) that no
// config knob can raise. So a ~30s bundle lands right on the cutoff: the
// probe idle-times-out at 30s exactly as the bundle finishes. It usually
// recovers only because Bun keeps compiling after the client disconnects
// and caches the result for the next probe — an undocumented margin one
// Bun upgrade could erase, and one that frays further as concurrency
// pushes the bundle past 30s. When the probe loses the race the specs
// connect to a server still mid-bundle: every `/api/**` call misses the
// in-browser mock and hits the dev-server's real proxy (no backend in
// mocked mode) — the `[WebServer] ... Unable to connect` lines — and the
// page never mounts, so `toBeVisible` fails. It has false-redded
// Rust-only cars that never touched the frontend.
//
// THE FIX: own the readiness gate here, with a generous per-attempt
// timeout we control. A GET to `/` both triggers the bundle and is held
// until it completes, so a single attempt with a 210s cap swallows a
// slow bundle whole — no 30s cutoff, no reliance on Bun's post-disconnect
// caching. We poll across the brief port-bind window, fail loudly if the
// server never serves (a truly broken server still reds the gate — this
// does not mask failure), and only then hand off to Playwright with
// PWTEST_SKIP_DEVSERVER=1 so it runs against the already-ready server
// instead of starting its own. It is a real readiness gate, not a sleep.

// WHICH SERVER, NOT JUST WHETHER ONE ANSWERS.
//
// This file used to reuse anything that returned 2xx on `/` at the
// preferred port. On a pod where several worktrees share one network
// namespace that is a false-green machine: on 2026-09-08 two runs here
// reported 94 passes against ANOTHER worktree's bundle, because an
// operator dev-server held :5174 (backlog eaca07e1). `chooseTarget`
// answers the question that actually matters — is the server on that
// port serving THIS tree — and refuses to reuse anything else. Under CI
// it refuses unconditionally. See src/dev-tree.ts.
import { DEFAULT_PORT, chooseTarget } from '../src/dev-tree';

const PREFERRED_PORT = Number(process.env['PORT'] ?? DEFAULT_PORT);

// Overall budget to reach a served 200 on `/` (i.e. the SPA bundle has
// compiled). Generous enough to survive concurrent-gate I/O pressure,
// bounded so a genuinely broken server fails loudly rather than hanging.
const READY_DEADLINE_MS = 240_000;
// Per-attempt cap. A `/` request is held open for the whole bundle, so
// this must exceed the worst bundle time under load (~30s at 2x CPU;
// this leaves ~7x headroom) while staying under the overall deadline.
const ATTEMPT_TIMEOUT_MS = 210_000;

function log(msg: string): void {
  console.log(`[mocked-runner] ${msg}`);
}

// A served 2xx on `/` means the shell is up AND — because the dev-server
// holds `/` open until the bundle finishes — the SPA has been compiled.
// A connection error, timeout, or non-2xx counts as not-ready.
async function isServed(readyUrl: string, timeoutMs: number): Promise<boolean> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await fetch(readyUrl, { signal: controller.signal });
    await res.body?.cancel();
    return res.ok;
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
  }
}

let devServer: ReturnType<typeof Bun.spawn> | null = null;
let weStartedIt = false;

function stopDevServer(): void {
  if (devServer && weStartedIt) {
    try {
      devServer.kill('SIGTERM');
    } catch {
      // Already gone — nothing to stop.
    }
  }
}

process.on('exit', stopDevServer);
process.on('SIGINT', () => {
  stopDevServer();
  process.exit(130);
});
process.on('SIGTERM', () => {
  stopDevServer();
  process.exit(143);
});

async function main(): Promise<never> {
  // Decide WHICH server this run tests, and say so out loud: a run that
  // does not name its server is a run whose green means nothing.
  const target = await chooseTarget(PREFERRED_PORT, process.env);
  const origin = `http://127.0.0.1:${target.port}`;
  const readyUrl = `${origin}/`;
  log(target.reason);

  if (!target.reuse) {
    log(`starting dev-server on :${target.port} ...`);
    devServer = Bun.spawn(['bun', 'src/dev-server.ts'], {
      // BOSS_SCRATCH=0: every /api call is mocked in-browser, so the
      // proxy target is irrelevant; 0 just avoids the scratch ports.
      env: { ...process.env, PORT: String(target.port), BOSS_SCRATCH: '0' },
      stdout: 'inherit', // keep the "Bundled page in Xms" diagnostic line visible
      stderr: 'inherit',
      stdin: 'ignore',
    });
    weStartedIt = true;
  }

  // Readiness is checked the same way either way — a reused server that
  // is mid-bundle gets the same generous wait as one we just started.
  // Race each probe against the process exiting: if the dev-server dies
  // (e.g. a port taken between the choice and the bind), fail in seconds
  // instead of blocking inside a held-open `/` request until the cap.
  const exited: Promise<{ kind: 'exited'; code: number }> =
    devServer?.exited.then((code) => ({ kind: 'exited' as const, code }))
    ?? new Promise(() => {
      /* we did not start it — it cannot exit under us */
    });
  const deadline = Date.now() + READY_DEADLINE_MS;
  let ready = false;
  while (Date.now() < deadline) {
    const remaining = deadline - Date.now();
    const outcome = await Promise.race([
      isServed(readyUrl, Math.min(ATTEMPT_TIMEOUT_MS, remaining)).then((ok) => ({
        kind: 'served' as const,
        ok,
      })),
      exited,
    ]);
    if (outcome.kind === 'exited') {
      throw new Error(
        `dev-server exited early (code ${outcome.code}) before serving ${readyUrl}`,
      );
    }
    if (outcome.ok) {
      ready = true;
      break;
    }
    await Bun.sleep(250); // brief backoff across the port-bind window
  }
  if (!ready) {
    throw new Error(
      `dev-server did not serve ${readyUrl} within ${READY_DEADLINE_MS}ms — treating as broken, not flaky`,
    );
  }
  log(`server ready at ${origin}`);

  // Hand off to Playwright against the now-ready server. PWTEST_SKIP_DEVSERVER
  // is honoured by playwright.mocked.config.ts (its webServer block goes
  // undefined) so Playwright does not start a second server or re-run the
  // fragile 30s-capped probe. PORT carries the chosen port through to the
  // config's baseURL — the suite must point at the server we vetted, not
  // at the default.
  const playwright = Bun.spawn(
    ['bun', 'x', 'playwright', 'test', '-c', 'playwright.mocked.config.ts'],
    {
      env: {
        ...process.env,
        PWTEST_SKIP_DEVSERVER: '1',
        PORT: String(target.port),
      },
      stdout: 'inherit',
      stderr: 'inherit',
      stdin: 'inherit',
    },
  );
  const code = await playwright.exited;
  stopDevServer();
  process.exit(code);
}

main().catch((err: unknown) => {
  console.error(`[mocked-runner] ${err instanceof Error ? err.message : String(err)}`);
  stopDevServer();
  process.exit(1);
});
