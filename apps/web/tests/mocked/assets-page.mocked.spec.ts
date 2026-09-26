// /ux/assets — "Assets" (department maintenance), every control and
// render state pinned as the page behaves TODAY (page audit 015c3935,
// step `test`).
//
// Before this spec the route was reached by the generic crawls only
// (route-smoke, outage-crawl, interaction-crawl), and _smokeMocks.ts
// holds no /api/assets fixture, so no mocked spec had ever rendered a
// row, the row link, a phase button, the overflow banner, a non-zero
// count or a failure line (gap 9, 3dd74dc0). The crawls saw `All (0)`,
// a search box, zero tiles and "No assets match."
//
// Two renders, because the live instance and the page disagree:
//   State A — the `equipment` module off (the live instance, whose
//             manifest has `equipment: false`): ModuleDisabled, one
//             button, no reads.
//   State B — the module on: AssetsList — 1 link per row plus the row
//             itself (same target), 1 `All` button + 1 per phase whose
//             summary count is > 0 (up to 5), 1 search input, 5 kanban
//             tiles, 1 conditional overflow banner; 2 reads, 0 writes.
//
// The page moved after `measure` read it (2026-09-23, tree c43c7d03):
// the refurb phases were retired from boss-assets (a8991c86), so
// PHASE_ORDER is five phases, not nine; the c3e4edcc sweep gave the
// failure line the shared `load-failed` marker and the header an
// "unavailable" title; and the row became a web-kit rowLink (2361ac45).
// This spec pins the page as it is on the base it was written against.
//
// Lines that pin a FILED gap's current behaviour name the gap. They are
// meant to be edited by the car that fixes it, so the fix shows up here
// as a changed expectation instead of a silently-passing one. Gap 1
// (835029e8, Maintenance has no protocol) has no line here: what the
// page should draw once one lands is that item's work, not a render.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import {
  installSmokeMocks, installTenantManifest, LIVE_MANIFEST_RECORDED_AT, MODULES_LIVE, MODULES_NONE,
} from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { moduleForRoute, sectionForRoute } from '../../src/shell/sections';
import { parseRoute } from '../../src/router';

const PATH = ROUTE_CATALOG.assets.path;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

// The list read and the summary read — and NOT /api/assets/{id}, which
// is the detail page's, reached by the row link.
const LIST = /\/api\/assets(\?|$)/;
const SUMMARY = /\/api\/assets\/summary$/;
const READ_PATHS: ReadonlyArray<string> = ['/api/assets', '/api/assets/summary'];

// ── Fixtures ────────────────────────────────────────────────────────
// One asset per phase the backend emits except `decommissioned`, so its
// tile reads 0 and its button is absent. A-103 is `registered`, which
// PHASE_ORDER omits (gap 4). A-101 has no warranty on record (gap 8)
// and A-100 carries an OEM serial the search does not read (gap 8).

const asset = (
  asset_id: string,
  sku: string | null,
  phase: string,
  holder_id: string | null,
  warranty_through: string | null,
  open_ticket_count: number,
  oem_serial: string | null,
) => ({
  asset_id, sku, phase,
  holder_kind: holder_id ? 'account' : null, holder_id,
  warranty_through, open_ticket_count,
  first_seen: '2026-01-02T00:00:00Z', last_event_at: '2026-09-20T10:00:00Z',
  oem_serial,
});
const ASSETS = [
  asset('A-100', 'SKU-FERM-01', 'installed', 'acct-1', '2027-01-31', 2, 'OEM-9001'),
  asset('A-101', 'SKU-FERM-01', 'received', null, null, 0, null),
  asset('A-102', 'SKU-BRITE-02', 'shipped', 'acct-2', '2026-10-15', 1, null),
  asset('A-103', null, 'registered', null, null, 0, null),
  asset('A-104', 'SKU-KEG-03', 'out-for-service', 'acct-1', '2026-12-01', 0, null),
];
const IDS = ASSETS.map((a) => a.asset_id);
const page_ = (data: ReadonlyArray<unknown>, total = data.length) => ({ data, total, limit: 500, offset: 0 });

const SUMMARY_BODY = {
  phase_counts: [
    { phase: 'registered', count: 1 }, { phase: 'received', count: 1 }, { phase: 'shipped', count: 1 },
    { phase: 'installed', count: 1 }, { phase: 'out-for-service', count: 1 }, { phase: 'decommissioned', count: 0 },
  ],
  total_systems: 5, in_field_count: 2, open_tickets_total: 3, warranty_expiring_30d: 1,
};

/// The five tiles, in PHASE_ORDER, as the page labels them.
const TILE_LABELS = ['Received', 'Shipped', 'Installed', 'In service', 'Decom.'];

/// The shell with every module on (installSmokeMocks' MODULES_ON) and the
/// two reads answered from the fixtures above; a spec re-routes either
/// after this to change it.
async function installAssets(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(LIST, (r) => json(r, page_(ASSETS)));
  await page.route(SUMMARY, (r) => json(r, SUMMARY_BODY));
}

/// The shell's own non-GET: App.svelte records every route open
/// (shell/surface-opens.ts). It is the chrome's write, not this page's.
const SHELL_WRITES: ReadonlySet<string> = new Set(['/api/surface-opens']);

/// Every read of the page's two paths (with its query), and every non-GET.
function watch(page: Page): { reads: string[]; writes: Request[] } {
  const seen = { reads: [] as string[], writes: [] as Request[] };
  page.on('request', (req) => {
    const url = new URL(req.url());
    if (!url.pathname.startsWith('/api/')) return;
    if (req.method() !== 'GET') {
      if (!SHELL_WRITES.has(url.pathname)) seen.writes.push(req);
      return;
    }
    if (READ_PATHS.includes(url.pathname)) seen.reads.push(`${url.pathname}${url.search}`);
  });
  return seen;
}

const body = (page: Page) => page.locator('.catalog');
const eyebrow = (page: Page) => body(page).locator('.exec-eyebrow');
const title = (page: Page) => body(page).locator('h1.exec-title');
const subtitle = (page: Page) => body(page).locator('header.exec-header p');
const tileLabels = (page: Page) => body(page).locator('.kanban .kanban-label');
const tileCounts = (page: Page) => body(page).locator('.kanban .kanban-count');
const filters = (page: Page) => body(page).locator('.catalog-filters').getByRole('button');
const filter = (page: Page, name: string) => body(page).locator('.catalog-filters').getByRole('button', { name, exact: true });
const search = (page: Page) => body(page).locator('.catalog-filters input[type=search]');
const rows = (page: Page) => body(page).locator('.list-section tbody tr');
const firstColumn = (page: Page) => body(page).locator('.list-section tbody tr td:first-child');
const listLine = (page: Page) => body(page).locator('.list-section p.empty');
const banner = (page: Page) => body(page).locator('.overflow-banner');

async function mountAssets(page: Page): Promise<void> {
  await mountPage(page, PATH);
  await expect(title(page)).toHaveText('5 tracked assets');
}

/// The route a link's href parses to, the sidebar section it lights (a
/// ROUTE_CATALOG entry), and the module that gates it — read from the
/// router and the catalog, never a second list. parseRoute reads
/// `window.location.search`; this is Node, so give it the one field it
/// reads (interaction-crawl does the same).
function landing(href: string): { kind: string; section: string; module: string | null } {
  (globalThis as { window?: unknown }).window = { location: { search: '', pathname: href } };
  const route = parseRoute(href);
  const section = sectionForRoute(route);
  expect(ROUTE_CATALOG, `${href} lights a catalogued section`).toHaveProperty(section);
  return { kind: route.kind, section, module: moduleForRoute(route)?.id ?? null };
}

// The live leg reads the recorded live manifest (equipment false), not a
// typed copy of the flag: that copy and MODULES_LIVE disagreed about the
// same instance once already (41454ce1).
test.describe('/ux/assets — State A, the equipment module off (the live instance)', () => {
  for (const [name, modules] of [
    ['a manifest listing no modules', MODULES_NONE],
    [`the live manifest recorded ${LIVE_MANIFEST_RECORDED_AT} (equipment = false)`, MODULES_LIVE],
  ] as const) {
    test(`${name} renders ModuleDisabled, and its one button goes home and back`, async ({ page }) => {
      const seen = watch(page);
      await installAssets(page);
      await installTenantManifest(page, modules, { inline: true });
      await mountPage(page, PATH);

      const notice = page.locator('.module-disabled');
      await expect(notice.locator('h1')).toHaveText('Not enabled for this tenant');
      await expect(notice.locator('strong')).toHaveText(ROUTE_CATALOG.assets.label);
      await expect(notice.locator('strong')).toHaveText('Assets');
      await expect(notice).toContainText(
        "The Assets module is turned off in this tenant's tenant.toml. The page exists in the platform — the active tenant just doesn't surface it.",
      );
      // Gap 2 (27ff23fa): the one instruction names a seed file under
      // examples/<tenant>, on an instance whose registries are the truth.
      await expect(notice).toContainText(
        'To enable: set equipment = true in examples/<tenant>/seeds/tenant.toml under [modules], redeploy, and the page comes back.',
      );
      await expect(notice.getByRole('button')).toHaveCount(1);
      await expect(notice.locator('a')).toHaveCount(0);
      await expect(page.locator('.kanban')).toHaveCount(0);

      await notice.getByRole('button', { name: 'Back to home' }).click();
      await expect.poll(() => new URL(page.url()).pathname).toBe('/');
      await expect(page.locator('.module-disabled')).toHaveCount(0);

      await page.goBack();
      await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
      await expect(page.locator('.module-disabled h1')).toHaveText('Not enabled for this tenant');

      expect(seen.reads, 'the gated page never reads').toEqual([]);
      expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
    });
  }

  // Gap 2 (27ff23fa): both Maintenance surfaces sit behind the one
  // `equipment` flag, so with it off the department has no page but the
  // generic jobs view. Read from the catalogue, the one list there is.
  test('every Maintenance surface in the catalogue is gated on equipment', () => {
    const maintenance = Object.values(ROUTE_CATALOG).filter((e) => e.app === 'maintenance');
    expect(maintenance.map((e) => e.id).sort()).toEqual(['assets', 'catalog']);
    for (const e of maintenance) expect(e.module, e.id).toBe('equipment');
    expect(MODULES_LIVE.equipment, `the live manifest recorded ${LIVE_MANIFEST_RECORDED_AT}`).toBe(false);
  });

  // With no inlined manifest (the fetch fallback) the page behind the
  // gate mounts first and makes its two reads before the notice replaces
  // it. A served page has the manifest inlined, so this is the
  // fallback's behaviour, not the live instance's.
  test('without an inlined manifest the page behind the gate reads once before the notice replaces it', async ({ page }) => {
    const seen = watch(page);
    await installAssets(page);
    await installTenantManifest(page, MODULES_LIVE);
    await mountPage(page, PATH);
    await expect(page.locator('.module-disabled h1')).toHaveText('Not enabled for this tenant');
    await expect(page.locator('.kanban')).toHaveCount(0);
    expect(await settledReads(page, () => seen.reads.length, 2)).toBe(2);
    expect([...seen.reads].sort()).toEqual(['/api/assets/summary', '/api/assets?limit=500']);
  });
});

test.describe('/ux/assets — State B, the module on: header, kanban, filters and the table', () => {
  test('mount makes the two reads once, writes nothing, and paints the header, the tiles, the filters and every row', async ({ page }) => {
    const seen = watch(page);
    await installAssets(page);
    await mountAssets(page);
    expect(await settledReads(page, () => seen.reads.length, 2)).toBe(2);
    expect([...seen.reads].sort()).toEqual(['/api/assets/summary', '/api/assets?limit=500']);

    await expect(eyebrow(page)).toHaveText('Assets');
    await expect(subtitle(page)).toHaveText('1 installed · 3 open tickets · 1 warranties expiring (30d)');

    await expect(tileLabels(page)).toHaveText(TILE_LABELS);
    await expect(tileCounts(page)).toHaveText(['1', '1', '1', '1', '0']);
    // The tiles carry no control.
    await expect(body(page).locator('.kanban button, .kanban a')).toHaveCount(0);
    // Gap 4 (3da7e008): the registered asset is in the total and in no
    // tile — the five tiles sum to 4 under a header of 5.
    await expect(tileLabels(page)).not.toContainText(['Registered']);

    await expect(body(page).locator('.filter-label')).toHaveText(['Search', 'Phase']);
    await expect(search(page)).toHaveAttribute('placeholder', 'Serial, sku…');
    await expect(search(page)).toHaveValue('');
    // A button per phase with a count above zero: Decom. (0) is absent,
    // and so — gap 4 again — is Registered (1).
    await expect(filters(page)).toHaveText(['All (5)', 'Received (1)', 'Shipped (1)', 'Installed (1)', 'In service (1)']);
    await expect(filter(page, 'All (5)')).toHaveAttribute('aria-pressed', 'true');

    await expect(body(page).locator('.list-section thead th')).toHaveText(
      ['BOSS ID', 'SKU', 'Phase', 'Holder', 'Warranty', 'Open SRs', 'Last event'],
    );
    await expect(firstColumn(page)).toHaveText(IDS);
    await expect(rows(page).nth(0).locator('td')).toHaveText(
      ['A-100', 'SKU-FERM-01', 'Installed', 'acct-1', '2027-01-31', '2', '2026-09-20T10:00:00Z'],
    );
    // Gap 8 (8429a34a): a null warranty_through prints "out" — an
    // expired warranty — where the record says none is known.
    await expect(rows(page).nth(1).locator('td')).toHaveText(
      ['A-101', 'SKU-FERM-01', 'Received', '—', 'out', '0', '2026-09-20T10:00:00Z'],
    );
    // The registered asset has a label, and a row under All.
    await expect(rows(page).nth(3).locator('td')).toHaveText(
      ['A-103', '—', 'Registered', '—', 'out', '0', '2026-09-20T10:00:00Z'],
    );
    await expect(rows(page).nth(4).locator('td').nth(2)).toHaveText('In service');

    await expect(banner(page)).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });

  test('each phase button narrows the table to its phase client-side; All restores every row', async ({ page }) => {
    const seen = watch(page);
    await installAssets(page);
    await mountAssets(page);
    expect(await settledReads(page, () => seen.reads.length, 2)).toBe(2);

    for (const [name, id] of [
      ['Received (1)', 'A-101'], ['Shipped (1)', 'A-102'], ['Installed (1)', 'A-100'], ['In service (1)', 'A-104'],
    ] as const) {
      await filter(page, name).click();
      await expect(filter(page, name)).toHaveAttribute('aria-pressed', 'true');
      await expect(filter(page, 'All (5)')).toHaveAttribute('aria-pressed', 'false');
      await expect(firstColumn(page)).toHaveText([id]);
      // Gap 4 (3da7e008): no button reaches the registered asset.
      await expect(firstColumn(page)).not.toContainText(['A-103']);
    }

    await filter(page, 'All (5)').click();
    await expect(filter(page, 'All (5)')).toHaveAttribute('aria-pressed', 'true');
    await expect(firstColumn(page)).toHaveText(IDS);

    // Client state: no read, no URL change, no write.
    expect(seen.reads).toHaveLength(2);
    expect(new URL(page.url()).pathname).toBe(PATH);
    expect(new URL(page.url()).search).toBe('');
    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });

  test('the search box filters on BOSS ID and SKU, combines with a phase, and does not read the OEM serial', async ({ page }) => {
    const seen = watch(page);
    await installAssets(page);
    await mountAssets(page);
    expect(await settledReads(page, () => seen.reads.length, 2)).toBe(2);

    await search(page).fill('sku-ferm');
    await expect(firstColumn(page)).toHaveText(['A-100', 'A-101']);
    await search(page).fill('a-104');
    await expect(firstColumn(page)).toHaveText(['A-104']);

    await search(page).fill('SKU-FERM');
    await filter(page, 'Received (1)').click();
    await expect(firstColumn(page)).toHaveText(['A-101']);
    await filter(page, 'All (5)').click();

    // Gap 8 (8429a34a): the placeholder says "Serial", and A-100's OEM
    // serial finds nothing — and the empty line is the same sentence as
    // an empty registry.
    await search(page).fill('OEM-9001');
    await expect(rows(page)).toHaveCount(0);
    await expect(listLine(page)).toHaveText('No assets match.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    await search(page).fill('');
    await expect(firstColumn(page)).toHaveText(IDS);
    expect(seen.reads).toHaveLength(2);
    expect(new URL(page.url()).search).toBe('');
  });
});

test.describe('/ux/assets — State B: every link lands on a catalogued route, and back', () => {
  test('the one link kind goes to the asset detail route, gated on the same module', () => {
    expect(landing(`${PATH}/A-100`)).toEqual({ kind: 'asset', section: 'assets', module: 'equipment' });
  });

  test('each row links its BOSS ID; following one lands on the asset and back returns to the page, unfiltered', async ({ page }) => {
    const seen = watch(page);
    await installAssets(page);
    await mountAssets(page);
    expect(await settledReads(page, () => seen.reads.length, 2)).toBe(2);

    const links = body(page).locator('.list-section tbody a');
    await expect(links).toHaveText(IDS);
    for (const [i, id] of IDS.entries()) {
      await expect(links.nth(i)).toHaveAttribute('href', `${PATH}/${id}`);
    }
    // The row link is the page's only anchor.
    await expect(body(page).locator('a')).toHaveCount(IDS.length);

    await filter(page, 'Shipped (1)').click();
    await body(page).getByRole('link', { name: 'A-102', exact: true }).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${PATH}/A-102`);
    await expect(page.locator('.kanban')).toHaveCount(0);

    // One click is one history entry: back is the list, not the list twice.
    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(title(page)).toHaveText('5 tracked assets');
    // The phase filter is component state, not in the URL: the remount
    // starts over on All (and re-reads both).
    await expect(filter(page, 'All (5)')).toHaveAttribute('aria-pressed', 'true');
    await expect(firstColumn(page)).toHaveText(IDS);
    expect(await settledReads(page, () => seen.reads.filter((r) => r.startsWith('/api/assets?')).length, 2)).toBe(2);
  });

  test('a click on a row off its link, or Enter on the focused row, opens that asset', async ({ page }) => {
    await installAssets(page);
    await mountAssets(page);

    const row = rows(page).nth(2);
    await expect(row).toHaveAttribute('role', 'link');
    await expect(row).toHaveAttribute('aria-label', 'Asset A-102');
    await expect(row).toHaveAttribute('tabindex', '0');

    await row.locator('td').nth(2).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${PATH}/A-102`);
    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(title(page)).toHaveText('5 tracked assets');

    await rows(page).nth(4).focus();
    await page.keyboard.press('Enter');
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${PATH}/A-104`);
    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
  });
});

test.describe('/ux/assets — State B: the overflow banner', () => {
  // Gap 7 (1382989d): the hint says the kanban sees only the window and
  // that phase or search reach the rest. The tiles read the summary (the
  // whole population), and both filters run over the loaded rows only —
  // /api/assets takes no phase or text parameter — so a button promising
  // 700 shows the one row the window holds.
  test('a capped list says how many of how many, and its hint is the one pinned here', async ({ page }) => {
    await installAssets(page);
    await page.route(LIST, (r) => json(r, page_(ASSETS, 1_200)));
    await page.route(SUMMARY, (r) =>
      json(r, {
        ...SUMMARY_BODY,
        phase_counts: [{ phase: 'received', count: 700 }, { phase: 'installed', count: 496 }, { phase: 'registered', count: 4 }],
        total_systems: 1_200,
      }),
    );
    await mountPage(page, PATH);
    await expect(title(page)).toHaveText('1,200 tracked assets');

    await expect(banner(page).locator('.text')).toHaveText(
      'Showing 5 of 1,200 devices loaded for the list below. Kanban + per-row counts only see this window. Narrow by phase or search to drill into the rest.',
    );
    await expect(banner(page)).toHaveAttribute('role', 'status');
    await expect(tileCounts(page)).toHaveText(['700', '0', '496', '0', '0']);

    await filter(page, 'Received (700)').click();
    await expect(firstColumn(page)).toHaveText(['A-101']);
  });
});

test.describe('/ux/assets — State B: empty, pending and failed reads', () => {
  test('an empty backend paints zero counts and the empty line, never a failure', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(LIST, (r) => json(r, page_([])));
    await page.route(SUMMARY, (r) =>
      json(r, { phase_counts: [], total_systems: 0, in_field_count: 0, open_tickets_total: 0, warranty_expiring_30d: 0 }),
    );
    await mountPage(page, PATH);
    await expect(title(page)).toHaveText('0 tracked assets');
    await expect(subtitle(page)).toHaveText('0 installed · 0 open tickets · 0 warranties expiring (30d)');
    await expect(tileCounts(page)).toHaveText(['0', '0', '0', '0', '0']);
    await expect(filters(page)).toHaveText(['All (0)']);
    await expect(rows(page)).toHaveCount(0);
    await expect(listLine(page)).toHaveText('No assets match.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(banner(page)).toHaveCount(0);
  });

  // What the crawls see: the smoke floor answers both reads `[]`. A bare
  // array is not the list envelope and normalises to an empty page
  // (data/paginated.ts), and — gap 6 (e1cb1ef3) — the summary is cast,
  // never checked, so a 200 of the wrong shape paints as data.
  test('the smoke floor ([] for both) paints the same empty page, with no failure line', async ({ page }) => {
    await installSmokeMocks(page);
    await mountPage(page, PATH);
    await expect(title(page)).toHaveText('0 tracked assets');
    await expect(filters(page)).toHaveText(['All (0)']);
    await expect(listLine(page)).toHaveText('No assets match.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('gap 6 (e1cb1ef3): a malformed summary beside a good list zeroes every count and says nothing', async ({ page }) => {
    await installAssets(page);
    await page.route(SUMMARY, (r) => json(r, []));
    await mountPage(page, PATH);
    // total_systems is missing, so the title falls back to the rows loaded.
    await expect(title(page)).toHaveText('5 tracked assets');
    await expect(subtitle(page)).toHaveText('0 installed · 0 open tickets · 0 warranties expiring (30d)');
    await expect(tileCounts(page)).toHaveText(['0', '0', '0', '0', '0']);
    await expect(filters(page)).toHaveText(['All (5)']);
    await expect(firstColumn(page)).toHaveText(IDS);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  // Unfiled when this was written (the audit's gap 5 names the failed
  // read, not the pending one): while the reads are in flight the header
  // counts "0 tracked assets" and every tile reads 0 above "Loading…".
  test('pending reads: the list says Loading…, and the header and tiles count zero meanwhile', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await installSmokeMocks(page);
    await page.route(LIST, async (r) => {
      await held;
      await json(r, page_(ASSETS));
    });
    await page.route(SUMMARY, async (r) => {
      await held;
      await json(r, SUMMARY_BODY);
    });
    await mountPage(page, PATH);
    await expect(listLine(page)).toHaveText('Loading…');
    await expect(title(page)).toHaveText('0 tracked assets');
    await expect(subtitle(page)).toHaveText('0 installed · 0 open tickets · 0 warranties expiring (30d)');
    await expect(tileCounts(page)).toHaveText(['0', '0', '0', '0', '0']);
    await expect(filters(page)).toHaveText(['All (0)']);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    release();
    await expect(title(page)).toHaveText('5 tracked assets');
    await expect(firstColumn(page)).toHaveText(IDS);
  });

  // The c3e4edcc sweep fixed half of gap 5 (93c1e5f1): the failure line
  // wears the shared marker and the header says "unavailable". The other
  // half stands: every tile and the All button still count 0 above it.
  // And gap 6 (e1cb1ef3): one read failing discards the other's good
  // answer, because both sit in one try.
  for (const [name, route, reason] of [
    ['the list read fails (503)', LIST, "Couldn't load assets: /api/assets?limit=500: HTTP 503"],
    ['the summary read fails (500)', SUMMARY, "Couldn't load assets: summary HTTP 500"],
  ] as const) {
    test(`${name}: the marked failure line names it, the header says unavailable, and no row is drawn`, async ({ page }) => {
      await installAssets(page);
      await page.route(route, (r) => json(r, { error: 'down' }, route === LIST ? 503 : 500));
      await mountPage(page, PATH);

      const failed = page.locator(FAILURE_MARKER);
      await expect(failed).toHaveText(reason);
      await expect(failed).toHaveAttribute('role', 'alert');
      await expect(page.locator(FAILURE_MARKER)).toHaveCount(1);
      await expect(eyebrow(page)).toHaveText('Assets');
      await expect(title(page)).toHaveText('Assets unavailable');
      await expect(subtitle(page)).toHaveText("Couldn't load the assets or their summary");
      await expect(rows(page)).toHaveCount(0);
      await expect(body(page).locator('.list-section').getByText('No assets match.')).toHaveCount(0);
      // Gap 5 (93c1e5f1), the unfixed half: zero counts above the failure.
      await expect(tileCounts(page)).toHaveText(['0', '0', '0', '0', '0']);
      await expect(filters(page)).toHaveText(['All (0)']);
    });
  }

  test('a network failure on the list read names the browser\'s message', async ({ page }) => {
    await installAssets(page);
    await page.route(LIST, (r) => r.abort('failed'));
    await mountPage(page, PATH);
    await expect(page.locator(FAILURE_MARKER)).toHaveText("Couldn't load assets: Failed to fetch");
    await expect(title(page)).toHaveText('Assets unavailable');
  });
});

test.describe('/ux/assets — State B: the page writes nothing', () => {
  // The page has 0 writes, so there is no refusal to show; this pins
  // that every control above is read-only against the mock.
  test('every control, used once, sends no non-GET', async ({ page }) => {
    const seen = watch(page);
    await installAssets(page);
    await mountAssets(page);
    for (const name of ['Received (1)', 'Shipped (1)', 'Installed (1)', 'In service (1)', 'All (5)']) {
      await filter(page, name).click();
    }
    await search(page).fill('A-10');
    await search(page).press('Enter');
    await expect(firstColumn(page)).toHaveText(IDS);
    expect(await settledReads(page, () => seen.reads.length, 2)).toBe(2);
    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });
});
