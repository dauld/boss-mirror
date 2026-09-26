// /it/operate — the IT incidents surface.
//
// David: "Do we have a good surface for IT to view post mortems more
// durably? I think we probably need a new 'Incidents' page that is
// both where we respond to active incidents and document post mortems
// for posterity." Two panels: active incident packets
// (respond), and closed ones rendered as readable documents (the
// archive). The renderer is SEMI-structured — packets have carried
// different metadata shapes, and every one must render
// without dropping content or dumping JSON.
//
// The failed-fetch case is pinned per the false-empty sweep (packet
// 3fba9c35): an outage must render as a FAILURE, never as "no
// incidents" — a page that reports calm during an outage is the worst
// possible incident surface.

import { test, expect, type Page, type Route } from '@playwright/test';
import { installSmokeMocks } from './_smokeMocks';

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

// ---- fixtures ---------------------------------------------------------

/// An open packet in the 2026-08-22 shape: incident_at / summary /
/// mitigations_shipped / open_questions / evidence, mid-workflow with
/// one ready step assigned to an agent.
const OPEN_JOB = {
  id: 'ipm-open-1',
  kind: 'incident',
  workflow_version: 1,
  subject: { subject_kind: 'custom', id: 'incident-2026-08-22-etcd' },
  title: 'Post-mortem: cp-2 etcd degradation',
  owner_id: 'emp-david',
  status: 'open',
  priority: 'standard',
  opened_on: '2026-08-22',
  due_on: null,
  closed_on: null,
  tags: [],
  metadata: {
    incident_at: '2026-08-22, two windows: 18:10-18:35Z and 19:17-19:20Z',
    summary: 'Six queued gates killed while etcd degraded on cp-2.',
    mitigations_shipped: 'gate-run protocol v1 ACTIVE; runner throttles baked in.',
    open_questions: '(a) dedicated etcd disk; (b) move gates off the control plane.',
    evidence: 'readyz verbose (etcd failed) captured 18:1xZ.',
  },
  steps: [
    { id: 's-open-0', job_id: 'ipm-open-1', kind: 'trigger', title: 'Incident raised', assignee_id: null, status: 'completed', sort_order: 0, blocked_by: [], completed_on: '2026-08-22', metadata: {} },
    { id: 's-open-1', job_id: 'ipm-open-1', kind: 'task', title: 'Establish the timeline from evidence', assignee_id: 'claude@algedonic.dev', status: 'ready', sort_order: 1, blocked_by: ['s-open-0'], completed_on: null, metadata: { authority_role: 'platform-admin' } },
    { id: 's-open-2', job_id: 'ipm-open-1', kind: 'task', title: 'Did we cause it?', assignee_id: null, status: 'pending', sort_order: 2, blocked_by: ['s-open-1'], completed_on: null, metadata: { authority_role: 'platform-admin' } },
  ],
};

/// A closed packet in the NEW shape — the newest archive entry.
const CLOSED_NEW = {
  ...OPEN_JOB,
  id: 'ipm-closed-new',
  title: 'Post-mortem: SoR outage during gate chain',
  status: 'closed',
  opened_on: '2026-08-20',
  closed_on: '2026-08-22',
  steps: [
    { id: 's-cn-0', job_id: 'ipm-closed-new', kind: 'trigger', title: 'Incident raised', assignee_id: null, status: 'completed', sort_order: 0, blocked_by: [], completed_on: '2026-08-20', metadata: {} },
    { id: 's-cn-7', job_id: 'ipm-closed-new', kind: 'outcome', title: 'Closed', assignee_id: null, status: 'completed', sort_order: 7, blocked_by: [], completed_on: '2026-08-22', metadata: {} },
  ],
};

/// A closed packet in the OLDER shape: ask / declared_by /
/// incident_date / outcome — no key the newest shape has. The archive
/// must render its content as labeled prose, not drop it.
const CLOSED_OLD = {
  id: 'ipm-closed-old',
  kind: 'incident',
  workflow_version: 1,
  subject: { subject_kind: 'custom', id: 'incident-2026-08-13-sor' },
  title: 'Post-mortem: production DB crash',
  owner_id: 'emp-david',
  status: 'closed',
  priority: 'standard',
  opened_on: '2026-08-13',
  due_on: null,
  closed_on: '2026-08-14',
  tags: [],
  metadata: {
    ask: 'Write the post mortem and file protocol changes as packets.',
    declared_by: 'emp-david',
    incident_date: '2026-08-13',
    outcome: 'Five protocol changes filed',
  },
  steps: [
    { id: 's-co-0', job_id: 'ipm-closed-old', kind: 'trigger', title: 'Incident raised', assignee_id: null, status: 'completed', sort_order: 0, blocked_by: [], completed_on: '2026-08-13', metadata: {} },
    { id: 's-co-7', job_id: 'ipm-closed-old', kind: 'outcome', title: 'Closed', assignee_id: null, status: 'completed', sort_order: 7, blocked_by: [], completed_on: '2026-08-14', metadata: {} },
  ],
};

const LIST = /\/api\/jobs\?kind=incident&/;

async function mocks(page: Page) {
  await installSmokeMocks(page);
}

// ---- specs ------------------------------------------------------------

test('active packets and the archive both render, archive newest first', async ({ page }) => {
  await mocks(page);
  await page.route(LIST, (r) =>
    json(r, { data: [OPEN_JOB, CLOSED_OLD, CLOSED_NEW], total: 3 }));

  await page.goto('/it/operate');

  // --- Active panel: the open packet, with its step strip. ---
  const active = page.locator('.inc-active');
  await expect(active.getByText('Post-mortem: cp-2 etcd degradation')).toBeVisible();
  await expect(active.getByText(/18:10-18:35Z/)).toBeVisible();
  // The compact step-state strip: one segment per step.
  await expect(active.locator('.inc-strip-step')).toHaveCount(3);
  // The current step and who holds it.
  await expect(active.getByText('Establish the timeline from evidence')).toBeVisible();
  await expect(active.getByText(/claude@algedonic\.dev/)).toBeVisible();
  // The link to the packet itself.
  await expect(active.locator('a[href*="/jobs/ipm-open-1"]')).toBeVisible();
  // Closed packets are not "active".
  await expect(active.getByText('Post-mortem: production DB crash')).toHaveCount(0);

  // --- Archive: closed packets as documents, newest first. ---
  const docs = page.locator('.inc-archive .inc-doc');
  await expect(docs).toHaveCount(2);
  await expect(docs.nth(0)).toContainText('Post-mortem: SoR outage during gate chain');
  await expect(docs.nth(1)).toContainText('Post-mortem: production DB crash');

  // New shape: known keys as first-class sections, in reading order.
  const newest = docs.nth(0);
  await expect(newest.getByText('Summary', { exact: true })).toBeVisible();
  await expect(newest.getByText(/Six queued gates killed/)).toBeVisible();
  await expect(newest.getByText('Evidence', { exact: true })).toBeVisible();
  // Its outcome — the terminal that fired. Scoped to the badge: the
  // terminal is titled `Closed`, and "closed <date>" sits beside it.
  await expect(newest.locator('.inc-outcome')).toHaveText('Closed');

  // Old shape: unknown keys as labeled prose — content survives.
  const oldest = docs.nth(1);
  await expect(oldest.getByText('Ask', { exact: true })).toBeVisible();
  await expect(
    oldest.getByText('Write the post mortem and file protocol changes as packets.'),
  ).toBeVisible();
  await expect(oldest.getByText('Declared by', { exact: true })).toBeVisible();
});

test('a failed fetch renders as a failure with Retry — never as an empty page', async ({ page }) => {
  await mocks(page);
  let up = false;
  await page.route(LIST, (r) =>
    up ? json(r, { data: [OPEN_JOB], total: 1 }) : json(r, 'jobs api down', 500));

  await page.goto('/it/operate');

  await expect(page.locator('.inc-failed')).toBeVisible();
  await expect(page.locator('.inc-failed')).toContainText('HTTP 500');
  // The false-empty this exists to prevent: an outage must not read
  // as "no incidents".
  await expect(page.getByText(/No active incidents/)).toHaveCount(0);
  await expect(page.getByText(/No post-mortems/)).toHaveCount(0);
  await expect(page.locator('.inc-doc')).toHaveCount(0);

  // The outage clears; Retry recovers without a reload.
  up = true;
  await page.getByRole('button', { name: 'Retry' }).click();
  await expect(page.getByText('Post-mortem: cp-2 etcd degradation')).toBeVisible();
  await expect(page.locator('.inc-failed')).toHaveCount(0);
});

/// An open packet in the LIVE shape (a65ba21e, read 2026-09-26): no
/// started_at on the job or the raised step, metadata.opened_at and
/// metadata.severity present, the current step held by a ROLE.
const LIVE_SHAPE = {
  id: 'inc-live-1',
  kind: 'incident',
  workflow_version: 3,
  subject: { subject_kind: 'custom', id: 'incident-2026-09-26' },
  title: 'Incident: gate bay stalled',
  owner_id: 'emp-david',
  status: 'open',
  priority: 'urgent',
  opened_on: '2026-09-26',
  due_on: null,
  closed_on: null,
  tags: [],
  metadata: {
    opened_at: '2026-09-26T02:32:07Z',
    severity: 'degradation (no service outage)',
  },
  steps: [
    { id: 's-live-0', job_id: 'inc-live-1', kind: 'trigger', title: 'Incident raised', assignee_id: null, status: 'completed', sort_order: 0, blocked_by: [], completed_on: '2026-09-26', metadata: { symptom: 'gates stuck' } },
    { id: 's-live-1', job_id: 'inc-live-1', kind: 'task', title: 'Establish the timeline from evidence', assignee_id: null, status: 'ready', sort_order: 2, blocked_by: ['s-live-0'], completed_on: null, metadata: { authority_role: 'platform-admin' } },
  ],
};

const QUEUE_AGE = /\/api\/jobs\/queue-age$/;

test('the active card shows severity, when it started, time open, time at step and the real audience', async ({ page }) => {
  await mocks(page);
  await page.route(LIST, (r) => json(r, { data: [LIVE_SHAPE], total: 1 }));
  await page.route(QUEUE_AGE, (r) =>
    json(r, {
      data: [{ job_id: 'inc-live-1', step_id: 's-live-1', since: '2026-09-26T04:00:00Z', exact: true }],
      total: 1,
      now: '2026-09-26T05:10:00Z',
    }));

  await page.goto('/it/operate');

  const card = page.locator('.inc-active .inc-card');
  await expect(card).toHaveCount(1);
  // Severity beside priority (1a242883 b).
  await expect(card.locator('.inc-priority')).toHaveText('urgent');
  await expect(card.locator('.inc-severity')).toHaveText('degradation (no service outage)');
  // When: no started_at anywhere on the live shape, so metadata.opened_at (a).
  await expect(card.locator('.inc-when')).toHaveText('2026-09-26T02:32:07Z');
  // Time open, against the server clock the lens sent (c).
  await expect(card.locator('.inc-age')).toHaveText('2h 37m');
  // Time at the current step, from became_ready_at (c).
  await expect(card.locator('.inc-at-step')).toHaveText('at step 1h 10m');
  // The role that holds it, not "(unassigned)" (d).
  await expect(card.locator('.inc-holder')).toHaveText('(role platform-admin)');
  await expect(card.getByText('(unassigned)')).toHaveCount(0);
});

test('an unreadable queue-age says so on the card and does not fail the queue', async ({ page }) => {
  await mocks(page);
  await page.route(LIST, (r) => json(r, { data: [LIVE_SHAPE], total: 1 }));
  await page.route(QUEUE_AGE, (r) => json(r, 'down', 500));

  await page.goto('/it/operate');

  const card = page.locator('.inc-active .inc-card');
  await expect(card.locator('.inc-at-step')).toHaveText('time at step unreadable');
  await expect(card.locator('.inc-severity')).toBeVisible();
  await expect(page.locator('.inc-failed')).toHaveCount(0);
});

test('a truly empty queue reads as empty — each panel says so distinctly', async ({ page }) => {
  await mocks(page);
  await page.route(LIST, (r) => json(r, { data: [], total: 0 }));

  await page.goto('/it/operate');

  await expect(page.getByText(/No active incidents/)).toBeVisible();
  await expect(page.getByText(/No post-mortems archived yet/)).toBeVisible();
  await expect(page.locator('.inc-failed')).toHaveCount(0);
});
