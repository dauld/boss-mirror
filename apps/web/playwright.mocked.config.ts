// Mocked-backend Playwright config — the CI-gated frontend smoke layer.
//
// Unlike playwright.config.ts (which runs the suite against the live
// backend via the dev-server proxy + seeds a scratch stack in
// globalSetup), this config runs specs that intercept EVERY `/api/**`
// call in-browser. So it needs only the dev-server serving the SPA
// shell — no backend, no seeding — which makes it fast, deterministic,
// and safe to gate in the fast `web` CI job. See tests/mocked/_mockApi.ts.

import { defineConfig } from '@playwright/test';

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

export default defineConfig({
  testDir: './tests/mocked',
  timeout: 30_000,
  retries: process.env['CI'] ? 1 : 0,
  reporter: [['list']],
  use: {
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
        // BOSS_SCRATCH is irrelevant (every /api call is mocked), but
        // 0 avoids the dev-server trying to reach scratch services.
        env: { BOSS_SCRATCH: '0' },
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
