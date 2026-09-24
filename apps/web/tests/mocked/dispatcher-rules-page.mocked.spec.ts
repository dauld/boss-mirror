// /it/registry/rules — every control the page audit counted, pinned
// (page-audit 08a444bc, step `test`).
//
// The audit's inventory (controls_md, measured 2026-09-23 against
// c43c7d03): 4 kinds of link — the breadcrumb, "+ New rule" in the
// header, "+ New rule" inline in the empty state, and one rule-name
// link per row — 0 buttons, 0 forms, 1 read (`GET
// /api/dispatcher/rules`), 0 writes. Since then #586 turned "+ New
// rule" into a link to authoring guidance rather than a create form
// (backlog 7d9df2fe, design ff1c3615), and added one line of prose
// beside it; dispatcher-rule-new.mocked.spec.ts pins what that guidance
// says, and this spec pins the list page as it stands on origin/main
// after that change. Before this spec the route was reached only by the
// three crawls, and nothing asserted what a row says, how rows are
// ordered, what the header claims when the read fails, or where the
// links land.
//
// THIS SPEC PINS THE PAGE AS IT IS, INCLUDING FILED DEFECTS, and names
// the item at each one, so the car that fixes a gap flips the assertion
// named for it:
//   gap 1 (14371116) the header counted a failed read as "0 active
//                    rules" — FIXED: it says the count is unknown;
//   gap 2 (cae1a377) the failure line carried no FAILURE_MARKER — FIXED:
//                    it carries it and role=alert, and outage-crawl no
//                    longer holds the route in SILENT;
//   gap 4 (f9e34a2c) why/source/authored/when and authored_registry are
//                    received and not rendered;
//   gap 6 (0a98d93f) no registry tab bar — FIXED: the page renders under
//                    the registry tabs, with a Rules tab of its own;
//   gap 7 (3071e235) the editor the row links and "+ New rule" land on
//                    had no ROUTE_CATALOG path of its own — FIXED: it is
//                    catalogued as /it/registry/rules/:ruleName, the
//                    pattern surface-opens records it under.
// Gap 8 (a9c4ad40) asked for both throw branches of the page's one read
// to be pinned; the two failure tests at the bottom pin both, as the
// page renders them.
//
// The fixture is three rules read from infra/dispatcher/rules/ (their
// names, triggers and handlers as the files spell them), a tenant rule,
// and one row with no trigger — the third branch of describeTrigger,
// which no live row reaches. Versions and the tenant row are invented.
// Every live row has exactly one `do` step (65 of 65, measured); the
// tenant row is given two, so the column is seen to be a count.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { parseRoute } from '../../src/router';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { sectionForRoute } from '../../src/shell/sections';
import { routePattern } from '../../src/shell/surface-opens';

const PAGE = '/it/registry/rules';
const TITLE = 'Dispatcher rules';
const NEW_RULE = '/it/registry/rules/new';
/// The editor's catalog path: a pattern, spelled the way surface-opens
/// records every open of it (routePattern), so the usage read and the
/// page march name the same surface.
const EDITOR = '/it/registry/rules/:ruleName';
const CASCADE = '/it/registry/dispatcher';

/// The page's one read, as ruleAuthoring.ts's listActiveRules spells it —
/// anchored, so the editor's `/api/dispatcher/rules/<name>/versions` is
/// not it.
const RULES = /\/api\/dispatcher\/rules$/;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const ROWS = [
  {
    name: 'sensors-poll-every-5-minutes',
    schedule: { cadence: 'every-5-minutes', anchor_date: '2026-09-17' },
    when: null,
    do: [{ handler: 'sensor.poll', args: {} }],
    version: 1,
    status: 'active',
    why: 'A TIMER, the standing exemption — a why the page does not render.',
    authored: true,
    source: 'product',
  },
  {
    name: 'auto-park-on-gate-green',
    on_event: 'step.done.gate-verdict',
    when: 'metadata.verdict == "green-guard-the-page-does-not-render"',
    do: [{ handler: 'jobs.auto-park', args: {} }],
    version: 2,
    status: 'active',
    why: 'A CROSS-PROTOCOL REACTOR — a why the page does not render.',
    authored: true,
    source: 'product',
  },
  {
    name: 'tenant-reorder-on-low-stock',
    on_event: 'inventory.stock.low',
    when: null,
    do: [
      { handler: 'jobs.spawn', args: {} },
      { handler: 'notify.assignee', args: {} },
    ],
    version: 4,
    status: 'active',
    why: null,
    authored: false,
    source: 'tenant:algedonic',
  },
  {
    name: 'department-retros-weekly',
    schedule: { cadence: 'weekly', anchor_date: '2026-09-21' },
    when: null,
    do: [{ handler: 'retro.open', args: {} }],
    version: 7,
    status: 'active',
    why: 'A TIMER — a why the page does not render.',
    authored: true,
    source: 'product',
  },
  {
    name: 'a-rule-with-no-trigger',
    when: null,
    do: [{ handler: 'jobs.spawn', args: {} }],
    version: 1,
    status: 'active',
    why: null,
    authored: false,
    source: 'product',
  },
];

/// The row names in the order the page must paint them: by name.
const SORTED = [...ROWS].map((r) => r.name).sort((a, b) => a.localeCompare(b));

const payload = (rules: unknown[]) => ({
  rules,
  handler_emits: { 'jobs.auto-park': ['jobs.car.filed'], 'sensor.poll': [] },
  system_edges: [],
  authored_registry: { dir: '/opt/boss/infra/dispatcher/rules-the-page-does-not-render', rules: 3, error: null },
});

async function install(page: Page, rules: unknown[] = ROWS): Promise<void> {
  await installSmokeMocks(page);
  await page.route(RULES, (r) => json(r, payload(rules)));
}

/// The shell's own write: App.svelte posts one surface-open per
/// navigation (shell/surface-opens.ts), on every route alike. It is
/// chrome, not a control of this page, so the page's write count
/// excludes it — and only it.
const SHELL_WRITE = /\/api\/surface-opens$/;

/// Every non-GET issued from the moment it is installed.
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

const catalogPaths = (): Set<string> =>
  new Set(Object.values(ROUTE_CATALOG).map((r) => (r as { path: string }).path));

/// The section a path lights, when that section is a ROUTE_CATALOG key.
/// Read from nav-catalog.ts and sections.ts, never from a second list.
function catalogued(path: string): string | null {
  const section = sectionForRoute(parseRoute(path));
  return section in ROUTE_CATALOG ? section : null;
}

const SUBTITLE = (n: string) => `${n} — the side-effect wiring boss-dispatcher runs`;
/// The header under a failed read (backlog 14371116), in the words the
/// step-plugins page uses for the same state.
const COUNT_UNKNOWN = 'Rule count unknown — the registry read failed';
const GUIDANCE =
  "A rule lasts when it is written down: a file under infra/dispatcher/rules/, or a tenant's seeds/rules.toml.";
const EMPTY = 'No active dispatcher rules. A rule is authored as a file or a tenant seed — + New rule says how.';

async function backHere(page: Page, rows: number): Promise<void> {
  await page.goBack();
  await expect(page).toHaveURL((u) => u.pathname === PAGE);
  await expect(page.locator('h1').first()).toHaveText(TITLE);
  await expect(page.locator('.catalog tbody tr')).toHaveCount(rows);
}

test.describe('/it/registry/rules — the rows', () => {
  test('header, guidance line, and one row per rule sorted by name with four columns', async ({ page }) => {
    await install(page);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const header = page.locator('.catalog header.exec-header');
    await expect(header.locator('.exec-eyebrow')).toHaveText('Platform · Dispatcher rules');
    await expect(header.locator('h1')).toHaveText(TITLE);
    await expect(header.locator('p')).toHaveText(SUBTITLE('5 active rules'));
    await expect(page.locator('.catalog').getByText(GUIDANCE)).toHaveCount(1);

    const section = page.locator('.catalog section.tab-section');
    await expect(section).toHaveCount(1);
    expect((await section.locator('h3').textContent())?.trim()).toBe('Active rules');
    const heads = await section.locator('thead th').allTextContents();
    expect(heads.map((h) => h.trim())).toEqual(['Rule', 'Trigger', 'Do steps', 'Version']);

    await expect(page.locator('.catalog tbody tr td:first-child')).toHaveText(SORTED);
    const row = (name: string) => page.locator('.catalog tbody tr').filter({ hasText: name });
    // describeTrigger's three branches: an event, a schedule (both cadence
    // spellings), and neither.
    await expect(row('auto-park-on-gate-green').locator('td')).toHaveText([
      'auto-park-on-gate-green', 'on step.done.gate-verdict', '1', '2',
    ]);
    await expect(row('sensors-poll-every-5-minutes').locator('td')).toHaveText([
      'sensors-poll-every-5-minutes', 'every 5 minutes · from 2026-09-17', '1', '1',
    ]);
    await expect(row('department-retros-weekly').locator('td')).toHaveText([
      'department-retros-weekly', 'every week · from 2026-09-21', '1', '7',
    ]);
    await expect(row('tenant-reorder-on-low-stock').locator('td')).toHaveText([
      'tenant-reorder-on-low-stock', 'on inventory.stock.low', '2', '4',
    ]);
    await expect(row('a-rule-with-no-trigger').locator('td')).toHaveText([
      'a-rule-with-no-trigger', 'no trigger recorded', '1', '1',
    ]);

    // GAP 4 (backlog f9e34a2c), pinned as it stands: the read carries
    // each rule's why, source, authored flag and when-guard, and the
    // authored registry's health, and the page shows none of them — so a
    // tenant rule and a live-only product rule paint like a tree rule.
    // The fixing car flips these counts.
    await expect(page.getByText('a why the page does not render')).toHaveCount(0);
    await expect(page.getByText('green-guard-the-page-does-not-render')).toHaveCount(0);
    await expect(page.getByText('tenant:algedonic')).toHaveCount(0);
    await expect(page.getByText('rules-the-page-does-not-render')).toHaveCount(0);
    await expect(page.getByText('jobs.auto-park')).toHaveCount(0);

    // GAP 6 (backlog 0a98d93f), fixed: the page carries the registry
    // tab bar its siblings carry, with a Rules tab of its own lit — it
    // was rendered bare, and 0 of 784 surface-opens reached it.
    await expect(page.locator('nav.it-tabs')).toHaveCount(1);
    await expect(page.locator('nav.it-tabs[aria-label="IT registry"] a[aria-current="page"]')).toHaveText('Rules');
  });

  test('one rule says rule in the singular', async ({ page }) => {
    await install(page, [ROWS[1]!]);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });
    await expect(page.locator('.catalog header.exec-header p')).toHaveText(SUBTITLE('1 active rule'));
  });

  test('while the read is in flight the header says Loading… and nothing else claims a state', async ({ page }) => {
    await installSmokeMocks(page);
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route(RULES, async (r) => {
      await held;
      await json(r, payload(ROWS));
    });
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    await expect(page.locator('.catalog header.exec-header p')).toHaveText('Loading…');
    await expect(page.getByText('No active dispatcher rules')).toHaveCount(0);
    await expect(page.getByText('Failed to load')).toHaveCount(0);
    await expect(page.locator('.catalog table')).toHaveCount(0);

    release();
    await expect(page.locator('.catalog header.exec-header p')).toHaveText(SUBTITLE('5 active rules'));
    await expect(page.locator('.catalog tbody tr')).toHaveCount(ROWS.length);
  });
});

test.describe('/it/registry/rules — the controls', () => {
  test('one unfiltered read, no buttons, no forms, no writes, and one link per rule plus two', async ({ page }) => {
    const writes = watchWrites(page);
    const reads: string[] = [];
    page.on('request', (r) => {
      if (RULES.test(new URL(r.url()).pathname)) reads.push(r.url());
    });
    await install(page);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });
    await expect(page.locator('.catalog tbody tr')).toHaveCount(ROWS.length);

    expect(await settledReads(page, () => reads.length, 1)).toBe(1);
    expect(new URL(reads[0]!).search, 'the page reads every active rule, unfiltered').toBe('');
    await expect(page.locator('.catalog button')).toHaveCount(0);
    await expect(page.locator('.catalog form')).toHaveCount(0);
    // The breadcrumb, the header's "+ New rule", and the rows. The
    // empty state's inline "+ New rule" is absent while rules exist.
    await expect(page.locator('.catalog a')).toHaveCount(2 + ROWS.length);
    await expect(page.getByRole('link', { name: '+ New rule' })).toHaveCount(1);
    expect(writes.map((w) => `${w.method()} ${w.url()}`)).toEqual([]);
  });

  test('the registry tabs, Rules among them, land on catalogued routes, and back returns here', async ({ page }) => {
    // GAP 6 (backlog 0a98d93f): the tab strip is the way in. Before it
    // the only in-app entries were the cascade's "Edit rules →" link and
    // this page's own breadcrumb.
    const writes = watchWrites(page);
    await install(page);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const tabs = page.locator('nav.it-tabs[aria-label="IT registry"] a');
    await expect(tabs).toHaveText(['Workflows', 'Dispatcher', 'Rules', 'Step plugins', 'Policy', 'Subjects', 'Drift']);
    await expect(tabs.nth(2)).toHaveAttribute('aria-current', 'page');
    await expect(tabs.nth(2)).toHaveAttribute('href', PAGE);
    await expect(page.locator('nav.it-tabs button')).toHaveCount(0);
    const hrefs = await tabs.evaluateAll((as) => as.map((a) => a.getAttribute('href') ?? ''));
    for (const h of hrefs) {
      expect(catalogPaths().has(h), `${h} is a ROUTE_CATALOG path`).toBe(true);
      expect(catalogued(h), `${h} lights a catalogued section`).not.toBeNull();
    }

    for (const [i, h] of hrefs.entries()) {
      if (h === PAGE) continue;
      await tabs.nth(i).click();
      await expect(page).toHaveURL((u) => u.pathname === h);
      await backHere(page, ROWS.length);
    }
    expect(writes.map((w) => `${w.method()} ${w.url()}`)).toEqual([]);
  });

  test('the breadcrumb lands on the catalogued cascade, and back returns here', async ({ page }) => {
    await install(page);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const crumb = page.locator('.catalog a.breadcrumb');
    await expect(crumb).toHaveText('← Dispatcher cascade');
    await expect(crumb).toHaveAttribute('href', CASCADE);
    expect(catalogPaths().has(CASCADE), `${CASCADE} is a ROUTE_CATALOG path`).toBe(true);
    expect(catalogued(CASCADE)).toBe('system-dispatcher');

    await crumb.click();
    await expect(page).toHaveURL((u) => u.pathname === CASCADE);
    await expect(page.locator('h1').first()).toHaveText('Dispatcher rules — reactive cascade');
    await backHere(page, ROWS.length);
  });

  test('"+ New rule" lands on the authoring guidance, writes nothing, and back returns here', async ({ page }) => {
    const writes = watchWrites(page);
    await install(page);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const create = page.getByRole('link', { name: '+ New rule' });
    await expect(create).toHaveAttribute('href', NEW_RULE);
    // It is a link dressed as the page's primary button.
    await expect(create).toHaveClass(/wb-btn-primary/);
    expect(parseRoute(NEW_RULE)).toEqual({ kind: 'dispatcherRuleEdit', ruleName: 'new' });
    // GAP 7 (backlog 3071e235), closed: the router places the guidance,
    // it lights the cascade's section, and the surface it opens is the
    // editor's own ROUTE_CATALOG path — it used to share this list's.
    expect(routePattern(parseRoute(NEW_RULE), NEW_RULE)).toBe(EDITOR);
    expect(catalogPaths().has(EDITOR), `${EDITOR} is a ROUTE_CATALOG path (gap 7)`).toBe(true);
    expect(catalogued(NEW_RULE)).toBe('system-dispatcher');

    await create.click();
    await expect(page).toHaveURL((u) => u.pathname === NEW_RULE);
    await expect(page.locator('h1').first()).toHaveText('New dispatcher rule');
    await backHere(page, ROWS.length);
    expect(writes.map((w) => `${w.method()} ${w.url()}`)).toEqual([]);
  });

  test('each rule name links to its editor, and back returns here', async ({ page }) => {
    await install(page);
    const target = ROWS[1]!;
    await page.route(/\/api\/dispatcher\/rules\/auto-park-on-gate-green\/versions$/, (r) =>
      json(r, [{
        name: target.name, version: 2, status: 'active', on_event: target.on_event, when: null,
        do: target.do, delay: null, created_at: '2026-09-20T00:00:00Z',
      }]),
    );
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const rowLinks = page.locator('.catalog tbody a');
    await expect(rowLinks).toHaveCount(ROWS.length);
    const hrefs = await rowLinks.evaluateAll((as) => as.map((a) => a.getAttribute('href') ?? ''));
    expect(hrefs).toEqual(SORTED.map((n) => `${PAGE}/${encodeURIComponent(n)}`));
    for (const [i, h] of hrefs.entries()) {
      expect(parseRoute(h), h).toEqual({ kind: 'dispatcherRuleEdit', ruleName: SORTED[i] });
      // GAP 7 (backlog 3071e235), closed as above: placed, lights the
      // cascade's section, and opens the editor's own catalog path.
      expect(routePattern(parseRoute(h), h), h).toBe(EDITOR);
      expect(catalogPaths().has(EDITOR), `${EDITOR} is a ROUTE_CATALOG path (gap 7)`).toBe(true);
      expect(catalogued(h)).toBe('system-dispatcher');
    }

    await page.getByRole('link', { name: 'auto-park-on-gate-green', exact: true }).click();
    await expect(page).toHaveURL((u) => u.pathname === `${PAGE}/auto-park-on-gate-green`);
    await expect(page.locator('h1').first()).toHaveText('auto-park-on-gate-green');
    await expect(page.locator('header.exec-header p')).toHaveText('1 version · active v2');
    await backHere(page, ROWS.length);
  });
});

test.describe('/it/registry/rules — empty and failed are never the same paint', () => {
  test('an empty registry says so, links to the guidance, and shows no failure line', async ({ page }) => {
    await install(page, []);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const empty = page.locator('.catalog p.empty');
    await expect(empty).toHaveText(EMPTY);
    await expect(page.locator('.catalog header.exec-header p')).toHaveText(SUBTITLE('0 active rules'));
    await expect(page.locator('.catalog table')).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.getByText('Failed to load')).toHaveCount(0);

    // The breadcrumb, the header's "+ New rule", and the inline one.
    await expect(page.locator('.catalog a')).toHaveCount(3);
    const inline = empty.getByRole('link', { name: '+ New rule' });
    await expect(inline).toHaveAttribute('href', NEW_RULE);
    await inline.click();
    await expect(page).toHaveURL((u) => u.pathname === NEW_RULE);
    await expect(page.locator('h1').first()).toHaveText('New dispatcher rule');
    await page.goBack();
    await expect(page).toHaveURL((u) => u.pathname === PAGE);
    await expect(page.locator('.catalog p.empty')).toHaveText(EMPTY);
  });

  test('a refused read says "Failed to load" with the status, never the empty-state copy', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(RULES, (r) => r.fulfill({ status: 503, contentType: 'text/plain', body: 'dispatcher down' }));
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const failure = page.locator('.catalog p.empty');
    await expect(failure).toHaveText('Failed to load: HTTP 503: dispatcher down');
    await expect(page.getByText('No active dispatcher rules')).toHaveCount(0);
    await expect(page.locator('.catalog table')).toHaveCount(0);
    // The one action stays reachable under a failed read.
    await expect(page.getByRole('link', { name: '+ New rule' })).toHaveCount(1);

    // GAP 2 (backlog cae1a377), closed: the line carries the shared
    // FAILURE_MARKER and role=alert, so outage-crawl asserts this route
    // instead of holding it in SILENT.
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(1);
    await expect(page.locator('.catalog [role=alert]')).toHaveCount(1);
    await expect(page.locator(`.catalog ${FAILURE_MARKER}[role=alert]`)).toHaveText(
      'Failed to load: HTTP 503: dispatcher down',
    );

    // GAP 1 (backlog 14371116), closed: a failed read leaves the count
    // unknown, and the header says so rather than counting nothing as
    // "0 active rules" — the empty registry's words, asserted above.
    const subtitle = page.locator('.catalog header.exec-header p');
    await expect(subtitle).toHaveText(COUNT_UNKNOWN);
    await expect(subtitle).not.toContainText(/\d/);
  });

  test('a 200 whose body carries an error is a failure, not an empty registry', async ({ page }) => {
    // What the dispatcher answers when dispatcher_rules will not load:
    // HTTP 200, no rules, and `error` spelled `load dispatcher_rules: <e>`
    // (boss-dispatcher http.rs `rules`). listActiveRules throws it.
    await installSmokeMocks(page);
    await page.route(RULES, (r) =>
      json(r, {
        rules: [],
        handler_emits: {},
        system_edges: [],
        authored_registry: null,
        error: 'load dispatcher_rules: relation "dispatcher_rules" does not exist',
      }),
    );
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    await expect(page.locator('.catalog p.empty')).toHaveText(
      'Failed to load: load dispatcher_rules: relation "dispatcher_rules" does not exist',
    );
    await expect(page.getByText('No active dispatcher rules')).toHaveCount(0);
    await expect(page.locator('.catalog table')).toHaveCount(0);
    // GAPS 1 and 2 again, closed on this branch too: a marked line, and
    // a header that does not count what it could not read.
    await expect(page.locator(`.catalog ${FAILURE_MARKER}[role=alert]`)).toHaveCount(1);
    await expect(page.locator('.catalog header.exec-header p')).toHaveText(COUNT_UNKNOWN);
  });
});
