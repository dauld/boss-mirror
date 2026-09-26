// /jobs/<id> on a detail read that is not a Job (backlog c2e18fdd, seen
// by run 12db3d4a in the full mocked run, 2026-09-25). The inbox spec
// clicks through to /jobs/job-2 under the api floor, which answers the
// unrouted `/api/jobs/job-2` with `[]`; JobDetailPage cast that to Job,
// `!job` passed it, and the Subject section threw "Cannot read
// properties of undefined (reading 'id')" in subjectPath — logged in the
// console under a passing spec, and only when the page rendered before
// the spec's goBack, so the run that saw it could not say which spec.
//
// The fix is the read's: a body the page cannot render is a FAILED read,
// said on the page's failure line and naming what it lacked — never a
// render throw, and never an optional chain painting a blank subject
// over a record that never had one.

import { expect, test, type Page, type Route } from '@playwright/test';
import { FAILURE_MARKER } from './_routes';
import { installApiFloor } from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const DETAIL = /\/api\/jobs\/job-2$/;

const JOB = {
  id: 'job-2', kind: 'user-feedback', title: 'A packet the inbox links to', status: 'open',
  subject: { subject_kind: 'account', id: 'a1' },
  owner_id: 'emp-david', priority: 'standard', opened_on: '2026-09-26',
  due_on: null, closed_on: null, metadata: {}, tags: [], steps: [],
};

/// Open /jobs/job-2 under the api floor, answering the detail read with
/// `body` (or leaving it to the floor's `[]` when undefined), and
/// collect every uncaught page error.
async function openJob(page: Page, body?: unknown): Promise<string[]> {
  const errs: string[] = [];
  page.on('pageerror', (e) => errs.push(String(e)));
  await installApiFloor(page);
  if (body !== undefined) await page.route(DETAIL, (r) => json(r, body));
  await page.goto('/jobs/job-2');
  return errs;
}

test.describe('/jobs/<id> says a body that is not a Job failed, rather than throwing', () => {
  test('control: a whole Job renders its subject, with no failure line', async ({ page }) => {
    const errs = await openJob(page, JOB);
    await expect(page.locator('h1')).toContainText('A packet the inbox links to');
    await expect(page.getByRole('link', { name: 'a1' })).toHaveAttribute('href', '/ux/accounts/a1');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    expect(errs).toEqual([]);
  });

  test('the mocked floor’s [] is not a Job, and is said so', async ({ page }) => {
    const errs = await openJob(page);
    const failed = page.locator(FAILURE_MARKER);
    await expect(failed).toHaveText("Couldn't load job: /api/jobs/job-2: the answer is not a Job");
    await expect(failed).toHaveAttribute('role', 'alert');
    expect(errs).toEqual([]);
  });

  test('a Job with no subject names it on the failure line and throws nothing', async ({ page }) => {
    const { subject: _subject, ...noSubject } = JOB;
    const errs = await openJob(page, noSubject);
    await expect(page.locator(FAILURE_MARKER)).toHaveText(
      "Couldn't load job: /api/jobs/job-2: the Job carries no subject",
    );
    expect(errs).toEqual([]);
  });
});
