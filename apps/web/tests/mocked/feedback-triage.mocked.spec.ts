// The triage board.
//
// What these pin is that the board is a rendering of the Workflow, not
// a screen with opinions. Columns come from the registry's fork
// vocabulary — add a disposition to the spec and a column appears —
// and routing an item is completing the fork step with that
// disposition, which is what opens the next step. A card therefore
// cannot disagree with the Job behind it.
//
// The earlier version of this board had three hardcoded columns
// ending in "Triaged", which made triage a synonym for closing. These
// tests exist partly to stop that coming back.

import { test, expect } from '@playwright/test';
import { mountPage } from '../smoke/_helpers';

const MANIFEST = { display_name: 'Algedonic Ales', modules: {}, labels: {} };

const DISPOSITIONS = 'reproduce|design|build|duplicate|needs-info|decline';

/// Mirrors `user_feedback_spec()`. The board reads the fork out of
/// this, so the shape here is the contract under test: a required
/// pipe-shaped field marks the fork, and each successor's
/// `title_template` names the route.
const KIND = {
  kind: 'user-feedback',
  version: 1,
  status: 'active',
  steps: [
    {
      title: 'submitted',
      kind: 'trigger',
      ready_when: 'true',
      title_template: 'Feedback submitted',
      fields: [],
    },
    {
      title: 'triage',
      kind: 'task',
      ready_when: 'steps.submitted.done',
      title_template: 'Triage feedback',
      fields: [
        { name: 'disposition', field_type: DISPOSITIONS, required: true },
        { name: 'finding', field_type: 'string', required: false },
      ],
    },
    ...[
      ['investigate', 'reproduce', 'Reproduce and investigate'],
      ['design-review', 'design', 'Decide the design'],
      ['build', 'build', 'Build the change'],
      ['needs-info', 'needs-info', 'Waiting on the reporter'],
      ['duplicate', 'duplicate', 'Closed as a duplicate'],
      ['declined', 'decline', 'Closed without action'],
    ].map(([title, disposition, label]) => ({
      title,
      kind: 'task',
      ready_when: `steps.triage.done AND steps.triage.metadata.disposition = "${disposition}"`,
      title_template: label,
      fields: [],
    })),
  ],
};

function job(
  id: string,
  message: string,
  triage: { status: string; metadata?: Record<string, unknown>; kind?: string },
  jobStatus?: string,
) {
  return {
    id,
    kind: 'user-feedback',
    title: 'Feedback on /ux/jobs',
    // A routed packet is OPEN at its branch step — triage completing
    // opens the next step, it does not close the Job. The old fixture
    // conflated the two, which is exactly the confusion the live
    // board reproduced (198 cards, 16 open, closed packets sitting in
    // route columns — David, 2026-08-19). Closed is now an explicit
    // fixture choice, never an inference from the fork step.
    status: jobStatus ?? 'open',
    subject: { subject_kind: 'custom', id: '/ux/jobs' },
    owner_id: 'emp-bootstrap-admin',
    metadata: { message, route: '/ux/jobs' },
    steps: [
      { id: `${id}-t`, kind: 'trigger', status: 'completed', fields: [], metadata: {} },
      {
        id: `${id}-a`,
        // `authority_role` keeps the step waiting for a person, and
        // the `disposition` field is how the board identifies the
        // fork. A real Job carries both; omitting either here would
        // make the fixture lie about what the board keys on.
        kind: triage.kind ?? 'task',
        status: triage.status,
        fields: [
        { name: 'disposition', field_type: DISPOSITIONS, required: true },
        { name: 'finding', field_type: 'string', required: false },
      ],
        metadata: { authority_role: 'platform-admin', ...(triage.metadata ?? {}) },
      },
      { id: `${id}-o`, kind: 'outcome', status: 'pending', fields: [], metadata: {} },
    ],
  };
}

const JOBS = [
  job('fb-waiting', 'Column picker forgets my choice', { status: 'ready' }),
  job('fb-agent', 'Typo on the vendors page', {
    status: 'ready',
    metadata: { agent_requested_at: '2026-08-06T10:00:00Z', agent_requested_by: 'emp-1' },
  }),
  job('fb-routed', 'Needs a design call', {
    status: 'completed',
    metadata: { disposition: 'design' },
  }),
  // Routed AND finished: must render in Closed, never under its old
  // route — a closed packet is not queue contents whatever its fork
  // step says.
  job(
    'fb-done',
    'Fixed last week',
    { status: 'completed', metadata: { disposition: 'design' } },
    'closed',
  ),
];

test.describe('feedback triage board', () => {
  test.beforeEach(async ({ page }) => {
    await page.route(/\/api\/tenant\/manifest$/, (r) => r.fulfill({ json: MANIFEST }));
    await page.route(/\/api\/workflows$/, (r) => r.fulfill({ json: [KIND] }));
    await page.route(/\/api\/jobs\?kind=user-feedback/, (r) =>
      r.fulfill({ json: { data: JOBS, total: JOBS.length } }),
    );
  });

  test('builds its columns from the Workflow fork, labelled by each next step', async ({
    page,
  }) => {
    await mountPage(page, '/it/design/feedback', { titleMatch: /feedback triage/i });

    await expect(page.locator('section[aria-label="Waiting on triage"]')).toBeVisible();
    // One column per disposition, named for the step it opens — not
    // for the disposition slug. "Reproduce and investigate" tells a
    // triager what happens; "reproduce" does not.
    for (const label of [
      'Reproduce and investigate',
      'Decide the design',
      'Build the change',
      'Waiting on the reporter',
      'Closed as a duplicate',
      'Closed without action',
    ]) {
      await expect(page.locator(`section[aria-label="${label}"]`)).toBeVisible();
    }
    // The old model's column must not survive.
    await expect(page.locator('section[aria-label="Triaged"]')).toHaveCount(0);
  });

  test('sorts each item by its fork step, not a stored column', async ({ page }) => {
    await mountPage(page, '/it/design/feedback', { titleMatch: /feedback triage/i });

    const waiting = page.locator('section[aria-label="Waiting on triage"]');
    await expect(waiting).toContainText('Column picker forgets my choice');
    // Handed to an agent is an annotation, not a destination — this
    // card is still waiting on a human decision.
    await expect(waiting).toContainText('Typo on the vendors page');
    await expect(waiting).toContainText(/with an agent/i);

    // Routed at triage, so it sits under the route it was sent to.
    await expect(page.locator('section[aria-label="Decide the design"]')).toContainText(
      'Needs a design call',
    );

    // Routed AND closed: only in Closed. A closed packet rendering
    // under its old route is the live defect this board shipped with —
    // 182 closed cards indistinguishable from 16 open ones.
    const design = page.locator('section[aria-label="Decide the design"]');
    await expect(design).not.toContainText('Fixed last week');
    await expect(page.locator('section[aria-label="Closed"]')).toContainText('Fixed last week');
  });

  test('routing an item completes the fork step with that disposition', async ({ page }) => {
    let body: Record<string, unknown> | null = null;
    await page.route(/\/api\/jobs\/[^/]+\/steps\/[^/]+$/, async (route) => {
      if (route.request().method() !== 'PUT') return route.fallback();
      body = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({ json: {} });
    });

    await mountPage(page, '/it/design/feedback', { titleMatch: /feedback triage/i });
    const card = page.locator('article', { hasText: 'Column picker forgets my choice' });
    await card.getByLabel('Route this item').selectOption('build');
    await card.getByRole('button', { name: /^route$/i }).click();

    await expect.poll(() => body !== null).toBe(true);
    const sent = body as unknown as { status: string; metadata: Record<string, unknown> };
    // Routing IS triaging: one write that both records the decision
    // and completes the step, so the next step opens.
    expect(sent.status).toBe('completed');
    expect(sent.metadata['disposition']).toBe('build');
    // The merge that keeps the step findable must survive.
    expect(sent.metadata['authority_role']).toBe('platform-admin');
  });

  test('dragging onto a route does exactly what picking it does', async ({ page }) => {
    let body: Record<string, unknown> | null = null;
    await page.route(/\/api\/jobs\/[^/]+\/steps\/[^/]+$/, async (route) => {
      if (route.request().method() !== 'PUT') return route.fallback();
      body = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({ json: {} });
    });

    await mountPage(page, '/it/design/feedback', { titleMatch: /feedback triage/i });
    await page
      .locator('article', { hasText: 'Column picker forgets my choice' })
      .dragTo(page.locator('section[aria-label="Reproduce and investigate"]'));

    await expect.poll(() => body !== null).toBe(true);
    const sent = body as unknown as { status: string; metadata: Record<string, unknown> };
    expect(sent.status).toBe('completed');
    expect(sent.metadata['disposition']).toBe('reproduce');
  });

  test('lifting a card offers every route as a drop target', async ({ page }) => {
    await mountPage(page, '/it/design/feedback', { titleMatch: /feedback triage/i });
    await expect(page.locator('.tb-drop-zone')).toHaveCount(0);

    const card = page.locator('article', { hasText: 'Column picker forgets my choice' });
    const box = await card.boundingBox();
    if (!box) throw new Error('card has no box');
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2, box.y + 140, { steps: 8 });

    // Six routes offered, and the column it came from is not one of
    // them — dropping back where you started is not a disposition.
    await expect(page.locator('.tb-drop-zone')).toHaveCount(6);
    await expect(
      page.locator('section[aria-label="Waiting on triage"]').locator('.tb-drop-zone'),
    ).toHaveCount(0);

    await page.mouse.up();
  });

  test('an already-routed card cannot be dragged or re-routed', async ({ page }) => {
    await mountPage(page, '/it/design/feedback', { titleMatch: /feedback triage/i });

    const routed = page.locator('section[aria-label="Decide the design"]').locator('article');
    // A completed fork step does not un-complete.
    await expect(routed.first()).toHaveAttribute('draggable', 'false');
    await expect(routed.getByRole('button', { name: /^route$/i })).toHaveCount(0);

    const waiting = page
      .locator('section[aria-label="Waiting on triage"]')
      .locator('article')
      .first();
    await expect(waiting).toHaveAttribute('draggable', 'true');
  });

  // A Job materialises its steps at open and keeps them, so Jobs
  // opened before the fork existed carry the old shape forever — a
  // gated triage step with no `disposition` field. They must stay
  // routable. Without a fallback the board renders their cards and
  // every control silently does nothing, which is the worst failure
  // available: it looks fine and refuses to work.
  test('items opened before the fork existed are still routable', async ({ page }) => {
    const legacy = {
      ...job('fb-legacy', 'Filed before the fork', { status: 'ready' }),
      steps: [
        { id: 'fb-legacy-t', kind: 'trigger', status: 'completed', fields: [], metadata: {} },
        {
          id: 'fb-legacy-a',
          kind: 'task',
          status: 'ready',
          fields: [], // the pre-fork shape
          metadata: { authority_role: 'platform-admin' },
        },
        { id: 'fb-legacy-o', kind: 'outcome', status: 'pending', fields: [], metadata: {} },
      ],
    };
    await page.route(/\/api\/jobs\?kind=user-feedback/, (r) =>
      r.fulfill({ json: { data: [legacy], total: 1 } }),
    );

    let body: Record<string, unknown> | null = null;
    let url = '';
    await page.route(/\/api\/jobs\/[^/]+\/steps\/[^/]+$/, async (route) => {
      if (route.request().method() !== 'PUT') return route.fallback();
      url = route.request().url();
      body = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({ json: {} });
    });

    await mountPage(page, '/it/design/feedback', { titleMatch: /feedback triage/i });

    const card = page.locator('article', { hasText: 'Filed before the fork' });
    await expect(page.locator('section[aria-label="Waiting on triage"]')).toContainText(
      'Filed before the fork',
    );
    await card.getByLabel('Route this item').selectOption('decline');
    await card.getByRole('button', { name: /^route$/i }).click();

    await expect.poll(() => body !== null).toBe(true);
    // It targets the gated step it does have, and still records the
    // decision rather than closing anonymously.
    expect(url).toContain('fb-legacy-a');
    const sent = body as unknown as { status: string; metadata: Record<string, unknown> };
    expect(sent.status).toBe('completed');
    expect(sent.metadata['disposition']).toBe('decline');
  });

  // A card whose cause is known must not look like an untouched one.
  // That is not cosmetic: three diagnosed items with shipped fixes sat
  // in "waiting" for an entire session because the board could only
  // record that an agent had been ASKED, never what it came back with.
  test('a recorded finding shows on the card', async ({ page }) => {
    const withFinding = [
      job('fb-found', 'Button is unreadable', {
        status: 'ready',
        metadata: {
          finding: 'color: inherit on a bar that sets none — 1.06:1 in light theme.',
          finding_by: 'emp-bootstrap-admin',
        },
      }),
    ];
    await page.route(/\/api\/jobs\?kind=user-feedback/, (r) =>
      r.fulfill({ json: { data: withFinding, total: 1 } }),
    );
    await mountPage(page, '/it/design/feedback', { titleMatch: /feedback triage/i });

    const card = page.locator('article', { hasText: 'Button is unreadable' });
    await expect(card).toContainText('color: inherit on a bar that sets none');
    // Evidence without an author is a rumour.
    await expect(card).toContainText(/found by emp-bootstrap-admin/i);
  });

  test('recording a finding writes it with provenance, and decides nothing', async ({
    page,
  }) => {
    let body: Record<string, unknown> | null = null;
    await page.route(/\/api\/jobs\/[^/]+\/steps\/[^/]+$/, async (route) => {
      if (route.request().method() !== 'PUT') return route.fallback();
      body = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({ json: {} });
    });

    await mountPage(page, '/it/design/feedback', { titleMatch: /feedback triage/i });
    const card = page.locator('article', { hasText: 'Column picker forgets my choice' });
    await card.getByRole('button', { name: /record finding/i }).click();
    await card.getByLabel(/what did you find/i).fill('Root cause: the picker never persists.');
    await card.getByRole('button', { name: /save finding/i }).click();

    await expect.poll(() => body !== null).toBe(true);
    const sent = body as unknown as { metadata: Record<string, unknown> };
    expect(sent.metadata['finding']).toContain('never persists');
    expect(sent.metadata['finding_by']).toBeTruthy();
    // Finding something is not deciding what to do about it — the item
    // stays in triage until somebody routes it.
    expect(body).not.toHaveProperty('status');
    expect(sent.metadata['disposition']).toBeUndefined();
    // The gate that keeps the step findable must survive.
    expect(sent.metadata['authority_role']).toBe('platform-admin');
  });

  // The finding is evidence for the routing decision, so it has to
  // outlive it — otherwise the reason a card went where it went is
  // gone the moment it gets there.
  test('a finding survives routing and shows on the routed card', async ({ page }) => {
    const routed = [
      job('fb-routed-found', 'Needs a design call', {
        status: 'completed',
        metadata: {
          disposition: 'design',
          finding: 'Generalises past feedback — this is the shape of every triage queue.',
          finding_by: 'automation:triage-agent',
        },
      }),
    ];
    await page.route(/\/api\/jobs\?kind=user-feedback/, (r) =>
      r.fulfill({ json: { data: routed, total: 1 } }),
    );
    await mountPage(page, '/it/design/feedback', { titleMatch: /feedback triage/i });

    const column = page.locator('section[aria-label="Decide the design"]');
    await expect(column).toContainText('Generalises past feedback');
    // An agent-written finding renders exactly like a human one.
    await expect(column).toContainText(/found by automation:triage-agent/i);
  });

  test('handing to an agent records a durable request without deciding', async ({ page }) => {
    let body: Record<string, unknown> | null = null;
    await page.route(/\/api\/jobs\/[^/]+\/steps\/[^/]+$/, async (route) => {
      if (route.request().method() !== 'PUT') return route.fallback();
      body = route.request().postDataJSON() as Record<string, unknown>;
      return route.fulfill({ json: {} });
    });

    await mountPage(page, '/it/design/feedback', { titleMatch: /feedback triage/i });
    const card = page.locator('article', { hasText: 'Column picker forgets my choice' });
    await card.getByRole('button', { name: /hand to agent/i }).click();

    await expect.poll(() => body !== null).toBe(true);
    const sent = body as unknown as { metadata: Record<string, unknown> };
    expect(sent.metadata['agent_requested_at']).toBeTruthy();
    // An agent looking is not a decision — no status, no disposition.
    expect(body).not.toHaveProperty('status');
    expect(sent.metadata['disposition']).toBeUndefined();
  });
});
