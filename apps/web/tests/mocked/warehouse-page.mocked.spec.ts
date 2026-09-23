// /ux/warehouse — "Inventory" (department warehouse), every control and
// render state pinned as the page behaves TODAY (page audit 7510b0c1,
// step `test`).
//
// Before this spec the route was reached by the generic crawls only
// (route-smoke, outage-crawl, interaction-crawl), under an all-modules
// manifest: 0 component tests named WarehousePage or any of its four
// paths (measure step, 2026-09-23). Nothing switched a tab, clicked a
// filter or a sort header and read what it left, submitted the Create
// PO form, failed ONE read of the three, or rendered the module-disabled
// notice the live instance actually shows — its manifest has
// `warehouse: false`, `parts: false`, `shipping: false` (read
// 2026-09-23 22:11Z).
//
// Two renders, because the live instance and the page disagree:
//   State A — the warehouse module off (the live instance): ModuleDisabled,
//             one button, no reads.
//   State B — the module on (the used-device-shop tenant.toml):
//             WarehousePage — 3 tabs; Overview 0 buttons + 1 link kind
//             (below-reorder SKU); Inventory 3 filter buttons + 7
//             clickable sort headers + 1 link kind (SKU); Receiving 2
//             fixed + 0–6 conditional filter buttons, 1 toggle, 1 form
//             (Vendor, Part SKU, Qty, Unit cost, Submit), 2 link kinds
//             (PO, vendor); 3 reads, 1 write.
//
// Lines that pin a FILED gap's current behaviour name the gap. They are
// meant to be edited by the car that fixes it, so the fix shows up here
// as a changed expectation instead of a silently-passing one. Gap 2
// (44dd9798, no warehouse protocol) has no line here: the page draws
// service projections, and what it should draw instead is the founder's
// decision, not a render to pin.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { installSmokeMocks, installTenantManifest, MODULES_LIVE, MODULES_ON } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { departmentJobsPath, ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { moduleForRoute, sectionForRoute } from '../../src/shell/sections';
import { parseRoute } from '../../src/router';

const PATH = ROUTE_CATALOG.warehouse.path;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const ITEMS = /\/api\/inventory\/items$/;
const ORDERS = /\/api\/inventory\/orders$/;
const STATUS = /\/api\/inventory\/warehouse-status$/;
const CREATE = /\/api\/inventory\/orders\/create$/;
const CATALOG_PARTS = /\/api\/catalog\/parts$/;
const READ_PATHS: ReadonlyArray<string> = [
  '/api/inventory/items', '/api/inventory/orders', '/api/inventory/warehouse-status',
];

// ── Fixtures ────────────────────────────────────────────────────────
// Every value in a sortable column is distinct, so each sort order is
// one answer, not a tie broken by arrival. Arrival order is NOT the
// page's default (severity) order. available = on_hand - allocated.

const item = (part_sku: string, bin: string, on_hand: number, allocated: number, reorder_point: number) => ({
  part_sku, bin, on_hand, allocated, reorder_point, reorder_qty: 10, trailing_90d_usage: 0,
});
const ITEMS_BODY = [
  item('ING-MALT-01', 'A-01', 500, 100, 200), //  400 > 200     → healthy
  item('ING-HOPS-01', 'A-02', 30, 0, 100), //     30 < 100/2    → critical
  item('PKG-CAN-01', 'B-01', 900, 900, 500), //   0 <= 0        → out
  item('SP-GASKET-01', 'C-01', 12, 2, 10), //     10 <= 10      → low
  item('CN-CIP-01', 'C-02', 40, 5, 20), //        35 > 20       → healthy
];

const po = (id: string, vendor: string, status: string, lines: ReadonlyArray<[string, number]>) => ({
  id, vendor, status, placed_on: '2026-09-01', expected_on: '2026-09-30', received_on: null,
  lines: lines.map(([part_sku, qty]) => ({ part_sku, qty, unit_cost_cents: 100, currency: 'USD' })),
});
// Five of the six statuses; `acknowledged` has none, so its button is absent.
const ORDERS_BODY = [
  po('PO-101', 'v-riverside', 'submitted', [['ING-HOPS-01', 200], ['PKG-CAN-01', 2000]]),
  po('PO-102', 'v-riverside', 'in-transit', [['ING-HOPS-01', 50]]),
  po('PO-103', 'v-canco', 'draft', [['PKG-CAN-01', 500]]),
  po('PO-104', 'v-maltings', 'received', [['ING-MALT-01', 1000]]),
  po('PO-105', 'v-maltings', 'closed', [['SP-GASKET-01', 5]]),
];

const lowRow = (part_sku: string, bin: string, on_hand: number, allocated: number, reorder_point: number) => ({
  part_sku, bin, on_hand, allocated, available: on_hand - allocated, reorder_point,
});
/// The used-device-shop shape: refurb WIP and ready-for-sale populated,
/// so the pipeline section and every subtitle segment render. The
/// below-reorder list is shorter than its count, as the server caps it.
const STATUS_BODY = {
  parts_stock: {
    total_skus: 5, total_on_hand: 482, total_allocated: 107, total_available: 375,
    below_reorder_count: 3,
    below_reorder_items: [lowRow('PKG-CAN-01', 'B-01', 900, 900, 500), lowRow('ING-HOPS-01', 'A-02', 30, 0, 100)],
  },
  inbound_pos: {
    total_open: 3, draft_count: 1, submitted_count: 1, acknowledged_count: 0, in_transit_count: 1,
    late_count: 1, arriving_this_week_count: 2, recent: [],
  },
  outbound_shipments: { label_created: 4, picked_up: 2, in_transit: 6, exception: 1, delivered_7d: 9, recent: [] },
  refurb_wip: { total_in_flight: 7, by_stage: [{ stage: 'intake', count: 3 }, { stage: 'diagnostics', count: 4 }] },
  ready_for_sale_count: 2,
  as_of: '2026-09-23T22:00:00Z',
};
/// The brewery shape: no refurb, nothing ready for sale, nothing below
/// reorder — the pipeline section hides and the subtitle drops two segments.
const STATUS_BREWERY = {
  ...STATUS_BODY,
  parts_stock: { ...STATUS_BODY.parts_stock, below_reorder_count: 0, below_reorder_items: [] },
  refurb_wip: { total_in_flight: 0, by_stage: [] },
  ready_for_sale_count: 0,
};

/// The Inventory tab's landing order: Status ascending, by severity
/// rank (out, critical, low, healthy), ties in arrival order.
const DEFAULT_SKUS = ['PKG-CAN-01', 'ING-HOPS-01', 'SP-GASKET-01', 'ING-MALT-01', 'CN-CIP-01'];

const flatPart = (part_sku: string, name: string) => ({
  part_sku, name, description: '', unit_price_cents: 1_000, currency: 'USD', lead_time_days: 5,
});

/// The shell with every module on (installSmokeMocks' MODULES_ON) and the
/// three reads answered from the fixtures above; a spec re-routes any one
/// of them after this to change it.
async function installWarehouse(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(ITEMS, (r) => json(r, ITEMS_BODY));
  await page.route(ORDERS, (r) => json(r, ORDERS_BODY));
  await page.route(STATUS, (r) => json(r, STATUS_BODY));
  // For the part page a SKU link lands on: a name to title it with.
  await page.route(CATALOG_PARTS, (r) =>
    json(r, [flatPart('ING-HOPS-01', 'Citra hops'), flatPart('PKG-CAN-01', '16oz can')]),
  );
}

/// The shell's own non-GET: App.svelte records every route open
/// (shell/surface-opens.ts). It is the chrome's write, not this page's.
const SHELL_WRITES: ReadonlySet<string> = new Set(['/api/surface-opens']);

/// Every read of the page's three paths, and every non-GET it sends.
function watch(page: Page): { reads: string[]; writes: Request[] } {
  const seen = { reads: [] as string[], writes: [] as Request[] };
  page.on('request', (req) => {
    const url = new URL(req.url());
    if (!url.pathname.startsWith('/api/')) return;
    if (req.method() !== 'GET') {
      if (!SHELL_WRITES.has(url.pathname)) seen.writes.push(req);
      return;
    }
    if (READ_PATHS.includes(url.pathname)) seen.reads.push(url.pathname);
  });
  return seen;
}

const body = (page: Page) => page.locator('.catalog');
const tab = (page: Page, name: string) => body(page).getByRole('tab', { name, exact: true });
const button = (page: Page, name: string | RegExp) =>
  body(page).getByRole('button', { name, exact: typeof name === 'string' });
const title = (page: Page) => body(page).locator('h1.exec-title');
const subtitle = (page: Page) => body(page).locator('header.exec-header p');
const filters = (page: Page) => body(page).locator('.catalog-filters').getByRole('button');
const rows = (page: Page) => body(page).locator('.list-section tbody tr');
const firstColumn = (page: Page) => body(page).locator('.list-section tbody tr td:first-child');
const listLine = (page: Page) => body(page).locator('.list-section p.empty');
const overviewLine = (page: Page) => body(page).locator('p.empty');
const poStatusLine = (page: Page) => body(page).locator('.list-section > div').first().locator('span');

async function mountWarehouse(page: Page): Promise<void> {
  await mountPage(page, PATH);
  await expect(title(page)).toHaveText('5 tracked SKUs');
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

/// The manifest as a SERVED page carries it: the gateway inlines it into
/// index.html as `window.__BOSS_TENANT_MANIFEST__` (5578e42d), so the
/// shell is `ready` before its first paint. The mocked dev-server does
/// not inline, so without this the route mounts WarehousePage while the
/// manifest is still loading — the fallback leg, pinned separately below.
async function inlineManifest(page: Page, modules: Readonly<Record<string, boolean>>): Promise<void> {
  await page.addInitScript((m) => {
    (globalThis as { __BOSS_TENANT_MANIFEST__?: unknown }).__BOSS_TENANT_MANIFEST__ = {
      display_name: 'Algedonic, LLC', tenant_id: 'algedonic', modules: m, labels: {},
    };
  }, modules);
}

/// The live manifest's three flags, read 2026-09-23 22:11Z.
const LIVE_FLAGS = { warehouse: false, parts: false, shipping: false };

test.describe('/ux/warehouse — State A, the warehouse module off (the live instance)', () => {
  for (const [name, modules] of [
    ['a manifest listing no modules', MODULES_LIVE],
    ['a manifest with warehouse, parts and shipping false', LIVE_FLAGS],
  ] as const) {
    test(`${name} renders ModuleDisabled, and its one button goes home and back`, async ({ page }) => {
      const seen = watch(page);
      await installWarehouse(page);
      await installTenantManifest(page, modules);
      await inlineManifest(page, modules);
      await mountPage(page, PATH);

      const notice = page.locator('.module-disabled');
      await expect(notice.locator('h1')).toHaveText('Not enabled for this tenant');
      await expect(notice.locator('strong')).toHaveText(ROUTE_CATALOG.warehouse.label);
      await expect(notice.locator('strong')).toHaveText('Inventory');
      await expect(notice).toContainText(
        "The Inventory module is turned off in this tenant's tenant.toml. The page exists in the platform — the active tenant just doesn't surface it.",
      );
      // Gap 1 (fa838818): the one instruction names examples/<tenant>,
      // and this instance's tenant is not in examples/.
      await expect(notice).toContainText(
        'To enable: set warehouse = true in examples/<tenant>/seeds/tenant.toml under [modules], redeploy, and the page comes back.',
      );
      await expect(notice.getByRole('button')).toHaveCount(1);
      await expect(notice.locator('a')).toHaveCount(0);
      await expect(page.getByRole('tab', { name: 'Overview' })).toHaveCount(0);

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

  // Gap 1 (a1fcee7b): with no inlined manifest (the fetch fallback) the
  // page behind the gate mounts first and makes its three reads before
  // the notice replaces it. A served page has the manifest inlined, so
  // this is the fallback's behaviour, not the live instance's.
  test('without an inlined manifest the page behind the gate reads once before the notice replaces it', async ({ page }) => {
    const seen = watch(page);
    await installWarehouse(page);
    await installTenantManifest(page, LIVE_FLAGS);
    await mountPage(page, PATH);
    await expect(page.locator('.module-disabled h1')).toHaveText('Not enabled for this tenant');
    await expect(page.getByRole('tab', { name: 'Overview' })).toHaveCount(0);
    expect(await settledReads(page, () => seen.reads.length, 3)).toBe(3);
    expect([...seen.reads].sort()).toEqual([...READ_PATHS].sort());
  });
});

test.describe('/ux/warehouse — State B, the module on: header, tabs and the Overview', () => {
  test('mount makes the three reads once, writes nothing, and paints the header, the tabs and every Overview section', async ({ page }) => {
    const seen = watch(page);
    await installWarehouse(page);
    await mountWarehouse(page);

    expect(await settledReads(page, () => seen.reads.length, 3)).toBe(3);
    expect([...seen.reads].sort()).toEqual([...READ_PATHS].sort());

    await expect(body(page).locator('.exec-eyebrow')).toHaveText('Warehouse');
    await expect(subtitle(page)).toHaveText('3 below reorder · 3 open POs · 7 refurb WIP · 2 ready for sale');

    // Three tabs, Overview selected; they are role=tab, so the Overview
    // carries no button at all.
    await expect(body(page).getByRole('tab')).toHaveText(['Overview', 'Inventory', 'Receiving']);
    await expect(tab(page, 'Overview')).toHaveAttribute('aria-selected', 'true');
    await expect(tab(page, 'Inventory')).toHaveAttribute('aria-selected', 'false');
    await expect(body(page).getByRole('button')).toHaveCount(0);

    await expect(body(page).locator('section.tab-section h3')).toHaveText([
      'Refurb pipeline · 7 in flight · 2 ready for sale',
      'Parts stock', 'Inbound POs', 'Outbound shipments',
      'Below reorder · showing 2 of 3',
    ]);
    const pipeline = body(page).locator('section.tab-section').first();
    await expect(pipeline.locator('div > div > div:first-child')).toHaveText(['intake', 'diagnostics']);
    await expect(pipeline.locator('div > div > div:last-child')).toHaveText(['3', '4']);

    const kv = (section: string) =>
      body(page).locator('section.tab-section', { has: page.locator('h3', { hasText: section }) }).locator('dl.kv');
    await expect(kv('Parts stock').locator('dt')).toHaveText(['Total SKUs', 'On hand', 'Allocated', 'Available', 'Below reorder']);
    await expect(kv('Parts stock').locator('dd')).toHaveText(['5', '482', '107', '375', '3']);
    await expect(kv('Inbound POs').locator('dt')).toHaveText(['Open', 'Draft', 'Submitted', 'In transit', 'Late', 'Arriving this week']);
    await expect(kv('Inbound POs').locator('dd')).toHaveText(['3', '1', '1', '1', '1', '2']);
    await expect(kv('Outbound shipments').locator('dt')).toHaveText(['Label created', 'Picked up', 'In transit', 'Exception', 'Delivered (7d)']);
    await expect(kv('Outbound shipments').locator('dd')).toHaveText(['4', '2', '6', '1', '9']);

    const below = body(page).locator('section.tab-section').last();
    await expect(below.locator('thead th')).toHaveText(['Part SKU', 'Bin', 'On hand', 'Allocated', 'Available', 'Reorder pt']);
    await expect(below.locator('tbody tr').nth(0).locator('td')).toHaveText(['PKG-CAN-01', 'B-01', '900', '900', '0', '500']);
    await expect(below.locator('tbody tr').nth(1).locator('td')).toHaveText(['ING-HOPS-01', 'A-02', '30', '0', '30', '100']);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });

  test('the brewery shape hides the refurb pipeline, drops two subtitle segments, and says nothing is below reorder', async ({ page }) => {
    await installWarehouse(page);
    await page.route(STATUS, (r) => json(r, STATUS_BREWERY));
    await mountWarehouse(page);
    await expect(subtitle(page)).toHaveText('0 below reorder · 3 open POs');
    await expect(body(page).locator('section.tab-section h3')).toHaveText([
      'Parts stock', 'Inbound POs', 'Outbound shipments', 'Below reorder',
    ]);
    await expect(overviewLine(page)).toHaveText('All SKUs at or above reorder point.');
    await expect(body(page).locator('table')).toHaveCount(0);
  });

  test('switching tabs is client state: no read, no URL change, no history entry', async ({ page }) => {
    const seen = watch(page);
    await installWarehouse(page);
    await mountWarehouse(page);
    expect(await settledReads(page, () => seen.reads.length, 3)).toBe(3);
    const depth = await page.evaluate(() => window.history.length);

    for (const name of ['Inventory', 'Receiving', 'Overview']) {
      await tab(page, name).click();
      await expect(tab(page, name)).toHaveAttribute('aria-selected', 'true');
      await expect(body(page).locator('[aria-selected="true"]')).toHaveCount(1);
    }
    expect(seen.reads).toHaveLength(3);
    expect(new URL(page.url()).pathname).toBe(PATH);
    expect(new URL(page.url()).search).toBe('');
    expect(await page.evaluate(() => window.history.length)).toBe(depth);
  });
});

test.describe('/ux/warehouse — State B: the Inventory tab', () => {
  test('paints three filter buttons, seven sort headers and every row in severity order', async ({ page }) => {
    await installWarehouse(page);
    await mountWarehouse(page);
    await tab(page, 'Inventory').click();

    await expect(body(page).locator('.filter-label')).toHaveText('Status');
    await expect(filters(page)).toHaveText(['All (5)', 'Critical / Out (2)', 'Low (1)']);
    await expect(body(page).locator('.filter-btn-active')).toHaveText('All (5)');
    // The three filters are the tab's only buttons; the headers are <th>.
    await expect(body(page).getByRole('button')).toHaveCount(3);

    // The default sort is Status ascending (severity rank), and says so.
    await expect(body(page).locator('thead th')).toHaveText([
      'Part SKU', 'Bin', 'On hand', 'Allocated', 'Available', 'Reorder pt', 'Status ↑',
    ]);
    await expect(body(page).locator('thead th[aria-sort]')).toHaveText('Status ↑');
    await expect(body(page).locator('thead th[aria-sort]')).toHaveAttribute('aria-sort', 'ascending');

    const CELLS = [
      ['PKG-CAN-01', 'B-01', '900', '900', '0', '500', 'out'],
      ['ING-HOPS-01', 'A-02', '30', '0', '30', '100', 'critical'],
      ['SP-GASKET-01', 'C-01', '12', '2', '10', '10', 'low'],
      ['ING-MALT-01', 'A-01', '500', '100', '400', '200', 'healthy'],
      ['CN-CIP-01', 'C-02', '40', '5', '35', '20', 'healthy'],
    ];
    for (const [i, cells] of CELLS.entries()) {
      await expect(rows(page).nth(i).locator('td')).toHaveText(cells);
    }
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('each filter button narrows the table to its set, client-side', async ({ page }) => {
    const seen = watch(page);
    await installWarehouse(page);
    await mountWarehouse(page);
    expect(await settledReads(page, () => seen.reads.length, 3)).toBe(3);
    await tab(page, 'Inventory').click();

    for (const [label, skus] of [
      ['Critical / Out (2)', ['PKG-CAN-01', 'ING-HOPS-01']],
      ['Low (1)', ['SP-GASKET-01']],
      ['All (5)', DEFAULT_SKUS],
    ] as const) {
      await button(page, label).click();
      await expect(body(page).locator('.filter-btn-active')).toHaveText(label);
      await expect(firstColumn(page), label).toHaveText([...skus]);
    }
    expect(seen.reads, 'filters are client-side').toHaveLength(3);
    expect(new URL(page.url()).search).toBe('');
  });

  test('each of the seven sort headers orders the rows by its column; a second click reverses', async ({ page }) => {
    await installWarehouse(page);
    await mountWarehouse(page);
    await tab(page, 'Inventory').click();
    const header = (label: string) => body(page).locator('thead th', { hasText: label });

    // A new column takes its default direction: ascending for text and
    // status, descending for the four numbers.
    for (const [label, dir, skus] of [
      ['Part SKU', 'ascending', ['CN-CIP-01', 'ING-HOPS-01', 'ING-MALT-01', 'PKG-CAN-01', 'SP-GASKET-01']],
      ['Bin', 'ascending', ['ING-MALT-01', 'ING-HOPS-01', 'PKG-CAN-01', 'SP-GASKET-01', 'CN-CIP-01']],
      ['On hand', 'descending', ['PKG-CAN-01', 'ING-MALT-01', 'CN-CIP-01', 'ING-HOPS-01', 'SP-GASKET-01']],
      ['Allocated', 'descending', ['PKG-CAN-01', 'ING-MALT-01', 'CN-CIP-01', 'SP-GASKET-01', 'ING-HOPS-01']],
      ['Available', 'descending', ['ING-MALT-01', 'CN-CIP-01', 'ING-HOPS-01', 'SP-GASKET-01', 'PKG-CAN-01']],
      ['Reorder pt', 'descending', ['PKG-CAN-01', 'ING-MALT-01', 'ING-HOPS-01', 'CN-CIP-01', 'SP-GASKET-01']],
      ['Status', 'ascending', DEFAULT_SKUS],
    ] as const) {
      await header(label).click();
      await expect(header(label), label).toHaveAttribute('aria-sort', dir);
      await expect(header(label)).toHaveText(`${label} ${dir === 'ascending' ? '↑' : '↓'}`);
      await expect(body(page).locator('thead th[aria-sort]')).toHaveCount(1);
      await expect(firstColumn(page), label).toHaveText([...skus]);
    }

    await header('Status').click();
    await expect(header('Status')).toHaveAttribute('aria-sort', 'descending');
    await expect(firstColumn(page)).toHaveText(['ING-MALT-01', 'CN-CIP-01', 'SP-GASKET-01', 'ING-HOPS-01', 'PKG-CAN-01']);

    // Keyboard: Enter on a focused header sorts too.
    await header('Bin').focus();
    await page.keyboard.press('Enter');
    await expect(header('Bin')).toHaveAttribute('aria-sort', 'ascending');
    expect(new URL(page.url()).search).toBe('');
  });
});

test.describe('/ux/warehouse — State B: every link lands on a catalogued route, and back', () => {
  test('the four link placements go to three routes; parts is module-gated, POs and vendors are not', async () => {
    expect(landing(`${ROUTE_CATALOG.parts.path}/ING-HOPS-01`)).toEqual({ kind: 'part', section: 'parts', module: 'parts' });
    expect(landing('/ux/purchase-orders/PO-101')).toEqual({ kind: 'po', section: 'vendors', module: null });
    expect(landing(`${ROUTE_CATALOG.vendors.path}/v-riverside`)).toEqual({ kind: 'vendor', section: 'vendors', module: null });
  });

  test('Overview: each below-reorder SKU links to its part; following one lands there and back returns to the Overview', async ({ page }) => {
    await installWarehouse(page);
    await mountWarehouse(page);
    const links = body(page).locator('section.tab-section').last().locator('tbody a');
    await expect(links).toHaveText(['PKG-CAN-01', 'ING-HOPS-01']);
    await expect(links.nth(0)).toHaveAttribute('href', `${ROUTE_CATALOG.parts.path}/PKG-CAN-01`);
    await expect(links.nth(1)).toHaveAttribute('href', `${ROUTE_CATALOG.parts.path}/ING-HOPS-01`);
    // The Overview's only link kind.
    await expect(body(page).locator('a')).toHaveCount(2);

    await links.nth(1).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${ROUTE_CATALOG.parts.path}/ING-HOPS-01`);
    await expect(page.locator('h1.detail-title')).toHaveText('Citra hops');

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(tab(page, 'Overview')).toHaveAttribute('aria-selected', 'true');
  });

  // Back from a link on the Inventory or Receiving tab lands on the
  // OVERVIEW: the tab is component state, not in the URL, so the
  // remount starts over (and re-reads).
  test('Inventory: every row links its SKU; back from the part returns to the page on the Overview tab', async ({ page }) => {
    const seen = watch(page);
    await installWarehouse(page);
    await mountWarehouse(page);
    expect(await settledReads(page, () => seen.reads.length, 3)).toBe(3);
    await tab(page, 'Inventory').click();

    const links = body(page).locator('.list-section tbody a');
    await expect(links).toHaveCount(5);
    for (const [i, sku] of DEFAULT_SKUS.entries()) {
      await expect(links.nth(i)).toHaveText(sku);
      await expect(links.nth(i)).toHaveAttribute('href', `${ROUTE_CATALOG.parts.path}/${sku}`);
    }
    await expect(body(page).locator('a')).toHaveCount(5);

    await body(page).getByRole('link', { name: 'PKG-CAN-01', exact: true }).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${ROUTE_CATALOG.parts.path}/PKG-CAN-01`);
    await expect(page.locator('h1.detail-title')).toHaveText('16oz can');

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(tab(page, 'Overview')).toHaveAttribute('aria-selected', 'true');
    // 3 at mount, 2 by the part page (it reads items and orders too), 3
    // again at the remount.
    expect(await settledReads(page, () => seen.reads.length, 8)).toBe(8);
  });

  test('Receiving: each row links its PO and its vendor; both land, and back returns', async ({ page }) => {
    await installWarehouse(page);
    await mountWarehouse(page);
    await tab(page, 'Receiving').click();

    const poLinks = body(page).locator('.list-section tbody td:nth-child(1) a');
    const vendorLinks = body(page).locator('.list-section tbody td:nth-child(2) a');
    await expect(poLinks).toHaveText(['PO-101', 'PO-102', 'PO-103']);
    await expect(vendorLinks).toHaveText(['v-riverside', 'v-riverside', 'v-canco']);
    for (const [i, id] of ['PO-101', 'PO-102', 'PO-103'].entries()) {
      await expect(poLinks.nth(i)).toHaveAttribute('href', `/ux/purchase-orders/${id}`);
    }
    for (const [i, id] of ['v-riverside', 'v-riverside', 'v-canco'].entries()) {
      await expect(vendorLinks.nth(i)).toHaveAttribute('href', `${ROUTE_CATALOG.vendors.path}/${id}`);
    }
    await expect(body(page).locator('a')).toHaveCount(6);

    await poLinks.nth(0).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe('/ux/purchase-orders/PO-101');
    await expect(page.locator('h1.detail-title')).toHaveText('v-riverside');
    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(tab(page, 'Overview')).toHaveAttribute('aria-selected', 'true');

    await tab(page, 'Receiving').click();
    await vendorLinks.nth(2).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${ROUTE_CATALOG.vendors.path}/v-canco`);
    // The mock floor holds no vendor rows, so the vendor page answers
    // its own not-found — the landing is what this proves.
    await expect(page.locator('h1')).toContainText(['Vendor not found']);
    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
  });

  // Gap 12 (481d7939): the department's packets are not linked from the
  // page — no link on any tab goes to the department jobs view.
  test('no link on any tab goes to the department jobs view', async ({ page }) => {
    await installWarehouse(page);
    await mountWarehouse(page);
    const hrefs: string[] = [];
    for (const name of ['Overview', 'Inventory', 'Receiving']) {
      await tab(page, name).click();
      await expect(tab(page, name)).toHaveAttribute('aria-selected', 'true');
      hrefs.push(...(await body(page).locator('a').evaluateAll((as) => as.map((a) => a.getAttribute('href') ?? ''))));
    }
    expect(hrefs).toHaveLength(2 + 5 + 6);
    expect(hrefs.filter((h) => h.startsWith(departmentJobsPath('warehouse').split('?')[0]!))).toEqual([]);
    for (const h of hrefs) {
      expect(h, h).toMatch(/^\/ux\/(parts|purchase-orders|vendors)\//);
    }
  });

  // Gap 13 (9db5097f): the SKU links go through the `parts` flag, a
  // second tenant flag, so warehouse on with parts off lands on the
  // module-disabled notice.
  test('with warehouse on and parts off, a SKU link lands on ModuleDisabled', async ({ page }) => {
    await installWarehouse(page);
    await installTenantManifest(page, { ...MODULES_ON, parts: false });
    await mountWarehouse(page);
    await body(page).locator('section.tab-section').last().getByRole('link', { name: 'PKG-CAN-01' }).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${ROUTE_CATALOG.parts.path}/PKG-CAN-01`);
    await expect(page.locator('.module-disabled strong')).toHaveText('Ingredients & parts');
  });
});

test.describe('/ux/warehouse — State B: the Receiving tab', () => {
  test('paints Open and All, one button per status present, the toggle, and the open POs', async ({ page }) => {
    await installWarehouse(page);
    await mountWarehouse(page);
    await tab(page, 'Receiving').click();

    await expect(body(page).locator('.filter-label')).toHaveText('PO status');
    // `acknowledged` has no PO, so no button: 2 fixed + 5 of the 6.
    await expect(filters(page)).toHaveText([
      'Open (3)', 'All (5)', 'draft (1)', 'submitted (1)', 'in transit (1)', 'received (1)', 'closed (1)',
    ]);
    await expect(body(page).locator('.filter-btn-active')).toHaveText('Open (3)');
    await expect(body(page).getByRole('button')).toHaveText([
      'Open (3)', 'All (5)', 'draft (1)', 'submitted (1)', 'in transit (1)', 'received (1)', 'closed (1)', 'Create PO',
    ]);

    await expect(body(page).locator('.list-section thead th')).toHaveText([
      'PO ID', 'Vendor', 'Status', 'Placed', 'Expected', 'Lines',
    ]);
    await expect(rows(page).nth(0).locator('td')).toHaveText([
      'PO-101', 'v-riverside', 'submitted', '2026-09-01', '2026-09-30', 'ING-HOPS-01 x200, PKG-CAN-01 x2000',
    ]);
    await expect(rows(page).nth(1).locator('td')).toHaveText([
      'PO-102', 'v-riverside', 'in transit', '2026-09-01', '2026-09-30', 'ING-HOPS-01 x50',
    ]);
    await expect(rows(page).nth(2).locator('td')).toHaveText([
      'PO-103', 'v-canco', 'draft', '2026-09-01', '2026-09-30', 'PKG-CAN-01 x500',
    ]);
    await expect(rows(page)).toHaveCount(3);
  });

  test('each filter button narrows the table to its set, client-side', async ({ page }) => {
    const seen = watch(page);
    await installWarehouse(page);
    await mountWarehouse(page);
    expect(await settledReads(page, () => seen.reads.length, 3)).toBe(3);
    await tab(page, 'Receiving').click();

    for (const [label, ids] of [
      ['All (5)', ['PO-101', 'PO-102', 'PO-103', 'PO-104', 'PO-105']],
      ['draft (1)', ['PO-103']],
      ['submitted (1)', ['PO-101']],
      ['in transit (1)', ['PO-102']],
      ['received (1)', ['PO-104']],
      ['closed (1)', ['PO-105']],
      ['Open (3)', ['PO-101', 'PO-102', 'PO-103']],
    ] as const) {
      await button(page, label).click();
      await expect(body(page).locator('.filter-btn-active')).toHaveText(label);
      await expect(firstColumn(page), label).toHaveText([...ids]);
    }
    expect(seen.reads, 'filters are client-side').toHaveLength(3);
    expect(new URL(page.url()).search).toBe('');
  });
});

test.describe('/ux/warehouse — State B: the Create PO form (the one write)', () => {
  async function openForm(page: Page): Promise<void> {
    await tab(page, 'Receiving').click();
    await button(page, 'Create PO').click();
    await expect(button(page, 'Cancel')).toBeVisible();
  }

  test('the toggle shows the form with its labels, defaults and options, and hides it again', async ({ page }) => {
    await installWarehouse(page);
    await mountWarehouse(page);
    await openForm(page);

    await expect(body(page).getByLabel('Vendor', { exact: true })).toHaveAttribute('placeholder', 'e.g. Riverside Malting');
    await expect(body(page).getByLabel('Vendor', { exact: true })).toHaveValue('');
    await expect(body(page).getByLabel('Part SKU', { exact: true }).locator('option')).toHaveText([
      'Select part...', 'ING-MALT-01', 'ING-HOPS-01', 'PKG-CAN-01', 'SP-GASKET-01', 'CN-CIP-01',
    ]);
    await expect(body(page).getByLabel('Qty', { exact: true })).toHaveValue('10');
    await expect(body(page).getByLabel('Qty', { exact: true })).toHaveAttribute('min', '1');
    await expect(body(page).getByLabel('Unit cost ($)', { exact: true })).toHaveValue('500');
    await expect(body(page).getByLabel('Unit cost ($)', { exact: true })).toHaveAttribute('min', '1');

    // Submit is disabled until both Vendor and Part SKU are set.
    await expect(button(page, 'Submit')).toBeDisabled();
    await body(page).getByLabel('Vendor', { exact: true }).fill('v-riverside');
    await expect(button(page, 'Submit')).toBeDisabled();
    await body(page).getByLabel('Part SKU', { exact: true }).selectOption('ING-HOPS-01');
    await expect(button(page, 'Submit')).toBeEnabled();

    await button(page, 'Cancel').click();
    await expect(button(page, 'Create PO')).toBeVisible();
    await expect(body(page).getByLabel('Vendor', { exact: true })).toHaveCount(0);
  });

  test('Submit sends one POST, says "PO {id} created", closes the form and re-reads all three', async ({ page }) => {
    const seen = watch(page);
    await installWarehouse(page);
    await page.route(CREATE, (r) => json(r, { id: 'PO-106' }));
    await mountWarehouse(page);
    expect(await settledReads(page, () => seen.reads.length, 3)).toBe(3);
    await openForm(page);

    await body(page).getByLabel('Vendor', { exact: true }).fill('v-riverside');
    await body(page).getByLabel('Part SKU', { exact: true }).selectOption('ING-HOPS-01');
    await body(page).getByLabel('Qty', { exact: true }).fill('25');
    await body(page).getByLabel('Unit cost ($)', { exact: true }).fill('12');
    await button(page, 'Submit').click();

    await expect(poStatusLine(page)).toHaveText('PO PO-106 created');
    await expect(poStatusLine(page)).toHaveCSS('color', 'rgb(22, 163, 74)');
    await expect(button(page, 'Create PO')).toBeVisible();
    await expect(body(page).getByLabel('Vendor', { exact: true })).toHaveCount(0);
    expect(await settledReads(page, () => seen.reads.length, 6)).toBe(6);

    expect(seen.writes).toHaveLength(1);
    const w = seen.writes[0]!;
    expect(`${w.method()} ${new URL(w.url()).pathname}`).toBe('POST /api/inventory/orders/create');
    expect(w.postDataJSON()).toEqual({
      vendor: 'v-riverside',
      lines: [{ part_sku: 'ING-HOPS-01', qty: 25, unit_cost_cents: 1200, currency: 'USD' }],
    });

    // Reopened: vendor, SKU and qty are reset; the unit cost is not.
    await button(page, 'Create PO').click();
    await expect(body(page).getByLabel('Vendor', { exact: true })).toHaveValue('');
    await expect(body(page).getByLabel('Part SKU', { exact: true })).toHaveValue('');
    await expect(body(page).getByLabel('Qty', { exact: true })).toHaveValue('10');
    await expect(body(page).getByLabel('Unit cost ($)', { exact: true })).toHaveValue('12');
  });

  test('a refused write shows the server\'s body in red beside the toggle, and the form stays filled', async ({ page }) => {
    await installWarehouse(page);
    await page.route(CREATE, (r) =>
      r.fulfill({ status: 422, contentType: 'text/plain', body: 'unknown vendor: v-nobody' }),
    );
    await mountWarehouse(page);
    await openForm(page);
    await body(page).getByLabel('Vendor', { exact: true }).fill('v-nobody');
    await body(page).getByLabel('Part SKU', { exact: true }).selectOption('PKG-CAN-01');
    await button(page, 'Submit').click();

    await expect(poStatusLine(page)).toHaveText('Error: unknown vendor: v-nobody');
    await expect(poStatusLine(page)).toHaveCSS('color', 'rgb(220, 38, 38)');
    await expect(button(page, 'Cancel')).toBeVisible();
    await expect(body(page).getByLabel('Vendor', { exact: true })).toHaveValue('v-nobody');
    await expect(body(page).getByLabel('Part SKU', { exact: true })).toHaveValue('PKG-CAN-01');
  });

  test('a network failure on the write shows the browser\'s message', async ({ page }) => {
    await installWarehouse(page);
    await page.route(CREATE, (r) => r.abort('failed'));
    await mountWarehouse(page);
    await openForm(page);
    await body(page).getByLabel('Vendor', { exact: true }).fill('v-riverside');
    await body(page).getByLabel('Part SKU', { exact: true }).selectOption('PKG-CAN-01');
    await button(page, 'Submit').click();
    await expect(poStatusLine(page)).toHaveText('Error: Failed to fetch');
  });

  // Gap 10 (aad31699): a quantity below 1 makes Submit a silent no-op —
  // the button stays enabled, nothing is sent and nothing is said.
  test('a quantity below 1 sends nothing and says nothing', async ({ page }) => {
    const seen = watch(page);
    await installWarehouse(page);
    await mountWarehouse(page);
    await openForm(page);
    await body(page).getByLabel('Vendor', { exact: true }).fill('v-riverside');
    await body(page).getByLabel('Part SKU', { exact: true }).selectOption('PKG-CAN-01');
    for (const qty of ['0', '-3']) {
      await body(page).getByLabel('Qty', { exact: true }).fill(qty);
      await expect(button(page, 'Submit')).toBeEnabled();
      await button(page, 'Submit').click();
    }
    await page.waitForTimeout(300);
    expect(seen.writes).toEqual([]);
    await expect(poStatusLine(page)).toHaveCount(0);
    await expect(button(page, 'Cancel')).toBeVisible();
  });

  // Gap 10 (aad31699): the unit cost is not guarded — 0 is sent.
  test('a unit cost of 0 is sent as 0 cents', async ({ page }) => {
    const seen = watch(page);
    await installWarehouse(page);
    await page.route(CREATE, (r) => json(r, { id: 'PO-107' }));
    await mountWarehouse(page);
    await openForm(page);
    await body(page).getByLabel('Vendor', { exact: true }).fill('v-riverside');
    await body(page).getByLabel('Part SKU', { exact: true }).selectOption('PKG-CAN-01');
    await body(page).getByLabel('Unit cost ($)', { exact: true }).fill('0');
    await button(page, 'Submit').click();
    await expect(poStatusLine(page)).toHaveText('PO PO-107 created');
    expect(seen.writes).toHaveLength(1);
    expect(seen.writes[0]!.postDataJSON().lines[0].unit_cost_cents).toBe(0);
  });
});

test.describe('/ux/warehouse — State B: empty, loading, and a failed read', () => {
  test('an empty backend paints each tab\'s empty sentence, never a failure', async ({ page }) => {
    await installWarehouse(page);
    await page.route(ITEMS, (r) => json(r, []));
    await page.route(ORDERS, (r) => json(r, []));
    await page.route(STATUS, (r) =>
      json(r, {
        ...STATUS_BREWERY,
        parts_stock: { ...STATUS_BREWERY.parts_stock, total_skus: 0, total_on_hand: 0, total_allocated: 0, total_available: 0 },
        inbound_pos: { ...STATUS_BODY.inbound_pos, total_open: 0 },
      }),
    );
    await mountPage(page, PATH);
    await expect(title(page)).toHaveText('0 tracked SKUs');
    await expect(subtitle(page)).toHaveText('0 below reorder · 0 open POs');
    await expect(overviewLine(page)).toHaveText('All SKUs at or above reorder point.');

    await tab(page, 'Inventory').click();
    await expect(listLine(page)).toHaveText('No items match that filter.');
    await expect(filters(page)).toHaveText(['All (0)', 'Critical / Out (0)', 'Low (0)']);

    await tab(page, 'Receiving').click();
    await expect(listLine(page)).toHaveText('No POs match that filter.');
    await expect(filters(page)).toHaveText(['Open (0)', 'All (0)']);
    await expect(body(page).getByLabel('Vendor', { exact: true })).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  // Gap 5 (8b1deea2), gap 6 (82674b2b), gap 7 (20410830): while the
  // reads are in flight the header counts zeros, the filters count
  // zeros, and the Inventory and Receiving tabs say their empty
  // sentence — only the Overview has a loading line.
  test('pending reads: "Loading warehouse status…" on the Overview, the empty sentences on the other two', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await installWarehouse(page);
    for (const [re, fixture] of [[ITEMS, ITEMS_BODY], [ORDERS, ORDERS_BODY], [STATUS, STATUS_BODY]] as const) {
      await page.route(re, async (r) => {
        await held;
        await json(r, fixture);
      });
    }
    await mountPage(page, PATH);

    await expect(overviewLine(page)).toHaveText('Loading warehouse status…');
    await expect(title(page)).toHaveText('0 tracked SKUs');
    await expect(subtitle(page)).toHaveText('0 below reorder point');
    await tab(page, 'Inventory').click();
    await expect(listLine(page)).toHaveText('No items match that filter.');
    await expect(filters(page)).toHaveText(['All (0)', 'Critical / Out (0)', 'Low (0)']);
    await tab(page, 'Receiving').click();
    await expect(listLine(page)).toHaveText('No POs match that filter.');
    await expect(filters(page)).toHaveText(['Open (0)', 'All (0)']);

    release();
    await expect(title(page)).toHaveText('5 tracked SKUs');
    await expect(firstColumn(page)).toHaveText(['PO-101', 'PO-102', 'PO-103']);
  });

  // Gap 4 (fcd0e29e): one shared failure state — the items read failing
  // blanks the Receiving tab too, naming purchase orders, although they
  // loaded. Gap 6 (82674b2b): the Inventory filters count zeros beside
  // the alert.
  test('a failed items read: the Inventory alert, and the Receiving tab blanked with the wrong read named', async ({ page }) => {
    await installWarehouse(page);
    await page.route(ITEMS, (r) => json(r, { error: 'down' }, 503));
    await mountWarehouse(page);
    // The status read answered, so the header and the Overview stand.
    await expect(subtitle(page)).toHaveText('3 below reorder · 3 open POs · 7 refurb WIP · 2 ready for sale');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    await tab(page, 'Inventory').click();
    const failed = page.locator(FAILURE_MARKER);
    await expect(failed).toHaveText("Couldn't load inventory — HTTP 503");
    await expect(failed).toHaveAttribute('role', 'alert');
    await expect(body(page).locator('table')).toHaveCount(0);
    await expect(filters(page)).toHaveText(['All (0)', 'Critical / Out (0)', 'Low (0)']);

    await tab(page, 'Receiving').click();
    await expect(failed).toHaveText("Couldn't load purchase orders — HTTP 503");
    await expect(body(page).locator('table')).toHaveCount(0);
    await expect(filters(page).first()).toHaveText('Open (3)');
  });

  // Gap 4 (fcd0e29e) the other way round, and gap 6 (82674b2b) on the
  // Receiving filters.
  test('a failed orders read: the Receiving alert, and the Inventory tab blanked although items loaded', async ({ page }) => {
    await installWarehouse(page);
    await page.route(ORDERS, (r) => json(r, { error: 'down' }, 500));
    await mountWarehouse(page);

    await tab(page, 'Receiving').click();
    const failed = page.locator(FAILURE_MARKER);
    await expect(failed).toHaveText("Couldn't load purchase orders — HTTP 500");
    await expect(failed).toHaveAttribute('role', 'alert');
    await expect(filters(page)).toHaveText(['Open (0)', 'All (0)']);
    await expect(button(page, 'Create PO')).toBeVisible();

    await tab(page, 'Inventory').click();
    await expect(failed).toHaveText("Couldn't load inventory — HTTP 500");
    await expect(filters(page)).toHaveText(['All (5)', 'Critical / Out (2)', 'Low (1)']);
    await expect(body(page).locator('table')).toHaveCount(0);
  });

  // Gap 3 (c3e4edcc): the status failure is not on the shared marker.
  // Gap 8 (0dcb0200): the server's status and reason are discarded.
  for (const [code, reason] of [
    [503, 'requires jobs/assets/shipping clients — not configured'],
    [502, 'shipping: connection refused'],
  ] as const) {
    test(`a failed warehouse-status read (${code}) says "unavailable" without its reason, and the header falls back to items`, async ({ page }) => {
      await installWarehouse(page);
      await page.route(STATUS, (r) => r.fulfill({ status: code, contentType: 'text/plain', body: reason }));
      await mountWarehouse(page);

      await expect(overviewLine(page)).toHaveText('Warehouse status unavailable.');
      await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
      await expect(body(page)).not.toContainText(reason);
      await expect(body(page)).not.toContainText(String(code));
      await expect(subtitle(page)).toHaveText('3 below reorder point');
      await expect(body(page).locator('section.tab-section')).toHaveCount(0);
    });
  }

  // Gap 5 (8b1deea2): items and status both down paints an empty
  // warehouse in the header.
  test('items and status both failed: the header reads "0 tracked SKUs"', async ({ page }) => {
    await installWarehouse(page);
    await page.route(ITEMS, (r) => json(r, { error: 'down' }, 503));
    await page.route(STATUS, (r) => json(r, { error: 'down' }, 503));
    await mountPage(page, PATH);
    await expect(overviewLine(page)).toHaveText('Warehouse status unavailable.');
    await expect(title(page)).toHaveText('0 tracked SKUs');
    await expect(subtitle(page)).toHaveText('0 below reorder point');
  });

  test('a network failure on one read names the browser\'s message on both tabs and loses the status too', async ({ page }) => {
    await installWarehouse(page);
    await page.route(ITEMS, (r) => r.abort('failed'));
    await mountPage(page, PATH);
    await expect(overviewLine(page)).toHaveText('Warehouse status unavailable.');
    await expect(title(page)).toHaveText('0 tracked SKUs');
    await tab(page, 'Inventory').click();
    await expect(page.locator(FAILURE_MARKER)).toHaveText("Couldn't load inventory — Failed to fetch");
    await tab(page, 'Receiving').click();
    await expect(page.locator(FAILURE_MARKER)).toHaveText("Couldn't load purchase orders — Failed to fetch");
  });

  test('a 200 with an unexpected body paints as empty; a {data} envelope is read', async ({ page }) => {
    await installWarehouse(page);
    await page.route(ITEMS, (r) => json(r, { error: 'contract changed' }));
    await mountWarehouse(page);
    await tab(page, 'Inventory').click();
    await expect(listLine(page)).toHaveText('No items match that filter.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    await page.route(ITEMS, (r) => json(r, { data: ITEMS_BODY }));
    await page.reload();
    await expect(title(page)).toHaveText('5 tracked SKUs');
    await tab(page, 'Inventory').click();
    await expect(firstColumn(page)).toHaveText(DEFAULT_SKUS);
  });
});
