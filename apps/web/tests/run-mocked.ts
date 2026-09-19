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
// in-browser mock and reaches the dev-server itself (it answers 404 in
// mocked mode and names the miss in its exit summary; until 2026-09-19
// it proxied on and printed `Unable to connect`) — and the page never
// mounts, so `toBeVisible` fails. It has false-redded Rust-only cars
// that never touched the frontend.
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
import { MOCKED_FLAG } from '../src/dev-mocked';
import { waitForReadyLine } from '../src/dev-ready';
import { DEFAULT_PORT, chooseTarget } from '../src/dev-tree';

const PREFERRED_PORT = Number(process.env['PORT'] ?? DEFAULT_PORT);

// How long a server we started gets to SAY it is listening (the line
// dev-server.ts prints once serve() returns — src/dev-ready.ts) before
// the first `/` probe. Binding is milliseconds; the bound is for a
// host so starved that even the import graph crawls, and a server that
// dies or hangs before it fails loudly, naming the port, instead of
// spending READY_DEADLINE_MS on probes of a port nothing serves.
const LISTENING_TIMEOUT_MS = 60_000;

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
    const started = Bun.spawn(['bun', 'src/dev-server.ts'], {
      // BOSS_MOCKED=1: every /api call is mocked in-browser and no
      // backend runs, so the dev-server answers a miss 404 locally and
      // prints one summary line of them at exit instead of proxying to
      // a port nothing listens on — six lines of bun connect noise per
      // request until 2026-09-19 (82b87a09). BOSS_SCRATCH=0 keeps the
      // (now unreached) proxy table off the scratch ports.
      env: {
        ...process.env,
        PORT: String(target.port),
        BOSS_SCRATCH: '0',
        [MOCKED_FLAG]: '1',
      },
      // Piped, not inherited: the ready line is read off it below, and
      // every line is echoed on so the "Bundled page in Xms" diagnostic
      // stays visible.
      stdout: 'pipe',
      stderr: 'inherit',
      stdin: 'ignore',
    });
    devServer = started;
    weStartedIt = true;
    // The server's own statement that it is listening comes first: a
    // spec launched before it connects to nothing, and that is the 261
    // `Unable to connect` lines that redded a gate and were green on
    // the re-gate (backlog aa828f3e, class 63a242ca). The HTTP probe
    // below still gates the BUNDLE; this gates the bind.
    await waitForReadyLine(started.stdout, target.port, LISTENING_TIMEOUT_MS, (line) => console.log(line));
    log(`server reports listening on :${target.port}`);
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
  // Anything after the script (a spec path, -g, --repeat-each) goes to
  // Playwright: until 2026-09-18 `bun tests/run-mocked.ts -- one.spec.ts`
  // ran the WHOLE suite and said nothing about the argument it dropped.
  const extra = process.argv.slice(2).filter((a, i) => !(i === 0 && a === '--'));
  const playwright = Bun.spawn(
    ['bun', 'x', 'playwright', 'test', '-c', 'playwright.mocked.config.ts', ...extra],
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
  // Let the server print its miss summary (on SIGTERM) before this
  // process exits, so the line lands inside the run's own output —
  // bounded, because a server that will not stop is not worth waiting on.
  if (devServer && weStartedIt) {
    await Promise.race([devServer.exited, Bun.sleep(5_000)]);
  }
  process.exit(code);
}

main().catch((err: unknown) => {
  console.error(`[mocked-runner] ${err instanceof Error ? err.message : String(err)}`);
  stopDevServer();
  process.exit(1);
});
