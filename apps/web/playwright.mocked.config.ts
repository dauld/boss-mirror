// Mocked-backend Playwright config — the CI-gated frontend smoke layer.
//
// The specs under tests/mocked intercept EVERY `/api/**` call
// in-browser, so this config needs only the dev-server serving the SPA
// shell — no backend, no seeding — which makes it fast, deterministic,
// and safe to gate in the fast `web` CI job. See tests/mocked/_mockApi.ts.
// (Until 2026-09-18 a sibling playwright.config.ts ran a live-backend
// suite against a scratch stack; nothing ran it, and it went with
// design 0e07ce64. The live crawl is playwright.live.config.ts.)

import { defineConfig } from '@playwright/test';

import { MOCKED_FLAG } from './src/dev-mocked';
import { DEFAULT_PORT } from './src/dev-tree';

// The port the runner chose. It is usually DEFAULT_PORT, but when the
// preferred port is held by a server serving a DIFFERENT tree the runner
// starts its own on a free one and passes it here — so baseURL must be
// derived, never hardcoded, or the suite would go back to testing
// whatever happens to be on :5174 (backlog eaca07e1).
const PORT = Number(process.env['PORT'] ?? DEFAULT_PORT);
const ORIGIN = `http://127.0.0.1:${PORT}`;

// The normal entrypoint is `bun run test:mocked` → tests/run-mocked.ts,
// which starts the dev-server, waits for a genuinely-served `/` under a
// generous timeout it controls, then invokes Playwright with
// PWTEST_SKIP_DEVSERVER=1 — so the webServer block below is DORMANT on
// the gated path. It stays as the fallback for a direct
// `playwright test -c playwright.mocked.config.ts` (debugging, --ui,
// --headed, a single spec). See tests/run-mocked.ts for why readiness
// moved out of Playwright's own url-probe (the 30s-capped race).
const skipDevServer = process.env['PWTEST_SKIP_DEVSERVER'] === '1';

// WHICH DIRECTORY THE RUN COLLECTS — tests/mocked for every real run,
// and one seam for the leak pin (backlog 2847f813).
//
// tests/run-mocked.ts refuses a run whose dev-server answered /api/**
// reads the mock did not, and that refusal could only ever be rehearsed
// by hand: the fixture it needs is a spec with NO installApiFloor, and
// such a spec under tests/mocked would red the suite by design. So the
// fixture lives at tests/leak-pin/ — collected by nothing, invisible to
// `bun run test:mocked` — and scripts/mocked-runner-refuses-a-leak.test.ts
// points this at it for one run. Nothing else sets the variable.
const TEST_DIR = process.env['BOSS_MOCKED_TEST_DIR'] ?? './tests/mocked';

export default defineConfig({
  testDir: TEST_DIR,
  // THIS SUITE'S LOAD BUDGET, STATED IN ONE PLACE (backlog e6bc776b).
  //
  // Every wait below used to be a framework default: Playwright gives an
  // unstated `expect` 5 000 ms, and gives an unstated click or goto no cap
  // of its own at all — they simply consume the per-test timeout, so the
  // failure that arrives names the test rather than the thing it was
  // waiting for. Nobody chose 5 000 ms for this pod, and three of the four
  // specs in the load-flake report were failing against it (false-empty
  // declares no budget anywhere; guest-signin and incident-review declare
  // one for the mount through `mountPage` and then assert on the default).
  //
  // WHAT LOAD DOES TO THIS SUITE, measured 2026-09-22 on the dev pod
  // (16-CPU cgroup quota, 32 host CPUs) by running the four named specs
  // unchanged against a bounded CPU load:
  //
  //   quiet                      16/16, light tests 0.45-0.73 s
  //   1.5x oversubscription      16/16, light tests 1.7 -2.7  s
  //   4x                         16/16, light tests 3.7 -5.6  s
  //   10x                        16/16, light tests 4.8 -7.8  s
  //   full suite (129) at 2x     129/129, false-empty 5.9 s
  //
  // Nothing about the page differs under load — only how long it takes to
  // paint, and by a factor of ten. A budget of 5 000 ms is inside that
  // band; 15 000 ms is not. The asymmetry is the same one tests/run-mocked.ts
  // argues for its readiness gate, and it is the reason these numbers are
  // set high rather than tuned: a generous budget costs wall-clock only in a
  // run that is already failing, while a tight one costs a whole suite of
  // false red that reads as an app regression — ~10 minutes of one of three
  // gate bays, plus the builder time to prove the diff innocent.
  //
  // It is NOT a retry and it hides nothing: a surface that never renders
  // still fails, with the same message, 10 s later. Only the starved one
  // stops lying.
  //
  // scripts/the-mocked-suite-states-its-budget.test.ts pins that these stay
  // stated, because the alternative is what happened for the last month —
  // one constant discovered and bumped per red, in whichever file paid for
  // it (interaction-crawl's CLICK_TIMEOUT_MS 3 000 -> 15 000 after train
  // #461 on 2026-09-18; _helpers.ts's mountPage 10 000; outage-crawl's
  // 20 000), five numbers in five files and none of them readable as this
  // suite's budget.
  timeout: 60_000,
  expect: { timeout: 15_000 },
  retries: process.env['CI'] ? 1 : 0,
  reporter: [['list']],
  use: {
    // Bounded here rather than left to eat the test timeout, so a click on
    // a starved renderer reports as a click that waited 15 s and not as a
    // test that died somewhere.
    actionTimeout: 15_000,
    // A goto also triggers the dev-server's on-the-fly bundle of the route
    // it lands on, which is the slowest thing in the suite under load —
    // hence twice the action budget.
    navigationTimeout: 30_000,
    baseURL: ORIGIN,
    headless: true,
    viewport: { width: 1280, height: 800 },
  },
  // No globalSetup — these specs mock the backend, so there is nothing
  // to seed.
  //
  // THE MASS-FAIL SIGNATURE, AND HOW TO TELL THE TWO CAUSES APART.
  // Three times in the week of 2026-08-17 this suite reported 60-70
  // failures in about 20 seconds — every spec dead on connect. That
  // number reads like the app collapsed; it never is. Nothing was
  // serving 127.0.0.1:5174, and there are exactly two ways for that to
  // happen here:
  //
  //   1. THE BOOT RAN OUT OF TIME. Playwright aborts the whole run with
  //      "Timed out waiting 60000ms from config.webServer" and no spec
  //      gets to fail — so if you see that line, this is your cause.
  //      The wait is not a health ping: `bun src/dev-server.ts` bundles
  //      136 .svelte components and 114 .ts modules through
  //      bun-plugin-svelte before / answers 200 at all. Measured on an
  //      M-series laptop, first boot in a fresh checkout: 5.1-6.0s from
  //      process start to that first 200, nearly all of it the bundle
  //      (the server's own line reads `Bundled page in 5966ms`). A
  //      shared, throttled CI container starting from a cold page cache
  //      is a large multiple of that, and 60s was only ~10x the laptop
  //      figure — close enough to lose the race under load. 180s is
  //      ~30x it. The asymmetry is why it is set high rather than
  //      tuned: a too-generous timeout costs wall-clock only in a run
  //      that is already failing, while a too-tight one costs a whole
  //      suite of false red that reads as an app regression.
  //
  //   2. NOTHING WAS EVER STARTED. If instead the specs DO run and each
  //      fails fast on connect, the webServer block was skipped or its
  //      process exited immediately — check PWTEST_SKIP_DEVSERVER in
  //      the environment first (it is honoured just below, and a stale
  //      export from a debugging session survives in a tmux shell), and
  //      read the [WebServer] lines for a port already in use.
  //
  // stdout is piped for that reason: the default swallows the dev
  // server's output, so a boot that fails or crawls leaves no evidence
  // and the only visible artefact is the pile of dead specs. Piped, the
  // run carries its own measurement —
  //
  //   [WebServer] Bundled page in 5966ms: index.html
  //
  // — which is the number to compare when a container looks slow, and
  // the line that is missing entirely when cause 2 is what happened.
  webServer: skipDevServer
    ? undefined
    : {
        command: 'bun src/dev-server.ts',
        url: `${ORIGIN}/`,
        // false under CI so a stale/occupied port fails loudly instead
        // of silently reusing a possibly-broken server; true locally for
        // fast re-runs against a `bun run dev` already up.
        //
        // NOTE: Playwright's own reuse test is "does the url answer" —
        // it cannot check WHICH tree answers, which is the whole of
        // eaca07e1. That check lives in tests/run-mocked.ts, the
        // entrypoint every gated run uses, so this dormant fallback is
        // the one place reuse is still identity-blind. It is reached
        // only by invoking playwright directly (--ui, --headed, one
        // spec) — a hand-driven debugging path where the operator reads
        // the output — so it keeps the faster ergonomics.
        reuseExistingServer: !process.env['CI'],
        timeout: 180_000,
        stdout: 'pipe',
        stderr: 'pipe',
        // The same environment tests/run-mocked.ts gives the server:
        // mocked mode answers an /api/** miss locally (82b87a09), and
        // BOSS_SCRATCH=0 keeps the unreached proxy table off the
        // scratch ports.
        env: { BOSS_SCRATCH: '0', [MOCKED_FLAG]: '1' },
      },
  projects: [
    {
      name: 'chromium',
      use: {
        browserName: 'chromium',
        launchOptions: { args: ['--no-sandbox', '--disable-dev-shm-usage'] },
      },
    },
  ],
});
