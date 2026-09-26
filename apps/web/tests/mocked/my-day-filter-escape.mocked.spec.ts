// The protocol filter must never outlive its own escape hatch.
//
// Repro from the 2026-08-20 audit: filter My Day to a protocol, drain
// its last row (claiming it does), and the chip row used to unmount
// with the filter still applied — every queue empty, no control left
// to clear it, reload the only way out. The chips (with the drained
// hint) must stand whenever a filter is set.

import { expect, test, type Page, type Route } from '@playwright/test';
import { servePeopleRows } from './_smokeMocks';

const EMP = { id: 'emp-david', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

// Unassigned + human-completion → "Up for grabs", where Claim lives.
const grab = (n: string, workflow: string) => ({
  job_id: `j-${n}`, job_title: `Job ${n}`, due_on: null, workflow,
  subject_kind: 'custom', subject_id: 's-1', priority: 'standard',
  step: { id: `s-${n}`, job_id: `j-${n}`, kind: 'task', title: `step ${n}`,
    status: 'ready', assignee_id: null, completion: 'human' },
});

async function mocks(page: Page, drained: { yes: boolean }) {
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  // Catch-all FIRST — routes match last-registered-first.
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await servePeopleRows(page, [EMP]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: 'emp-david', role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/assignments/, (r) =>
    json(r, {
      data: drained.yes
        ? [grab('b', 'ship-a-change')]
        : [grab('a', 'approval'), grab('b', 'ship-a-change')],
    }));
  await page.route(/\/claim$/, (r) => {
    drained.yes = true;
    return json(r, {});
  });
}

test('draining the filtered protocol leaves the way out standing', async ({ page }) => {
  const drained = { yes: false };
  await mocks(page, drained);
  await page.goto('/');

  // Filter to approval: only its packet stays on the board.
  await page.getByRole('button', { name: 'approval (1)' }).click();
  await expect(page.getByText('Job a')).toBeVisible();
  await expect(page.getByText('Job b')).toHaveCount(0);

  // Claim the last approval row; the refetch comes back without it,
  // leaving one protocol — the count the old gate unmounted the chips
  // at, stranding the filter.
  await page.getByRole('button', { name: 'Claim' }).click();

  // The escape stands: the hint names the drained protocol and All is
  // still clickable.
  await expect(page.getByText(/Nothing left under/)).toBeVisible();
  await page.getByRole('button', { name: /^All \(/ }).click();
  await expect(page.getByText('Job b')).toBeVisible();
});
