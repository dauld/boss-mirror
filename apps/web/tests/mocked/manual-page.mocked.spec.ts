// /manual — every control the page renders, pinned against a NON-EMPTY
// tree (page audit 5ab2662c, step `test`).
//
// Before this spec the route was reached only by the two crawls, and
// both render it against the mock's `[]` catch-all: route-smoke sees
// "No sections yet." and outage-crawl sees the tree's `.load-failed`.
// So 0 of the 17 live tree links and 0 of the 17 toggles were ever
// clicked, and collapse persistence and the ancestor auto-expand effect
// ran under no test at all (gap 9, 5be7378c). The fixture below is a
// small tree with the three shapes the page draws differently: a leaf
// at the root, a parent with two children, and a two-deep parent.
//
// The measure step's inventory (controls_md on the packet), which this
// spec covers item by item:
//   links    2 kinds  — tree section link; entity link in a section body
//   buttons  1 kind   — tree toggle (Expand / Collapse; a no-op on a leaf)
//   forms    0
//   reads    3        — the tree, the active section, and one
//                        /api/people/{id} per employee shortcode in
//                        that section's body (the whole roster until
//                        backlog 1e73bd93, which closed gap 3)
//   writes   0
//
// THIS STEP PINS WHAT THE PAGE DOES TODAY; it does not fix the gaps.
// Where today's behaviour IS a filed gap, the test says so in its name
// ("CURRENT, gap N") and asserts the current paint, so the car that
// fixes the gap has to flip that assertion — the fix is visible in this
// file rather than passing beside a spec that never noticed.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import { installSmokeMocks } from './_smokeMocks';
import { parseRoute } from '../../src/router';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// The page's three reads, as ManualPage.svelte spells them.
const TREE = /\/api\/content\/manual$/;
const SECTION = /\/api\/content\/manual\/(.+)$/;
const PERSON = /\/api\/people\/([^/]+)$/;

/// The one employee the fixture bodies name, as its own row answers.
/// The smoke ROSTER (the app shell's session read) names emp-001 "Demo
/// CEO", so a page that went back to reading the whole roster to name
/// a shortcode shows the wrong name and fails the shortcode test rather
/// than passing it by accident (backlog 1e73bd93).
const EMP_001 = { id: 'emp-001', name: 'Rhea Okafor' };

/// localStorage key the tree's collapse set lives under.
const COLLAPSED_KEY = 'boss.manual.collapsed';

const section = (
  slug: string,
  parent_slug: string | null,
  sort_order: number,
  title: string,
  body: string,
  current_version = 1,
) => ({
  id: `ms-${slug.replaceAll('/', '-')}`,
  slug, parent_slug, title, body, sort_order,
  audience: { all: true },
  current_version,
  published: true,
  created_at: '2026-09-16T12:00:00Z',
  updated_at: '2026-09-16T12:00:00Z',
});

/// Seven sections. `policies/security` sorts BEFORE `policies/conduct`
/// on sort_order although it sorts after it by title, so the order
/// below proves the tree sorts on sort_order first. The welcome body
/// carries an employee shortcode (the second link kind) and the seed's
/// `_TBD, owner: HR._` placeholder shape, which every live body has.
const SECTIONS = [
  section('benefits', null, 3, 'Benefits', '_TBD, owner: HR._'),
  section('welcome', null, 1, 'Welcome', 'Questions about this manual go to emp-001.\n\n_TBD, owner: HR._', 2),
  section('policies/conduct', 'policies', 2, 'Conduct', '_TBD, owner: HR._'),
  section('policies', null, 2, 'Policies', '_TBD, owner: HR._'),
  section('benefits/time-off/sick', 'benefits/time-off', 1, 'Sick leave', '_TBD, owner: HR._'),
  section('policies/security', 'policies', 1, 'Security', '_TBD, owner: Operations lead._'),
  section('benefits/time-off', 'benefits', 1, 'Time off', '_TBD, owner: HR._'),
];

/// Tree order, top to bottom, with each node's depth.
const TREE_ORDER: ReadonlyArray<readonly [string, number]> = [
  ['Welcome', 0],
  ['Policies', 0],
  ['Security', 1],
  ['Conduct', 1],
  ['Benefits', 0],
  ['Time off', 1],
  ['Sick leave', 2],
];

const PARENTS = ['policies', 'benefits', 'benefits/time-off'];
const LEAVES = ['welcome', 'policies/security', 'policies/conduct', 'benefits/time-off/sick'];

async function installManualReads(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(TREE, (r) => json(r, SECTIONS));
  await page.route(SECTION, (r) => {
    const slug = SECTION.exec(new URL(r.request().url()).pathname)?.[1] ?? '';
    const hit = SECTIONS.find((s) => s.slug === slug);
    return hit ? json(r, hit) : json(r, { error: 'not found' }, 404);
  });
  await page.route(PERSON, (r) => {
    const id = PERSON.exec(new URL(r.request().url()).pathname)?.[1] ?? '';
    return id === EMP_001.id ? json(r, EMP_001) : json(r, 'not found', 404);
  });
}

const node = (page: Page, title: string) =>
  page.locator('.manual-tree-node', { has: page.getByRole('link', { name: title, exact: true }) });

const treeTitles = (page: Page): Promise<string[]> =>
  page.locator('.manual-tree-label').allInnerTexts().then((ts) => ts.map((t) => t.trim()));

const storedCollapsed = (page: Page): Promise<string | null> =>
  page.evaluate((k) => localStorage.getItem(k), COLLAPSED_KEY);

/// parseRoute reads `window.location.search` for two routes; this is
/// Node, so give it the one field it reads (interaction-crawl does the
/// same).
function route(path: string): ReturnType<typeof parseRoute> {
  (globalThis as { window?: unknown }).window = { location: { search: '', pathname: path } };
  return parseRoute(path);
}

test.describe('/manual — the page chrome and its two spellings', () => {
  for (const path of ['/ux/manual', ROUTE_CATALOG.manual.path]) {
    test(`${path} renders the header and the no-section prompt`, async ({ page }) => {
      await installManualReads(page);
      await mountPage(page, path, { titleMatch: /Company manual/ });

      await expect(page.locator('.exec-eyebrow')).toHaveText('Know');
      await expect(page.locator('h1').first()).toHaveText('Company manual');
      await expect(page.getByText('Authored by HR. Every change is versioned.')).toBeVisible();
      await expect(page.locator('.manual-content')).toHaveText(
        'Select a section from the tree to start reading.',
      );
      await expect(page.locator('.manual-tree-label')).toHaveCount(SECTIONS.length);
    });
  }

  test('the tree says it is loading until its read answers', async ({ page }) => {
    await installManualReads(page);
    let release: () => void = () => {};
    const held = new Promise<void>((r) => { release = r; });
    await page.route(TREE, async (r) => { await held; await json(r, SECTIONS); });
    await mountPage(page, '/ux/manual');

    await expect(page.locator('.manual-tree')).toHaveText('Loading tree…');
    release();
    await expect(page.locator('.manual-tree-label')).toHaveCount(SECTIONS.length);
  });

  // CURRENT, gap 1 (ee688ba9): the catalog row exists, but nothing in
  // the app links to it — the URL has to be typed. The smoke persona's
  // session carries no role, and a role-less sidebar renders no rows at
  // all, so this leg signs in a role that sees every surface; the
  // control is the Home sidebar's My Day row, on the same page.
  test('CURRENT, gap 1: no link anywhere in the app shell leads to the manual', async ({ page }) => {
    await installManualReads(page);
    await page.route(/\/api\/session$/, (r) =>
      json(r, { username: 'david', employee_id: 'emp-001', role: 'platform-admin' }));
    await mountPage(page, '/ux/manual', { titleMatch: /Company manual/ });

    await expect(page.locator('.shell-nav').getByRole('link', { name: 'My Day', exact: true }).first()).toBeVisible();
    await expect(page.locator('a[href="/manual"], a[href="/ux/manual"]')).toHaveCount(0);
  });
});

test.describe('/manual — tree links', () => {
  test('every section is one link, in sort order, indented by depth', async ({ page }) => {
    await installManualReads(page);
    await mountPage(page, '/ux/manual', { titleMatch: /Company manual/ });

    await expect.poll(() => treeTitles(page)).toEqual(TREE_ORDER.map(([t]) => t));
    for (const [title, depth] of TREE_ORDER) {
      await expect(node(page, title)).toHaveCSS('padding-left', `${10 + depth * 14}px`);
    }
  });

  test('each link lands on the catalogued manual surface, at its own section', async ({ page }) => {
    await installManualReads(page);
    await mountPage(page, '/ux/manual', { titleMatch: /Company manual/ });

    // The surface the links belong to is the catalog's `manual` row.
    expect(route(ROUTE_CATALOG.manual.path)).toEqual({ kind: 'manual' });
    const hrefs = await page.locator('.manual-tree-label').evaluateAll((as) =>
      as.map((a) => a.getAttribute('href')),
    );
    expect(hrefs.length).toBe(SECTIONS.length);
    for (const s of SECTIONS) {
      const link = `/ux${ROUTE_CATALOG.manual.path}/${s.slug}`;
      expect(hrefs).toContain(link);
      expect(route(link)).toEqual({ kind: 'manualSection', slug: s.slug });
    }
  });

  test('a link opens its section, marks it active, and back returns to the page it came from', async ({ page }) => {
    await installManualReads(page);
    await mountPage(page, '/ux/manual', { titleMatch: /Company manual/ });

    await page.getByRole('link', { name: 'Welcome', exact: true }).click();
    await expect(page).toHaveURL(/\/ux\/manual\/welcome$/);
    const article = page.locator('article.manual-article');
    await expect(article.locator('.manual-article-eyebrow')).toHaveText('welcome');
    await expect(article.locator('h2')).toHaveText('Welcome');
    await expect(article.locator('.manual-article-meta')).toHaveText('Version 2 · updated Sep 16, 2026');
    await expect(node(page, 'Welcome')).toHaveClass(/manual-tree-active/);

    await page.getByRole('link', { name: 'Sick leave', exact: true }).click();
    await expect(page).toHaveURL(/\/ux\/manual\/benefits\/time-off\/sick$/);
    await expect(article.locator('h2')).toHaveText('Sick leave');
    await expect(article.locator('.manual-article-eyebrow')).toHaveText('benefits/time-off/sick');
    await expect(article.locator('.manual-article-meta')).toHaveText('Version 1 · updated Sep 16, 2026');
    await expect(node(page, 'Sick leave')).toHaveClass(/manual-tree-active/);
    await expect(node(page, 'Welcome')).not.toHaveClass(/manual-tree-active/);

    await page.goBack();
    await expect(page).toHaveURL(/\/ux\/manual\/welcome$/);
    await expect(article.locator('h2')).toHaveText('Welcome');

    await page.goBack();
    await expect(page).toHaveURL(/\/ux\/manual$/);
    await expect(page.locator('.manual-content')).toHaveText(
      'Select a section from the tree to start reading.',
    );
  });

  test('a section says it is loading until its read answers', async ({ page }) => {
    await installManualReads(page);
    let release: () => void = () => {};
    const held = new Promise<void>((r) => { release = r; });
    await page.route(SECTION, async (r) => { await held; await json(r, SECTIONS[1]); });
    await mountPage(page, '/ux/manual/welcome', { titleMatch: /Company manual/ });

    await expect(page.locator('.manual-content')).toHaveText('Loading…');
    release();
    await expect(page.locator('article.manual-article h2')).toHaveText('Welcome');
  });

  test('a section the server does not have says so, naming the slug', async ({ page }) => {
    await installManualReads(page);
    await mountPage(page, '/ux/manual/policies/missing', { titleMatch: /Company manual/ });

    await expect(page.locator('.manual-content')).toHaveText(
      "Section policies/missing not found, or you don't have access to it.",
    );
    await expect(page.locator('.manual-content code')).toHaveText('policies/missing');
  });
});

test.describe('/manual — tree toggles', () => {
  test('a parent toggle collapses and expands its children, and says which it will do', async ({ page }) => {
    await installManualReads(page);
    await mountPage(page, '/ux/manual', { titleMatch: /Company manual/ });

    const toggle = node(page, 'Policies').locator('button.manual-tree-toggle');
    await expect(toggle).toHaveAttribute('aria-label', 'Collapse');
    await expect(toggle).toHaveText('▾');

    await toggle.click();
    await expect(toggle).toHaveAttribute('aria-label', 'Expand');
    await expect(toggle).toHaveText('▸');
    await expect.poll(() => treeTitles(page)).toEqual(['Welcome', 'Policies', 'Benefits', 'Time off', 'Sick leave']);
    // The toggle is not a link: collapsing moves nobody.
    await expect(page).toHaveURL(/\/ux\/manual$/);

    await toggle.click();
    await expect(toggle).toHaveAttribute('aria-label', 'Collapse');
    await expect.poll(() => treeTitles(page)).toEqual(TREE_ORDER.map(([t]) => t));
  });

  test('collapsing a parent hides its whole subtree, not only its children', async ({ page }) => {
    await installManualReads(page);
    await mountPage(page, '/ux/manual', { titleMatch: /Company manual/ });

    await node(page, 'Benefits').locator('button.manual-tree-toggle').click();
    await expect.poll(() => treeTitles(page)).toEqual(['Welcome', 'Policies', 'Security', 'Conduct', 'Benefits']);
  });

  test('the collapse set survives a reload, in this viewer’s browser', async ({ page }) => {
    await installManualReads(page);
    await mountPage(page, '/ux/manual', { titleMatch: /Company manual/ });

    await node(page, 'Policies').locator('button.manual-tree-toggle').click();
    await expect.poll(() => storedCollapsed(page)).toBe('["policies"]');

    await page.reload();
    await expect(page.locator('.manual-tree-label').first()).toBeVisible();
    await expect(node(page, 'Policies').locator('button.manual-tree-toggle')).toHaveAttribute('aria-label', 'Expand');
    await expect.poll(() => treeTitles(page)).toEqual(['Welcome', 'Policies', 'Benefits', 'Time off', 'Sick leave']);
  });

  test('opening a section expands its collapsed ancestors, and only those', async ({ page }) => {
    await page.addInitScript((k) => {
      localStorage.setItem(k, JSON.stringify(['benefits', 'benefits/time-off', 'policies']));
    }, COLLAPSED_KEY);
    await installManualReads(page);
    await mountPage(page, '/ux/manual/benefits/time-off/sick', { titleMatch: /Company manual/ });

    await expect(node(page, 'Sick leave')).toHaveClass(/manual-tree-active/);
    await expect.poll(() => treeTitles(page)).toEqual(['Welcome', 'Policies', 'Benefits', 'Time off', 'Sick leave']);
    await expect.poll(() => storedCollapsed(page)).toBe('["policies"]');
  });

  // CURRENT, gap 8 (e703e1af): every leaf renders a toggle button with
  // no text and no accessible name, and clicking it does nothing. With
  // no glyph it collapses to zero height, so a pointer cannot reach it
  // (Playwright calls it not visible) — but it is still a button, and
  // still a keyboard tab stop with nothing to announce. The click is
  // dispatched for that reason.
  test('CURRENT, gap 8: a leaf renders an unlabelled toggle that does nothing', async ({ page }) => {
    await installManualReads(page);
    await mountPage(page, '/ux/manual', { titleMatch: /Company manual/ });

    await expect(page.locator('button.manual-tree-toggle')).toHaveCount(SECTIONS.length);
    for (const s of PARENTS) {
      const title = SECTIONS.find((x) => x.slug === s)!.title;
      await expect(node(page, title).locator('button.manual-tree-toggle')).toHaveAttribute('aria-label', 'Collapse');
    }
    for (const s of LEAVES) {
      const title = SECTIONS.find((x) => x.slug === s)!.title;
      const toggle = node(page, title).locator('button.manual-tree-toggle');
      await expect(toggle).toHaveText('');
      expect(await toggle.getAttribute('aria-label')).toBeNull();
      await toggle.focus();
      await expect(toggle).toBeFocused();
      await toggle.dispatchEvent('click');
      await expect(page).toHaveURL(/\/ux\/manual$/);
    }
    expect(await storedCollapsed(page)).toBeNull();
    await expect.poll(() => treeTitles(page)).toEqual(TREE_ORDER.map(([t]) => t));
  });
});

test.describe('/manual — the section body', () => {
  test('an employee shortcode is a link to that employee, labelled by name, and back returns', async ({ page }) => {
    await installManualReads(page);
    await mountPage(page, '/ux/manual/welcome', { titleMatch: /Company manual/ });

    const link = page.locator('.manual-article-body a');
    await expect(link).toHaveCount(1);
    await expect(link).toHaveText(EMP_001.name);
    await expect(link).toHaveAttribute('href', '/ux/people/emp-001');
    expect(route('/ux/people/emp-001')).toEqual({ kind: 'employee', empId: 'emp-001' });

    await link.click();
    await expect(page).toHaveURL(/\/ux\/people\/emp-001$/);
    await page.goBack();
    await expect(page).toHaveURL(/\/ux\/manual\/welcome$/);
    await expect(page.locator('article.manual-article h2')).toHaveText('Welcome');
  });

  // CURRENT, gap 4 (d1b6cfef): the seed says bodies are markdown; the
  // page renders them as plain text, so emphasis shows as underscores.
  test('CURRENT, gap 4: a markdown body renders as plain text, underscores and all', async ({ page }) => {
    await installManualReads(page);
    await mountPage(page, '/ux/manual/policies/security', { titleMatch: /Company manual/ });

    await expect(page.locator('.manual-article-body')).toHaveText('_TBD, owner: Operations lead._');
    await expect(page.locator('.manual-article-body em')).toHaveCount(0);
  });
});

test.describe('/manual — empty and failed reads are never the same paint', () => {
  test('an empty tree says "No sections yet." and is not a failure', async ({ page }) => {
    await installManualReads(page);
    await page.route(TREE, (r) => json(r, []));
    await mountPage(page, '/ux/manual', { titleMatch: /Company manual/ });

    await expect(page.locator('.manual-tree')).toHaveText('No sections yet.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('a failed tree read is said with its status, never drawn as an empty tree', async ({ page }) => {
    await installManualReads(page);
    await page.route(TREE, (r) => json(r, { error: 'content down' }, 503));
    await mountPage(page, '/ux/manual', { titleMatch: /Company manual/ });

    const failed = page.locator(`.manual-tree ${FAILURE_MARKER}`);
    await expect(failed).toHaveText("Couldn't load the manual — HTTP 503");
    await expect(failed).toHaveAttribute('role', 'alert');
    await expect(page.getByText('No sections yet.')).toHaveCount(0);
  });

  test('an unreachable tree read is said too, with the error', async ({ page }) => {
    await installManualReads(page);
    await page.route(TREE, (r) => r.abort('connectionrefused'));
    await mountPage(page, '/ux/manual', { titleMatch: /Company manual/ });

    await expect(page.locator(`.manual-tree ${FAILURE_MARKER}`)).toContainText("Couldn't load the manual — ");
    await expect(page.getByText('No sections yet.')).toHaveCount(0);
  });

  // CURRENT, gap 2 (e8394d44), first half: a non-404 failure of the
  // section read paints a line with no status and no failure marker.
  test('CURRENT, gap 2: a failed section read on first load says "Unable to load section." and nothing more', async ({ page }) => {
    await installManualReads(page);
    await page.route(SECTION, (r) => json(r, { error: 'content down' }, 500));
    await mountPage(page, '/ux/manual/welcome', { titleMatch: /Company manual/ });

    await expect(page.locator('.manual-content')).toHaveText('Unable to load section.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  // CURRENT, gap 2 (e8394d44), second half: after the reader moves from
  // one section to another whose read fails, the FIRST section's article
  // stays on screen under the second's URL — false content.
  test('CURRENT, gap 2: a failed section read after navigating keeps the previous section on screen', async ({ page }) => {
    await installManualReads(page);
    await page.route(/\/api\/content\/manual\/policies\/security$/, (r) => json(r, { error: 'content down' }, 500));
    await mountPage(page, '/ux/manual/welcome', { titleMatch: /Company manual/ });
    await expect(page.locator('article.manual-article h2')).toHaveText('Welcome');

    await page.getByRole('link', { name: 'Security', exact: true }).click();
    await expect(page).toHaveURL(/\/ux\/manual\/policies\/security$/);
    await expect(node(page, 'Security')).toHaveClass(/manual-tree-active/);
    await expect(page.locator('article.manual-article h2')).toHaveText('Welcome');
    await expect(page.locator('.manual-article-eyebrow')).toHaveText('welcome');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  // Gap 3 (5013bef4), FIXED by backlog 1e73bd93: this test was
  // "CURRENT, gap 3: a failed roster read leaves the raw employee id and
  // says nothing". The shortcode still falls back to its id — an id is
  // the honest label when the name is unknown — but the page now says
  // the name read failed, and which read it was.
  test('a failed name read keeps the raw employee id and says so, naming the read', async ({ page }) => {
    await installManualReads(page);
    await page.route(PERSON, (r) => json(r, { error: 'people down' }, 503));
    await mountPage(page, '/ux/manual/welcome', { titleMatch: /Company manual/ });

    await expect(page.locator('article.manual-article h2')).toHaveText('Welcome');
    await expect(page.locator('.manual-article-body a')).toHaveText('emp-001');
    const failed = page.locator(`article.manual-article ${FAILURE_MARKER}`);
    await expect(failed).toHaveAttribute('role', 'alert');
    await expect(failed).toHaveText(
      "Couldn't load employee names — /api/people/emp-001: HTTP 503. Employees show as ids.",
    );
  });

  // 5013bef4's other half: 0 of the 17 live bodies carry a shortcode,
  // and the page read the whole roster on every mount anyway. The
  // roster half is pinned by the shortcode test above (the roster
  // fixture names emp-001 "Roster Copy"); the app shell's session still
  // reads the roster once, which is not this page's read, so this
  // counts the per-person reads only.
  test('a body with no shortcode reads no one, and a body with one reads only that person', async ({ page }) => {
    const personReads: string[] = [];
    page.on('request', (req) => {
      const path = new URL(req.url()).pathname;
      if (PERSON.test(path)) personReads.push(path);
    });
    await installManualReads(page);
    await mountPage(page, '/ux/manual/policies/security', { titleMatch: /Company manual/ });
    await expect(page.locator('article.manual-article h2')).toHaveText('Security');
    expect(personReads).toEqual([]);

    await page.getByRole('link', { name: 'Welcome', exact: true }).click();
    await expect(page.locator('.manual-article-body a')).toHaveText(EMP_001.name);
    expect([...new Set(personReads)]).toEqual(['/api/people/emp-001']);
  });
});

test.describe('/manual — writes', () => {
  // The inventory counts 0 writes, so there is no refusal to show: the
  // refused-write leg has nothing to refuse. What IS pinned is the zero
  // — a write added to this page without a refusal leg fails here
  // first. /api/surface-opens is the shell's open telemetry, silent by
  // design (interaction-crawl's SILENT_WRITES), and not the page's.
  test('the page issues no write while it is read, navigated and toggled', async ({ page }) => {
    const writes: string[] = [];
    page.on('request', (req) => {
      const url = new URL(req.url());
      if (req.method() !== 'GET' && url.pathname.startsWith('/api/') && url.pathname !== '/api/surface-opens') {
        writes.push(`${req.method()} ${url.pathname}`);
      }
    });
    await installManualReads(page);
    await mountPage(page, '/ux/manual', { titleMatch: /Company manual/ });

    await page.getByRole('link', { name: 'Welcome', exact: true }).click();
    await expect(page.locator('article.manual-article h2')).toHaveText('Welcome');
    await node(page, 'Policies').locator('button.manual-tree-toggle').click();
    await node(page, 'Welcome').locator('button.manual-tree-toggle').dispatchEvent('click');
    await page.getByRole('link', { name: 'Benefits', exact: true }).click();
    await expect(page.locator('article.manual-article h2')).toHaveText('Benefits');

    expect(writes).toEqual([]);
  });
});
