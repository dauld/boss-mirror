// Design review list (/it/design) — content-level guard for the
// docs-review surface: the table must show each indexed doc with its
// LIVE open-question count (a doc with 3 unresolved `### Qn:` anchors
// must not read "0" — the pre-2026-07-06 page showed pending_count,
// i.e. unflushed decisions, under an "Open Qs" header) and offer the
// review-Job entry point. Route-smoke only asserts the page mounts.

import { test, expect } from '@playwright/test';
import { mountPage } from '../smoke/_helpers';

const DOCS = [
  {
    path: 'docs/architecture-decisions.md',
    title: 'Inventory value conservation (costing PR 6)',
    status: 'in-review',
    open_questions: 3,
    pending_count: 0,
    word_count: 941,
    last_modified: new Date().toISOString(),
    last_author: 'david',
    last_indexed_at: new Date().toISOString(),
    last_commit_sha: 'abc1234',
    content_html: '<h1>Inventory value conservation</h1>',
  },
  {
    path: 'docs/design/correctness-protocol.md',
    title: 'The BOSS correctness protocol',
    status: 'living',
    open_questions: 0,
    pending_count: 0,
    word_count: 1398,
    last_modified: new Date().toISOString(),
    last_author: 'david',
    last_indexed_at: new Date().toISOString(),
    last_commit_sha: 'def5678',
    content_html: '<h1>The BOSS correctness protocol</h1>',
  },
];

test.beforeEach(async ({ page }) => {
  // THE QUEUE IS THE PAGE, and it is the one read that throws — so a
  // spec that does not mock it renders chrome and nothing else, and
  // every content assertion below fails with "element not found".
  //
  // That is exactly how this suite rotted. `/it/design` grew a
  // station-lens read on 2026-08-15; these specs were never updated,
  // and eight of ten went red on main without anyone noticing,
  // because NOTHING RUNS THEM — not gate.sh, not ci.yml. Thirteen
  // mocked spec files, zero gated (filed separately).
  //
  // An empty lens is deliberate: `panelsFor` falls back to every
  // known panel when a lens declares none, so this exercises the
  // default the page ships with rather than a shape invented here.
  await page.route('**/api/stations/design-review/queue', (route) =>
    route.fulfill({ json: { station: 'design-review', lens: null, packets: [], steps: {} } }),
  );
  await page.route('**/api/design/docs', (route) =>
    route.fulfill({ json: DOCS }),
  );
  // Empty by default: the common case is a fully-indexed corpus, and
  // it keeps the panel out of the way of the assertions below. The
  // non-empty case has its own test.
  await page.route('**/api/design/rejections', (route) =>
    route.fulfill({ json: [] }),
  );
  await page.route('**/api/design/stale-statuses', (route) =>
    route.fulfill({ json: [] }),
  );
  await page.route('**/api/jobs?*', (route) =>
    route.fulfill({ json: { jobs: [], total: 0 } }),
  );
});

// The POST contract the page must satisfy: jobs-api deserializes the
// identity-first Subject ({subject_kind, id}) — the retired
// {custom_kind, ref_id} shape 422s with "missing field `id`", which is
// exactly how this page's button silently died in production. The mock
// enforces the shape so the regression fails in CI, not on the box.
async function installJobCreateMock(page: import('@playwright/test').Page) {
  await page.route('**/api/jobs', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const body = route.request().postDataJSON() as {
      kind: string;
      subject?: { subject_kind?: string; id?: string };
    };
    if (!body.subject?.id || !body.subject?.subject_kind) {
      return route.fulfill({
        status: 422,
        body: 'invalid job body: missing field `id`',
      });
    }
    return route.fulfill({
      json: {
        id: 'job-review-1',
        kind: body.kind,
        status: 'open',
        title: 'Review',
        subject: body.subject,
        steps: [],
      },
    });
  });
  await page.route('**/api/jobs/job-review-1', (route) =>
    route.fulfill({
      json: {
        id: 'job-review-1',
        status: 'open',
        steps: [
          { id: 'step-1', kind: 'review-design', status: 'pending', metadata: {} },
        ],
      },
    }),
  );
  await page.route('**/api/jobs/job-review-1/steps/step-1', (route) =>
    route.fulfill({
      json: { id: 'step-1', kind: 'review-design', status: 'pending', metadata: {} },
    }),
  );
}

test.describe('Design review list', () => {
  test('shows live open-question counts, not pending decisions', async ({
    page,
  }) => {
    await mountPage(page, '/it/design', { titleMatch: /design review/i });

    const row = page.locator('tr', {
      hasText: 'Inventory value conservation',
    });
    await expect(row).toBeVisible({ timeout: 10_000 });
    // Column order: doc, status, open Qs, pending decisions, …
    await expect(row.locator('td').nth(2)).toHaveText('3');
    await expect(row.locator('td').nth(3)).toHaveText('0');

    const settled = page.locator('tr', { hasText: 'correctness protocol' });
    await expect(settled.locator('td').nth(2)).toHaveText('0');
  });

  test('splits docs under discussion from living references', async ({
    page,
  }) => {
    await mountPage(page, '/it/design', { titleMatch: /design review/i });
    // The doc with open questions sits in the section that wants a
    // decision; the living reference sits in the settled corpus with a
    // reopen affordance — the pre-2026-07-08 page showed both as
    // "in-review".
    //
    // Section names updated 2026-08-16: the page moved from a two-way
    // split ("In review & discussion" / "Living references & settled")
    // to the three-way grouping in designGroups.ts — needs-you / draft
    // / library. The old names lived on here for days because NOTHING
    // RUNS THIS SUITE: thirteen mocked spec files, and neither gate.sh
    // nor ci.yml executes any of them, so eight of these ten tests
    // were red on main and silent about it.
    const reviewing = page.locator('section', { hasText: 'Needs you' }).first();
    await expect(
      reviewing.locator('tr', { hasText: 'Inventory value conservation' }),
    ).toBeVisible({ timeout: 10_000 });
    const settled = page.locator('section', { hasText: 'Design library' }).first();
    await expect(
      settled.locator('tr', { hasText: 'correctness protocol' }),
    ).toBeVisible();
    await expect(settled.locator('td', { hasText: 'living' })).toBeVisible();
    await expect(
      settled.getByRole('button', { name: /reopen discussion/i }),
    ).toBeVisible();
  });

  test('docs without an open review offer the review-Job entry point', async ({
    page,
  }) => {
    await mountPage(page, '/it/design', { titleMatch: /design review/i });
    // One doc is under discussion ("Open review Job"), one is a living
    // reference ("Reopen discussion") — every doc gets exactly one
    // affordance, worded for its state.
    await expect(
      // Label updated 2026-08-16: the button reads "Start review"
      // now, not "Open review Job". Another casualty of a suite
      // nothing runs.
      page.getByRole('button', { name: /start review/i }),
    ).toHaveCount(1);
    await expect(
      page.getByRole('button', { name: /reopen discussion/i }),
    ).toHaveCount(1);
  });

  test('Open review Job posts the identity-first subject shape', async ({
    page,
  }) => {
    await installJobCreateMock(page);
    await mountPage(page, '/it/design', { titleMatch: /design review/i });
    const row = page.locator('tr', {
      hasText: 'Inventory value conservation',
    });
    await row.getByRole('button', { name: /start review/i }).click();
    // A 422 from the shape-enforcing mock surfaces as the page error
    // banner; success re-loads the list. Assert no error rendered.
    await expect(page.locator('.empty', { hasText: /HTTP 422/ })).toHaveCount(
      0,
    );
  });

  test('names docs the reindexer refused, and how long they have been invisible', async ({
    page,
  }) => {
    const sixDaysAgo = new Date(Date.now() - 6 * 86_400_000).toISOString();
    await page.route('**/api/design/rejections', (route) =>
      route.fulfill({
        json: [
          {
            path: 'docs/design/half-written.md',
            reason: 'no title heading',
            first_seen_at: sixDaysAgo,
            last_seen_at: new Date().toISOString(),
          },
        ],
      }),
    );
    await mountPage(page, '/it/design', { titleMatch: /design review/i });
    await expect(
      page.getByRole('heading', { name: /not indexed \(1\)/i }),
    ).toBeVisible();
    const row = page.locator('tr', { hasText: 'docs/design/half-written.md' });
    await expect(row).toContainText('6 days');
    await expect(row).toContainText('no title heading');
  });

  test('an in-review doc links to the full-page step surface, not the job page', async ({
    page,
  }) => {
    // Reading the doc is the entire point of this Job. The job page
    // renders the review plugin in a panel beside the sidebar, the job
    // header and the step list; the step-focus route renders it under
    // the chrome bar with the whole panel to itself. The list must
    // link to the second one.
    // Seeded through the STATION QUEUE, which is where the page reads
    // open reviews from — `reviewsByDocPath(queue.data)`. It used to
    // read /api/jobs?*, and seeding the old source here meant the page
    // saw no existing review and POSTed a second one, navigating to a
    // freshly minted uuid instead of job-review-9.
    await page.route('**/api/stations/design-review/queue', (route) =>
      route.fulfill({
        json: {
          station: 'design-review',
          discipline: ['priority', 'age'],
          lens: null,
          total: 1,
          data: [
            {
              id: 'job-review-9',
              title: 'Review: Inventory value conservation',
              status: 'open',
              opened_on: '2026-08-01',
              subject: { id: 'docs/architecture-decisions.md' },
              steps: [
                { id: 'step-other', kind: 'sign-off' },
                { id: 'step-rd', kind: 'review-design' },
              ],
            },
          ],
        },
      }),
    );
    // `reviewStepId` re-reads the Job to find its review-design step
    // rather than trusting the queue packet, so the step surface is
    // only reachable when this answers.
    await page.route('**/api/jobs/job-review-9', (route) =>
      route.fulfill({
        json: {
          id: 'job-review-9',
          kind: 'design-doc-review',
          title: 'Review: Inventory value conservation',
          status: 'open',
          subject: { subject_kind: 'custom', id: 'docs/architecture-decisions.md' },
          steps: [
            { id: 'step-other', spec_slug: 'open', kind: 'sign-off', status: 'completed' },
            { id: 'step-rd', spec_slug: 'review', kind: 'review-design', status: 'ready' },
          ],
        },
      }),
    );
    await mountPage(page, '/it/design', { titleMatch: /design review/i });
    // Asserted through NAVIGATION, not an href. The column used to
    // fork — a link when a Job existed, a button when it did not — and
    // David killed that on 2026-08-14: "that link should just
    // consistently launch the review UX". There is one button now, so
    // the property survives but the mechanism it is read through does
    // not. Rewritten rather than deleted: where the control lands is
    // still the whole point of the test.
    await page.getByRole('button', { name: /review/i }).first().click();
    await expect(page).toHaveURL(/\/jobs\/job-review-9\/steps\/step-rd(\?|$)/);
    // The `from` pair is not incidental: the step surface cannot infer
    // where Back should go, and guessing from the Job's kind would put
    // a per-workflow branch in core routing. Only the lens that sent
    // the operator here knows, so it says.
    await expect(page).toHaveURL(/from=%2Fit%2Fdesign/);
    await expect(page).toHaveURL(/from_label=Design(%20|\+)Review/);
  });

  test('falls back to the job page when the Job has no review step yet', async ({
    page,
  }) => {
    // A Job caught before its steps materialize has no step id. Better
    // the job page than a link to a step id we invented.
    await page.route('**/api/stations/design-review/queue', (route) =>
      route.fulfill({
        json: {
          station: 'design-review',
          discipline: ['priority', 'age'],
          lens: null,
          total: 1,
          data: [
            {
              id: 'job-review-9',
              title: 'Review: Inventory value conservation',
              status: 'open',
              opened_on: '2026-08-01',
              subject: { id: 'docs/architecture-decisions.md' },
              steps: [],
            },
          ],
        },
      }),
    );
    await mountPage(page, '/it/design', { titleMatch: /design review/i });
    await page.getByRole('button', { name: /review/i }).first().click();
    // No step id to link to, so the job page — never a step id we
    // invented.
    await expect(page).toHaveURL(/\/service\/job-review-9(\?|$)/);
  });

  test('a failing rejections call does not blank the page', async ({
    page,
  }) => {
    // Rejections are supplementary. Throwing on a non-OK response
    // replaced the entire surface with an error banner — the docs
    // list, the counts, every review button — over a panel that
    // renders nothing when empty anyway.
    await page.route('**/api/design/rejections', (route) =>
      route.fulfill({ status: 500, body: 'boom' }),
    );
    await mountPage(page, '/it/design', { titleMatch: /design review/i });
    await expect(
      page.locator('tr', { hasText: 'Inventory value conservation' }),
    ).toHaveCount(1);
    await expect(page.locator('.empty', { hasText: /^Error:/ })).toHaveCount(0);
  });
});

// The status line is hand-written and almost nothing updates it, so it
// goes stale by default: on 2026-08-15 eleven of the twenty docs
// claiming to be live had nothing open, every one wrong in the same
// direction, and no surface said so (0b8ae875). This is the surface.
//
// Rendered rather than reasoned about, per the invariant
// `a-rendered-surface-is-verified-by-rendering-it` — which exists
// because a landing-page fix was applied to two wrong files before
// anyone rendered the third.
test('a doc whose status drifted is reported, with what it claims', async ({ page }) => {
  await page.route('**/api/design/stale-statuses', (route) =>
    route.fulfill({
      json: [
        {
          path: 'docs/design/stations.md',
          title: 'Stations',
          status: 'in-review',
          reason:
            'status is `in-review`, which asserts live discussion, but the doc has no open questions',
        },
      ],
    }),
  );
  await mountPage(page, '/it/design');

  await expect(page.getByText('Status drifted (1)')).toBeVisible();
  await expect(page.getByText('docs/design/stations.md')).toBeVisible();
  // The reason has to travel to the surface. A panel that says a doc
  // is wrong without saying how is one an operator has to go
  // investigate before they can act, which is most of the cost.
  await expect(page.getByText(/no open questions/)).toBeVisible();
});

// Empty is the healthy state and must render as NOTHING, not as an
// empty table. A panel that is always present teaches people to skip
// the region it lives in.
test('a corpus with no drift shows no panel at all', async ({ page }) => {
  await mountPage(page, '/it/design');
  await expect(page.getByText(/Status drifted/)).toHaveCount(0);
});

