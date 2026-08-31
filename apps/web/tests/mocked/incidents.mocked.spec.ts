// /it/operate — the IT incidents surface.
//
// David: "Do we have a good surface for IT to view post mortems more
// durably? I think we probably need a new 'Incidents' page that is
// both where we respond to active incidents and document post mortems
// for posterity." Two panels: active incident-post-mortem packets
// (respond), and closed ones rendered as readable documents (the
// archive). The renderer is SEMI-structured — the two live packets
// already carry different metadata shapes, and both must render
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
  kind: 'incident-post-mortem',
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
    { id: 's-open-0', job_id: 'ipm-open-1', kind: 'trigger', title: 'Incident opened', assignee_id: null, status: 'completed', sort_order: 0, blocked_by: [], completed_on: '2026-08-22', metadata: {} },
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
    { id: 's-cn-0', job_id: 'ipm-closed-new', kind: 'trigger', title: 'Incident opened', assignee_id: null, status: 'completed', sort_order: 0, blocked_by: [], completed_on: '2026-08-20', metadata: {} },
    { id: 's-cn-7', job_id: 'ipm-closed-new', kind: 'outcome', title: 'Post-mortem closed', assignee_id: null, status: 'completed', sort_order: 7, blocked_by: [], completed_on: '2026-08-22', metadata: {} },
  ],
};

/// A closed packet in the OLDER shape: ask / declared_by /
/// incident_date / outcome — no key the newest shape has. The archive
/// must render its content as labeled prose, not drop it.
const CLOSED_OLD = {
  id: 'ipm-closed-old',
  kind: 'incident-post-mortem',
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
    { id: 's-co-0', job_id: 'ipm-closed-old', kind: 'trigger', title: 'Incident opened', assignee_id: null, status: 'completed', sort_order: 0, blocked_by: [], completed_on: '2026-08-13', metadata: {} },
    { id: 's-co-7', job_id: 'ipm-closed-old', kind: 'outcome', title: 'Post-mortem closed', assignee_id: null, status: 'completed', sort_order: 7, blocked_by: [], completed_on: '2026-08-14', metadata: {} },
  ],
};

const LIST = /\/api\/jobs\?kind=incident-post-mortem/;

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
  // Its outcome — the terminal that fired.
  await expect(newest.getByText('Post-mortem closed')).toBeVisible();

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

test('a truly empty queue reads as empty — each panel says so distinctly', async ({ page }) => {
  await mocks(page);
  await page.route(LIST, (r) => json(r, { data: [], total: 0 }));

  await page.goto('/it/operate');

  await expect(page.getByText(/No active incidents/)).toBeVisible();
  await expect(page.getByText(/No post-mortems archived yet/)).toBeVisible();
  await expect(page.locator('.inc-failed')).toHaveCount(0);
});
