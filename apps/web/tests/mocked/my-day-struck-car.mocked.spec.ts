// A builder's own struck car must look struck on their My Day.
//
// Repro from d6e53a35 (2026-09-14): the assignments lens built its card
// through the yard's one constructor, but the server row carried no job
// metadata, so a car one red train had released stood in the builder's
// personal queue exactly like a clean one while the yard drew it struck.
// The builder is the one who can act on a strike before the NEXT red
// holds the car out; today they learned it from the dock, if they looked.
// The row now carries `red_trains`, and the card says so in the yard's
// own words.

import { expect, test, type Page, type Route } from '@playwright/test';
import { servePeopleRows } from './_smokeMocks';

const EMP = { id: 'emp-david', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

// Assigned to me + human-completion → the personal queue.
const mine = (n: string, extra: Record<string, unknown>) => ({
  job_id: `j-${n}`, job_title: `Car ${n}`, due_on: null, workflow: 'ship-a-change',
  subject_kind: 'custom', subject_id: 's-1', priority: 'standard',
  ...extra,
  step: { id: `s-${n}`, job_id: `j-${n}`, kind: 'task', title: `review ${n}`,
    status: 'ready', assignee_id: 'emp-david', completion: 'human' },
});

async function mocks(page: Page) {
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
      data: [
        mine('struck', { red_trains: 1 }),
        mine('twice', { red_trains: 2 }),
        // A clean car, and a row from a server that predates the field.
        mine('clean', { red_trains: 0 }),
        mine('older', {}),
      ],
    }));
}

test('a struck car in the personal queue wears the yard\'s strike sentence', async ({ page }) => {
  await mocks(page);
  await page.goto('/');

  await expect(page.getByText('Car struck')).toBeVisible();
  await expect(page.getByText('1 red train behind it')).toHaveCount(1);
  await expect(page.getByText('2 red trains behind it')).toHaveCount(1);
  // Two of the four cards carry a strike; the clean ones say nothing.
  await expect(page.getByText(/red trains? behind it/)).toHaveCount(2);
  await expect(page.getByText('Car clean')).toBeVisible();
  await expect(page.getByText('Car older')).toBeVisible();
});
