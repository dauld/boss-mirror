// Live-instance Playwright config — the nightly playground crawl.
//
// tests/live/ runs against a REAL BOSS instance named by
// BOSS_E2E_BASE_URL: no dev-server, no mocks, no seeding. The mocked
// suite (playwright.mocked.config.ts) proves every surface renders
// under an adversarial fake backend; this one proves the same surfaces
// render the playground's real data as a guest sees it. It is the
// whole of what survived the 35-spec live suite deleted on 2026-09-18
// (design 0e07ce64): that suite ran under nothing, and a check nobody
// runs is a check that is not running.
//
// Run by infra/cluster/manifests/boss-playground-crawl.yaml nightly
// (`bun run test:live`, the `maintenance-playground-crawl` chore) and
// by hand from any box that can reach an instance:
//
//   BOSS_E2E_BASE_URL=http://boss-gateway.boss-playground.svc.cluster.local bun run test:live
//
// The variable is REQUIRED. A default here would point the crawl at
// something — a stale port, another tree's dev-server — and report on
// it as if it were the playground (the eaca07e1 false green, one
// config over). Missing, the config refuses with the name.

import { defineConfig } from '@playwright/test';

const baseURL = process.env['BOSS_E2E_BASE_URL'];
if (!baseURL) {
  throw new Error(
    'playwright.live.config.ts: BOSS_E2E_BASE_URL is not set — the live crawl needs the ' +
      'instance to crawl named explicitly (e.g. http://boss-gateway.boss-playground.svc.cluster.local)',
  );
}

export default defineConfig({
  testDir: './tests/live',
  // One test crawls every route; the budget is per test, so it is the
  // whole crawl's. ~60 routes at up to 20 s each plus settle time.
  timeout: 30 * 60_000,
  retries: 0,
  reporter: [['list']],
  use: {
    baseURL,
    headless: true,
    viewport: { width: 1280, height: 800 },
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
