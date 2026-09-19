// The floor under every mocked spec's backend (backlog f88e7908).
//
// The mocked suite runs no backend: what a spec's page.route does not
// answer reaches the dev-server, which in mocked mode answers 404
// {"mock":"unanswered"} and names the miss in its shutdown summary.
// Measured 2026-09-19 before this floor existed: 403 such misses per
// full run, 16 distinct paths, all from five specs that mocked only
// what they were testing and let the SHELL's own reads (identity, the
// workflow registry, step types, classes, the live-jobs poll, the
// sim-clock stream, the surface-open record, the unread badge) fall
// through. Eight of those paths never showed in the old noise because
// the proxy had no port for them and they 502'd silently.
//
// `installApiFloor` answers every one of them with the empty-but-valid
// shape of ITS endpoint, so a spec that installs it first can mock only
// what it tests and the summary reads zero. This spec pins that claim
// where the suite can read it: mount a surface with nothing but the
// floor installed, and (a) the surface paints — a list where an object
// is due is a malformed read, not an empty one, and the shell would not
// mount — and (b) no /api/** response the page saw carries the
// dev-server's unanswered marker.

import { test, expect, type Page } from '@playwright/test';
import { mountPage } from './_helpers';
import { installApiFloor } from './_smokeMocks';

/// Three code paths for the chrome: an AppShell route, an IT surface,
/// and /login, which renders outside the shell.
const SURFACES: ReadonlyArray<{ path: string; root?: string }> = [
  { path: '/ux/jobs' },
  { path: '/it' },
  { path: '/login', root: '.login-card' },
];

/// Every /api/** response the page received that the dev-server, not a
/// page.route, answered — its body is the one marker the server writes.
async function unanswered(page: Page, run: () => Promise<void>): Promise<string[]> {
  const seen: string[] = [];
  const pending: Promise<void>[] = [];
  page.on('response', (res) => {
    if (!res.url().includes('/api/') || res.status() !== 404) return;
    pending.push(
      res
        .json()
        .then((body: { mock?: string }) => {
          if (body.mock === 'unanswered') {
            seen.push(`${res.request().method()} ${new URL(res.url()).pathname}`);
          }
        })
        .catch(() => {
          // Not JSON — a 404 some route fulfilled with text, not the server's.
        }),
    );
  });
  await run();
  await Promise.all(pending);
  return seen;
}

test.describe('the api floor', () => {
  for (const { path, root } of SURFACES) {
    test(`answers everything ${path} asks, and the surface paints`, async ({ page }) => {
      await installApiFloor(page);
      const misses = await unanswered(page, () => mountPage(page, path, { root }));
      expect(misses, `reached the dev-server under the floor on ${path}`).toEqual([]);
    });
  }
});
