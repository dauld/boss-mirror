// /it/design shows what is asking for something, not what claims to be
// (David, bedda461: "This page is full of stale info").
//
// The fixture IS the measured corpus of 2026-08-15, including the
// eleven docs that were cluttering the top section on the strength of
// a frontmatter line alone.

import { expect, test, type Page, type Route } from '@playwright/test';

// The live corpus as measured 2026-08-15: 38 docs, 9 with open
// questions, 11 claiming in-review/draft with nothing open.
const D = (path: string, status: string, q = 0, p = 0) =>
  ({ path: `docs/design/${path}`, title: path.replace('.md', '').replace(/-/g, ' '),
     status, open_questions: q, pending_count: p, word_count: 2000,
     last_modified: '2026-08-14T00:00:00Z' });

const DOCS = [
  D('design-docs-as-data.md', 'in-review', 8), D('payload-encryption.md', 'in-review', 5),
  D('queue-visibility.md', 'in-review', 5), D('views-as-queue-lenses.md', 'in-review', 5),
  D('workflow-ux-as-data.md', 'in-review', 5), D('department-flow-dashboards.md', 'in-review', 4),
  D('packet-loss.md', 'in-review', 4), D('design-conformance.md', 'in-review', 3),
  D('dev-cluster.md', 'in-review', 3), D('feedback-triage-agent.md', 'in-review', 3),
  D('framing-convergence.md', 'in-review', 3),
  D('idm-kanidm.md', 'draft', 0, 4),
  // The eleven that were cluttering the top section.
  D('crates-and-layers.md', 'in-review'), D('departure-board.md', 'in-review'),
  D('protocol-cadence.md', 'in-review'), D('protocol-experiments.md', 'in-review'),
  D('requirements-based-addressing.md', 'in-review'), D('self-inflicted-outage.md', 'in-review'),
  D('stations.md', 'in-review'), D('deployment-as-network.md', 'draft'),
  D('internal-forge.md', 'draft'), D('job-packet-network.md', 'draft'),
  D('protocol-policy-publish.md', 'draft'),
  D('class-registry.md', 'living'), D('testing-strategy.md', 'living'),
  D('the-three-layers.md', 'living'), D('transactional-audit-log.md', 'approved'),
];

async function mocks(page: Page) {
  const json = (r: Route, b: unknown) =>
    r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [{ id: 'emp-david', name: 'David', email: 'd@a',
    role: 'platform-admin', department: 'it', hire_date: '2023-01-01', status: 'active',
    location: 'loc-hq', employment_type: 'full-time', skills: [], certifications: [] }]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: 'emp-david', role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/live$/, (r) => json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/design\/docs/, (r) => json(r, DOCS));
  await page.route(/\/api\/design\/rejections/, (r) => json(r, []));
  await page.route(/\/api\/stations\/design-review\/queue/, (r) => json(r, {
    station: 'design-review', kind: 'batch', discipline: ['priority', 'age'], over_limit: false,
    total: 0, data: [],
    lens: { eyebrow: 'System Model · Design review', title: 'Design review',
            subtitle: 'Open questions, pending decisions, ADRs', panels: ['rejections', 'corpus'] },
  }));
}

test('a doc claiming in-review with nothing open is in the library, not your queue', async ({ page }) => {
  await mocks(page);
  await page.goto('/it/design');

  const needs = page.locator('section', { has: page.getByText(/^Needs you/) });
  const library = page.locator('section', { has: page.getByText(/^Design library/) });

  await expect(page.getByText('Needs you (12)')).toBeVisible();
  await expect(page.getByText('Design library (11)')).toBeVisible();
  await expect(page.getByText('Being written (4)')).toBeVisible();

  // The exact docs that were wrong before: in-review, nothing open.
  for (const stale of ['crates-and-layers', 'departure-board', 'protocol-cadence', 'stations']) {
    await expect(library).toContainText(stale);
    await expect(needs).not.toContainText(stale);
  }

  // Deepest first — the page is a queue, and 8 questions is a
  // different size of afternoon from 3.
  const firstRow = needs.locator('tbody tr').first();
  await expect(firstRow).toContainText('design-docs-as-data');

  // A doc with only unflushed answers still needs you: those are
  // decisions that never reached the doc.
  await expect(needs).toContainText('idm-kanidm');
  await expect(needs).toContainText('4 recorded answers not yet flushed');

  // A draft that has asked nothing is neither settled nor pending.
  const written = page.locator('section', { has: page.getByText(/^Being written/) });
  await expect(written).toContainText('internal-forge');

  // The pointer David asked for.
  await expect(library).toContainText('architecture-decisions.md');
  await expect(library.getByRole('link', { name: /Knowledge Base/ })).toBeVisible();
});

test('design page screenshot', async ({ page }, testInfo) => {
  await mocks(page);
  await page.goto('/it/design');
  await page.waitForTimeout(1200);
  await page.screenshot({ path: testInfo.outputPath('design.png'), fullPage: true });
});
