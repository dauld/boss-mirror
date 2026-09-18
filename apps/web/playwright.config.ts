// Playwright config for the Svelte SPA smoke suite.
//
// Auto-spawns `bun src/dev-server.ts` on port 5174 if nothing is
// already listening there. Reuse keeps local dev fast — if you
// already started the dev server in another terminal, the test
// run binds to it instead of double-starting. CI can set
// PWTEST_SKIP_DEVSERVER=1 to disable the spawn (e.g. when a
// container fixture brings the server up out-of-band).
//
// Scratch isolation: by default the smoke suite spawns the
// dev-server with BOSS_SCRATCH=1, so paired-service writes
// (people, messages, inventory, commerce, fleet, catalog,
// shipping) route to the +1000 scratch ports → boss_scratch DB
// instead of polluting the live boss DB. Set BOSS_SCRATCH=0 to
// run smoke against the prod stack (e.g. for diffing live data
// against a deploy). Note: the container runs no scratch stack,
// so /api/jobs writes land in boss.

import { defineConfig } from '@playwright/test';

const skipDevServer = process.env['PWTEST_SKIP_DEVSERVER'] === '1';
const scratchMode = process.env['BOSS_SCRATCH'] ?? '1';

export default defineConfig({
  testDir: './tests',
  // Seeds brewery reference data (accounts, vendors, employees,
  // messages) into the scratch stack on suite start. Skipped
  // when BOSS_SCRATCH=0 (prod runs already have the data) or
  // PWTEST_SKIP_SEED=1 (fast iteration on non-data specs).
  globalSetup: './tests/globalSetup.ts',
  timeout: 20_000,
  retries: 0,
  reporter: [['list']],
  use: {
    baseURL: 'http://127.0.0.1:5174',
    headless: true,
    viewport: { width: 1280, height: 720 },
  },
  webServer: skipDevServer
    ? undefined
    : {
        command: 'bun src/dev-server.ts',
        url: 'http://127.0.0.1:5174/',
        reuseExistingServer: true,
        timeout: 30_000,
        stdout: 'pipe',
        stderr: 'pipe',
        env: { BOSS_SCRATCH: scratchMode },
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
