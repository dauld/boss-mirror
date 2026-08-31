// My Watchlist as cards standing at stops (David, 2026-08-15).
//
// The placement depends on the queue envelope carrying steps, which
// this station's lens asks for via `with_steps`. Both halves are
// pinned here: the track when steps arrive, and the flat list when
// they do not — a registry that predates 139 must degrade, not blank.

import { expect, test, type Page, type Route } from '@playwright/test';

const EMP = { id: 'emp-david', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

const J = (id: string, about: string, status: string, meta: Record<string, unknown> = {}) =>
  ({ id, kind: 'user-feedback', title: `Feedback on ${about}`, status,
     opened_on: '2026-08-14', closed_on: status === 'open' ? null : '2026-08-15',
     tags: [], metadata: { submitted_by: 'emp-david', ...meta }, simulated: false });

const DATA = [
  J('w1', '/it', 'open'),
  J('w2', '/ux/jobs', 'open'),
  J('w3', '/shop', 'open'),
  J('w4', '/it/design', 'closed', { outcome: 'completed' }),
  J('w5', '/', 'closed', { outcome: 'declined' }),
];
const STEPS: Record<string, unknown[]> = {
  w1: [{ spec_slug: 'triage', status: 'ready' }],
  w2: [{ spec_slug: 'triage', status: 'completed' }, { spec_slug: 'design-review', status: 'active' }],
  w3: [{ spec_slug: 'triage', status: 'completed' }, { spec_slug: 'build', status: 'ready' }],
  w4: [{ spec_slug: 'closed', status: 'completed' }],
  w5: [{ spec_slug: 'declined', status: 'completed' }],
};

async function mocks(page: Page) {
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: 'emp-david', role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/live$/, (r) => json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/assignments/, (r) => json(r, { mine: [], group: [], data: [] }));
  await page.route(/\/api\/stations\/my-watchlist\/queue/, (r) => json(r, {
    station: 'my-watchlist', kind: 'actor', discipline: ['recency'], over_limit: false,
    terminal_window_days: 14, total: DATA.length, data: DATA, steps: STEPS,
    lens: { title: 'My watchlist', subtitle: 'What you sent, and where it got to', with_steps: true },
  }));
}

test('packets stand at the stop their steps say they reached', async ({ page }) => {
  await mocks(page);
  await page.goto('/');

  const track = page.locator('.watch-track');
  await expect(track).toBeVisible();

  const stop = (label: string) =>
    track.locator('.watch-stop').filter({ has: page.getByText(label, { exact: true }) });
  await expect(stop('Being read')).toContainText('/it');
  await expect(stop('Being worked out')).toContainText('/ux/jobs');
  await expect(stop('Being built')).toContainText('/shop');
  await expect(stop('Done')).toContainText('/it/design');
  // Nothing has arrived un-triaged, and the stop says so rather than
  // borrowing a packet from further along.
  await expect(stop('Received')).toContainText('—');

  // Turned down is an answer the filer is owed: off the track, still
  // on the board.
  const off = page.locator('.watch-offtrack');
  await expect(off).toContainText('Feedback on /');
  await expect(track).not.toContainText('Read, not taken up');
});

test('an envelope without steps falls back to the list rather than blanking', async ({ page }) => {
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await mocks(page);
  // A deployment whose registry predates 139: no `steps` key at all.
  await page.route(/\/api\/stations\/my-watchlist\/queue/, (r) => json(r, {
    station: 'my-watchlist', kind: 'actor', discipline: ['recency'], over_limit: false,
    terminal_window_days: 14, total: DATA.length, data: DATA,
  }));
  await page.goto('/');

  await expect(page.locator('.watch-track')).toHaveCount(0);
  // Same packets, just not placed.
  await expect(page.getByText('Feedback on /ux/jobs')).toBeVisible();
});
