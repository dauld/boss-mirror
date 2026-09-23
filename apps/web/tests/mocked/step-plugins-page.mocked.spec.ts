// /it/registry/step-plugins — every control the page audit counted,
// pinned (page-audit 5ff999f8, step `test`).
//
// The audit's inventory (controls_md, measured 2026-09-23 against
// c49ed647): 6 tab links + one Kind link per plugin row, 0 buttons,
// 0 forms, 1 read (`GET /api/jobs/step-plugins`, no query), 0 writes.
// Before this spec the route was reached only by the three crawls:
// route-smoke paints it, interaction-crawl follows its links, and
// outage-crawl holds it in SILENT. Nothing asserted what a row says,
// how the rows are grouped and ordered, or what the header claims when
// the read fails.
//
// THIS SPEC PINS THE PAGE AS IT IS, INCLUDING TWO FILED DEFECTS, and
// says so at each one. Gap 1 (backlog 7267f9ce): the failure line has
// no FAILURE_MARKER and no role=alert. Gap 2 (backlog 044f55e4): under
// a failed read the header still counts "0 active plugins across 0
// categories" — the same words the empty backend paints. The car that
// fixes either one flips the assertion named for it here; that is the
// point of pinning it rather than skipping it.
//
// The fixture is four of the twelve live rows (read 2026-09-23 through
// boss-api), listed out of order so the per-category sort is visible.
// Every live row is owned by `platform`; marketing-brief is given
// `marketing` here, because the page has a second Owner branch and no
// live row reaches it.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { parseRoute } from '../../src/router';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { sectionForRoute } from '../../src/shell/sections';

const PAGE = '/it/registry/step-plugins';
const TITLE = 'Step UX plugins';

/// The page's one read, as StepPluginsPage.svelte spells it — anchored,
/// so the detail page's `/api/jobs/step-plugins/<kind>` is not it.
const PLUGINS = /\/api\/jobs\/step-plugins$/;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const plugin = (
  kind: string,
  label: string,
  category: string,
  version: number,
  owning_team = 'platform',
) => ({
  kind,
  version,
  status: 'active',
  label,
  description: `${label} — description the page does not render`,
  category,
  metadata_schema: {},
  frontend_url: `${kind}.js`,
  owning_team,
  authoring_job_id: null,
  created_at: '2026-09-16T23:54:53.174029Z',
});

const ROWS = [
  plugin('sign-off', 'Sign off', 'coordination', 3),
  plugin('review-design', 'Review Design', 'platform', 1),
  plugin('answer-question', 'Answer a question', 'coordination', 1),
  plugin('marketing-brief', 'Marketing Brief', 'admin', 1, 'marketing'),
];

async function install(page: Page, rows: unknown = ROWS): Promise<void> {
  await installSmokeMocks(page);
  await page.route(PLUGINS, (r) => json(r, rows));
}

/// The shell's own write: App.svelte posts one surface-open per
/// navigation (shell/surface-opens.ts), on every route alike. It is
/// chrome, not a control of this page, so the page's write count
/// excludes it — and only it.
const SHELL_WRITE = /\/api\/surface-opens$/;

/// Every non-GET the page issues, from the moment it is installed.
function watchWrites(page: Page): Request[] {
  const writes: Request[] = [];
  page.on('request', (r) => {
    const url = r.url();
    if (url.includes('/api/') && r.method() !== 'GET' && !SHELL_WRITE.test(new URL(url).pathname)) {
      writes.push(r);
    }
  });
  return writes;
}

/// One table per category, top to bottom: [section title, kinds in order].
/// textContent, not innerText: the stylesheet upper-cases a section
/// title, and the words are the category code as the registry spells it.
async function sections(page: Page): Promise<Array<[string, string[]]>> {
  return page.locator('.catalog section.tab-section').evaluateAll((els) =>
    els.map(
      (s) =>
        [
          (s.querySelector('h3')?.textContent ?? '').trim(),
          [...s.querySelectorAll('tbody tr td:first-child')].map((td) => (td.textContent ?? '').trim()),
        ] as [string, string[]],
    ),
  );
}

/// Is `path` a catalogued route: the router serves it and the section it
/// lights is a ROUTE_CATALOG key. Read from nav-catalog.ts and
/// sections.ts, never from a second list here.
function catalogued(path: string): string | null {
  const section = sectionForRoute(parseRoute(path));
  return section in ROUTE_CATALOG ? section : null;
}

test.describe('/it/registry/step-plugins — the rows', () => {
  test('groups rows by category, sorts each by kind, and renders the five columns', async ({ page }) => {
    await install(page);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const header = page.locator('header.exec-header');
    await expect(header.locator('.exec-eyebrow')).toHaveText('Platform · Step plugins');
    await expect(header.locator('p')).toHaveText('4 active plugins across 3 categories');

    await expect.poll(() => sections(page)).toEqual([
      ['admin', ['marketing-brief']],
      ['coordination', ['answer-question', 'sign-off']],
      ['platform', ['review-design']],
    ]);

    const heads = await page.locator('.catalog table').first().locator('thead th').allTextContents();
    expect(heads.map((h) => h.trim())).toEqual(['Kind', 'Label', 'Owner', 'Version', 'Frontend bundle']);

    const row = (kind: string) => page.locator('.catalog tbody tr').filter({ hasText: kind });
    await expect(row('sign-off').locator('td')).toHaveText(['sign-off', 'Sign off', 'system', '3', 'sign-off.js']);
    // The second Owner branch: a team that is not `platform` is named.
    await expect(row('marketing-brief').locator('td').nth(2)).toHaveText('marketing');
    // The bundle is text, not a link to /plugins/<url>.
    await expect(page.locator('.catalog a[href*="/plugins/"]')).toHaveCount(0);
    // Received and not rendered (gap 5, 9ce24f0b names provenance).
    await expect(page.getByText('description the page does not render')).toHaveCount(0);
  });

  test('one row says plugin and category in the singular', async ({ page }) => {
    await install(page, [ROWS[0]]);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });
    await expect(page.locator('header.exec-header p')).toHaveText('1 active plugin across 1 category');
  });
});

test.describe('/it/registry/step-plugins — the controls', () => {
  test('one read, no buttons, no forms, no writes', async ({ page }) => {
    const writes = watchWrites(page);
    const reads: string[] = [];
    page.on('request', (r) => {
      if (PLUGINS.test(new URL(r.url()).pathname)) reads.push(r.url());
    });
    await install(page);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    expect(await settledReads(page, () => reads.length, 1)).toBe(1);
    expect(new URL(reads[0]!).search, 'the page reads every category, unfiltered').toBe('');
    await expect(page.locator('.catalog button')).toHaveCount(0);
    await expect(page.locator('.catalog form')).toHaveCount(0);
    await expect(page.locator('nav.it-tabs button')).toHaveCount(0);
    expect(writes.map((w) => `${w.method()} ${w.url()}`)).toEqual([]);
  });

  test('the six registry tabs land on catalogued routes, and back returns here', async ({ page }) => {
    const writes = watchWrites(page);
    await install(page);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const tabs = page.locator('nav.it-tabs[aria-label="IT registry"] a');
    await expect(tabs).toHaveText(['Workflows', 'Dispatcher', 'Step plugins', 'Policy', 'Subjects', 'Drift']);
    const hrefs = await tabs.evaluateAll((as) => as.map((a) => a.getAttribute('href') ?? ''));
    const catalogPaths = new Set(Object.values(ROUTE_CATALOG).map((r) => (r as { path: string }).path));
    for (const h of hrefs) {
      expect(catalogPaths.has(h), `${h} is a ROUTE_CATALOG path`).toBe(true);
      expect(catalogued(h), `${h} lights a catalogued section`).not.toBeNull();
    }
    await expect(tabs.nth(2)).toHaveAttribute('aria-current', 'page');
    await expect(tabs.nth(2)).toHaveAttribute('href', PAGE);

    for (const [i, h] of hrefs.entries()) {
      if (h === PAGE) continue;
      await tabs.nth(i).click();
      await expect(page).toHaveURL((u) => u.pathname === h);
      await page.goBack();
      await expect(page).toHaveURL((u) => u.pathname === PAGE);
      await expect(page.locator('h1').first()).toHaveText(TITLE);
      await expect(page.locator('.catalog tbody tr')).toHaveCount(ROWS.length);
    }
    expect(writes.map((w) => `${w.method()} ${w.url()}`)).toEqual([]);
  });

  test('each Kind links to its catalogued detail page, and back returns here', async ({ page }) => {
    await install(page);
    const signOff = ROWS[0]!;
    await page.route(/\/api\/jobs\/step-plugins\/sign-off$/, (r) => json(r, signOff));
    await page.route(/\/api\/jobs\/step-plugins\/sign-off\/versions$/, (r) => json(r, [signOff]));
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const links = page.locator('.catalog a');
    await expect(links).toHaveCount(ROWS.length);
    const hrefs = (await links.evaluateAll((as) => as.map((a) => a.getAttribute('href') ?? ''))).sort();
    expect(hrefs).toEqual(ROWS.map((p) => `${PAGE}/${p.kind}`).sort());
    for (const h of hrefs) {
      const route = parseRoute(h);
      expect(route.kind, h).toBe('systemStepPluginDetail');
      expect(catalogued(h), `${h} lights the Step plugins row`).toBe('system-step-plugins');
    }

    await page.getByRole('link', { name: 'sign-off', exact: true }).click();
    await expect(page).toHaveURL((u) => u.pathname === `${PAGE}/sign-off`);
    await expect(page.locator('h1').first()).toHaveText('Sign off');
    await page.goBack();
    await expect(page).toHaveURL((u) => u.pathname === PAGE);
    await expect(page.locator('h1').first()).toHaveText(TITLE);
    await expect(page.locator('.catalog tbody tr')).toHaveCount(ROWS.length);
  });
});

test.describe('/it/registry/step-plugins — empty and failed are never the same paint', () => {
  const EMPTY =
    'No plugins installed yet. See infra/step-plugins/README.md for the shape; ' +
    'seed one with POST /api/jobs/step-plugins.';

  test('an empty registry says so, with no failure line and no tables', async ({ page }) => {
    await install(page, []);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    await expect(page.locator('.catalog p.empty')).toHaveText(EMPTY);
    await expect(page.locator('header.exec-header p')).toHaveText('0 active plugins across 0 categories');
    await expect(page.locator('.catalog table')).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.getByText('Failed to load')).toHaveCount(0);
  });

  test('a failed read says "Failed to load" and never the empty-state copy', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(PLUGINS, (r) => r.fulfill({ status: 503, contentType: 'text/plain', body: 'jobs down' }));
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const failure = page.locator('.catalog p.empty');
    await expect(failure).toHaveText('Failed to load: HTTP 503: jobs down');
    await expect(page.getByText('No plugins installed yet')).toHaveCount(0);
    await expect(page.locator('.catalog table')).toHaveCount(0);

    // GAP 1 (backlog 7267f9ce), pinned as it stands: the line carries
    // neither FAILURE_MARKER nor role=alert, which is why outage-crawl
    // holds this route in SILENT. The fixing car flips both counts to 1
    // and deletes that SILENT line.
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.locator('.catalog [role=alert]')).toHaveCount(0);

    // GAP 2 (backlog 044f55e4), pinned as it stands: the header counts
    // the failed read as an empty registry — the same words the empty
    // test above asserts. The fixing car replaces this with a header
    // that says the count is unknown.
    await expect(page.locator('header.exec-header p')).toHaveText('0 active plugins across 0 categories');
  });
});
