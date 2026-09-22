// THE MACHINE-RUN PIN ON THE MOCKED RUNNER'S LEAK REFUSAL (backlog
// 2847f813).
//
// tests/run-mocked.ts exits 1 when the dev-server's shutdown summary
// names /api/** reads the in-browser mock did not answer — the thing
// that holds the api floor car f88e7908 built (403 leaked reads to
// none) in place. Until this file the refusal was pinned only as PURE
// FUNCTIONS (isMissSummary, missRefusal in src/dev-mocked.test.ts) plus
// a hand rehearsal against a throwaway floorless spec that was
// deliberately never committed, because a committed floorless spec
// under tests/mocked would red the suite by design. So the END-TO-END
// path — leaked read -> server counts it -> server prints the summary
// on SIGTERM -> runner reads that line off stdout -> exit 1 — was
// exactly what CLAUDE.md calls mostly sure: observed once, by one
// builder, and not inherited by the next shift.
//
// WHAT THE PURE FUNCTIONS CANNOT SEE, and this file can: that the
// server prints the line at all, that it prints it before the runner
// takes its verdict (the stdout DRAIN wait), that the runner's stdout
// reader recognises it, and that the refusal OVERRIDES a green
// Playwright. Four joints, none of them a pure function.
//
// WHY HERE. The packet proposed a boss-testing shell test, but the
// gate's `test` phase runs before `web install` (infra/gate.sh), so a
// Rust-side test would drive a runner with no node_modules and no
// browser. `bun test src/ scripts/` is the first web phase that has
// both, so the pin lives beside open-page-audits.test.ts and runs in
// `bun run test:unit`.
import { expect, test } from 'bun:test';

import { freePort } from '../src/dev-tree';
import { MISS_SUMMARY_PREFIX } from '../src/dev-mocked';

// apps/web — this file is in scripts/.
const WEB_ROOT = new URL('..', import.meta.url).pathname;

// The path tests/leak-pin/leaks-one-api-read.spec.ts leaks. Spelled in
// both files on purpose: the pin's claim is that the runner's output
// NAMES the leaked route, so the pin has to know the route to look for.
const LEAKED_PATH = '/api/leak-pin/2847f813';

// One dev-server boot (the `/` request is held open for the whole SPA
// bundle — ~2s idle, ~30s under 2x CPU load) plus one chromium launch
// and one spec. The runner's own budget is 240s; this leaves room for
// it to fail loudly rather than being killed here first.
const RUN_TIMEOUT_MS = 300_000;

test(
  'the mocked runner refuses a run that leaked an /api read to the dev-server',
  async () => {
    // Our own port, so this never disturbs — or is disturbed by — an
    // operator dev-server on the preferred 5174 (backlog eaca07e1).
    const port = await freePort();
    const proc = Bun.spawn(['bun', 'tests/run-mocked.ts'], {
      cwd: WEB_ROOT,
      env: {
        ...process.env,
        PORT: String(port),
        // The one seam: playwright.mocked.config.ts collects this
        // directory instead of tests/mocked, so the floorless fixture
        // is never part of the normal suite.
        BOSS_MOCKED_TEST_DIR: './tests/leak-pin',
      },
      stdout: 'pipe',
      stderr: 'pipe',
      stdin: 'ignore',
    });
    const [out, err, code] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
      proc.exited,
    ]);
    const output = `${out}\n${err}`;

    // Playwright ITSELF was green — so an exit 1 below can only be the
    // refusal. Without this the pin would pass just as well against a
    // fixture that failed to mount, which is a different defect wearing
    // the same exit code.
    expect(output).toContain('1 passed');

    // The server counted the miss and said so, naming the route.
    expect(output).toContain(MISS_SUMMARY_PREFIX);
    expect(output).toContain(`GET ${LEAKED_PATH} x1`);

    // And the runner turned that line into a refusal, not an echo.
    expect(output).toContain('refusing this run');
    expect(output).toContain('installApiFloor(page)');
    expect(code).toBe(1);
  },
  RUN_TIMEOUT_MS,
);
