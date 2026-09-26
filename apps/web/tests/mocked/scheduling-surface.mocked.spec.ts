// Backlog 2f14cdd8 — the Schedule button enabled on scheduled_at
// alone, so a step pressed without a duration or an assignee went
// active and the calendar hook (crates/core/boss-jobs/src/calendar_hook.rs)
// answered NoOp: no reservation, and nothing on screen to say so.
// Decided 2026-09-24: Schedule needs all three — scheduled_at, a
// POSITIVE duration_minutes, an assignee — and the surface names what
// is missing beside the button. Required-at-done validation is
// untouched; this pins only the button and its line.

import { expect, test, type Page, type Route } from '@playwright/test';
import { servePeopleRows } from './_smokeMocks';

const JOB_ID = 'job-sched-1';

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };
const EMP2 = { ...EMP, id: 'emp-002', name: 'Robin', role: 'brewer' };

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

async function mountScheduling(
  page: Page,
  metadata: Record<string, unknown>,
  assigneeId: string | null = null,
) {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  const step = {
    id: 's1', job_id: JOB_ID, title: 'plan the brew', kind: 'scheduling', status: 'ready',
    assignee_id: assigneeId, sort_order: 0, blocked_by: [], sign_offs_required: [],
    sign_offs: [], metadata, notes: null,
  };
  const job = {
    id: JOB_ID, kind: 'morning-brew', title: 'Scheduling fixture', status: 'open',
    opened_on: '2026-09-24', due_on: null, closed_on: null, owner_id: EMP.id,
    priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'fixture' }, metadata: {},
  };
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP, EMP2]));
  await servePeopleRows(page, [EMP, EMP2]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'scheduling', label: 'Scheduling', category: 'generic', ux: 'inline',
      description: '', surface: 'scheduling' },
  ]));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, { ...job, steps: [step] }));
  const putBodies: Record<string, unknown>[] = [];
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1$`), (r) => {
    const body = JSON.parse(r.request().postData() ?? '{}') as Record<string, unknown>;
    putBodies.push(body);
    return json(r, { ...step, ...body });
  });
  // The step merge door: the surface's metadata goes here, before the
  // PUT, and the PUT carries none (backlog e39a9d2a).
  const mergeBodies: Record<string, unknown>[] = [];
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1/metadata$`), (r) => {
    mergeBodies.push(JSON.parse(r.request().postData() ?? '{}') as Record<string, unknown>);
    return json(r, step);
  });
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.step-scheduling');
  await expect(surface).toBeVisible();
  // The roster populates the assignee select; wait for it so the
  // selectOption below is not racing the fetch.
  await expect(surface.locator('select option', { hasText: 'Robin' })).toHaveCount(1);
  return { surface, putBodies, mergeBodies };
}

test('Schedule stays disabled until when, a positive duration and an assignee are all set, and names what is missing', async ({ page }) => {
  const { surface, putBodies, mergeBodies } = await mountScheduling(page, {});
  const schedule = surface.getByRole('button', { name: 'Schedule' });
  const missing = surface.locator('.step-schedule-missing');

  // Nothing set: all three are named.
  await expect(schedule).toBeDisabled();
  await expect(missing).toHaveText('Missing: date/time, duration, assignee');

  // scheduled_at alone — the shape that went active and reserved
  // nothing before this fix.
  await surface.locator('input[type="datetime-local"]').fill('2026-09-25T09:00');
  await expect(schedule).toBeDisabled();
  await expect(missing).toHaveText('Missing: duration, assignee');

  // A zero duration is not a duration: the hook refuses non-positive.
  await surface.locator('input[type="number"]').fill('0');
  await expect(schedule).toBeDisabled();
  await expect(missing).toHaveText('Missing: duration, assignee');

  await surface.locator('input[type="number"]').fill('90');
  await expect(schedule).toBeDisabled();
  await expect(missing).toHaveText('Missing: assignee');

  await surface.locator('select').selectOption(EMP2.id);
  await expect(schedule).toBeEnabled();
  await expect(missing).toHaveCount(0);

  await schedule.click();
  await expect.poll(() => putBodies.length).toBe(1);
  const body = putBodies[0];
  expect(body['status']).toBe('active');
  expect(body['assignee_id']).toBe(EMP2.id);
  expect(body['metadata']).toBeUndefined();
  expect(mergeBodies.length).toBe(1);
  const meta = mergeBodies[0];
  expect(meta['scheduled_at']).toBe('2026-09-25T09:00');
  expect(meta['duration_minutes']).toBe(90);
  // The emptied location is an explicit null the door deletes — not a
  // key left out of a wholesale PUT.
  expect(meta['location']).toBeNull();
});

test('a step that already carries all three opens with Schedule enabled and no missing line', async ({ page }) => {
  const { surface } = await mountScheduling(page, {
    scheduled_at: '2026-09-25T09:00:00Z', duration_minutes: 60,
  }, EMP.id);
  await expect(surface.getByRole('button', { name: 'Schedule' })).toBeEnabled();
  await expect(surface.locator('.step-schedule-missing')).toHaveCount(0);
});
