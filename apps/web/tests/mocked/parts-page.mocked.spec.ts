// /ux/parts — "Ingredients & parts" (department warehouse), every
// control and render state pinned as the page behaves TODAY (page audit
// 63d810aa, step `test`).
//
// Before this spec the route was reached by the generic crawls only
// (route-smoke, outage-crawl, interaction-crawl), under an all-modules
// manifest: nothing clicked a filter and read what it left, searched,
// followed a SKU link, failed ONE read of the four, or rendered the
// module-disabled notice the live instance actually shows — its
// manifest has `parts: false` and `warehouse: false` (read 2026-09-23).
//
// Two renders, because the live instance and the page disagree:
//   State A — the parts module off (the live instance, 2026-09-23):
//             ModuleDisabled, one button, no reads.
//   State B — the module on (the brewery's tenant.toml): PartsList —
//             1 link kind (the SKU cell, once per row), 6 stock-status
//             buttons + up to 4 kind buttons, 1 search input, 4 reads,
//             0 writes.
//
// Lines that pin a FILED gap's current behaviour name the gap. They are
// meant to be edited by the car that fixes it, so the fix shows up here
// as a changed expectation instead of a silently-passing one.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { installSmokeMocks, installTenantManifest, MODULES_LIVE } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG.parts.path;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const MODELS = /\/api\/catalog\/models$/;
const ITEMS = /\/api\/inventory\/items$/;
const ORDERS = /\/api\/inventory\/orders$/;
const CATALOG_PARTS = /\/api\/catalog\/parts$/;
const READ_PATHS: ReadonlySet<string> = new Set([
  '/api/catalog/models', '/api/inventory/items', '/api/inventory/orders', '/api/catalog/parts',
]);

// ── Fixtures: both tenant shapes at once ────────────────────────────
// The device shape (satellite linkage on catalog models: spare parts
// and consumables, with "used by") and the brewery shape (a flat
// /api/catalog/parts list, kind from the SKU prefix), so every kind
// button renders and every branch of the Name / Kind / Used by cells
// is reached.

const deviceModel = (
  sku: string, name: string,
  spare_parts: ReadonlyArray<Record<string, unknown>>,
  consumables: ReadonlyArray<Record<string, unknown>>,
) => ({
  sku, name, manufacturer: 'Acme', model_year: 2024, category: 'brewhouse',
  extras: null, physical: null, regulatory: null,
  commerce: {
    list_price_new_cents: 100_000, typical_refurb_price_cents: null, currency: 'USD',
    lead_time_days: null, tagline: '', description: '', use_cases: [], hero_image: null,
  },
  service: {
    preventive_maintenance_hours: 2, preventive_maintenance_interval_months: 6,
    calibration_interval_months: 12, required_skill_level: 2, depot_required: false,
    common_failure_modes: [], pm_checklist: [],
  },
  spare_parts, consumables, documents: [], end_of_support: null, current_firmware: null,
});

const GASKET = {
  part_sku: 'SP-GASKET-01', name: 'Tri-clamp gasket', description: 'EPDM seal for 2in clamps',
  unit_price_cents: 450, currency: 'USD', lead_time_days: 7, high_usage: true,
};
const CAUSTIC = {
  part_sku: 'CN-CIP-01', name: 'CIP caustic', description: 'Clean-in-place wash',
  unit_price_cents: 3_000, currency: 'USD', treatments_per_unit: 20,
};

const MODELS_BODY = [
  deviceModel('DM-FERM-1', 'Fermenter 1000L', [GASKET], [CAUSTIC]),
  // A second model using the gasket — "Used by" 2 for it. Its own SKU
  // is ALSO stocked (DM-KEG-1 below), which paints "Used by" as "—".
  deviceModel('DM-KEG-1', 'Keg washer', [GASKET], []),
];

const flatPart = (part_sku: string, name: string, description: string) => ({
  part_sku, name, description, unit_price_cents: 1_000, currency: 'USD', lead_time_days: 5,
});
const CATALOG_PARTS_BODY = [
  flatPart('ING-MALT-01', 'Pale malt', '2-row base malt'),
  flatPart('ING-HOPS-01', 'Citra hops', 'Pellet hops, T90'),
  flatPart('PKG-CAN-01', '16oz can', 'Aluminium can, printed'),
  // Catalogued but never stocked: no inventory row (gap 8, 4cb8c06a).
  flatPart('ING-YEAST-01', 'Ale yeast', 'Dry yeast, 500g'),
];

const item = (part_sku: string, on_hand: number, allocated: number, reorder_point: number, bin: string) => ({
  part_sku, bin, on_hand, allocated, reorder_point, reorder_qty: 10, trailing_90d_usage: 0,
});
// Arrival order deliberately NOT the page's sort order (out, critical,
// low, healthy). available = on_hand - allocated.
const ITEMS_BODY = [
  item('ING-MALT-01', 500, 100, 200, 'A-01'), // 400 > 200          → healthy
  item('ING-HOPS-01', 30, 0, 100, 'A-02'), //    30 < 100/2         → critical
  item('PKG-CAN-01', 1_000, 1_000, 500, 'B-01'), // 0 <= 0          → out
  item('SP-GASKET-01', 12, 2, 10, 'C-01'), //    10 <= 10           → low
  item('CN-CIP-01', 40, 0, 10, 'C-02'), //       40 > 10            → healthy
  item('DM-KEG-1', 3, 0, 1, 'D-01'), //          3 > 1              → healthy
];

const po = (id: string, status: string, lines: ReadonlyArray<[string, number]>) => ({
  id, vendor: 'v-1', status, placed_on: '2026-09-01', expected_on: '2026-09-30', received_on: null,
  lines: lines.map(([part_sku, qty]) => ({ part_sku, qty, unit_cost_cents: 100, currency: 'USD' })),
});
const ORDERS_BODY = [
  po('PO-1', 'submitted', [['ING-HOPS-01', 200], ['PKG-CAN-01', 2_000]]),
  po('PO-2', 'in-transit', [['ING-HOPS-01', 50]]),
  // Received and closed POs are not "on order".
  po('PO-3', 'received', [['ING-MALT-01', 1_000]]),
  po('PO-4', 'closed', [['SP-GASKET-01', 5]]),
];

/// The table, in the page's sort order, cell by cell:
/// Part SKU · Name · Kind · On hand · Allocated · Reorder pt · On order · Status · Used by · Bin.
const ROWS: ReadonlyArray<ReadonlyArray<string>> = [
  ['PKG-CAN-01', '16oz can', 'packaging', '1000', '1000', '500', '2000', 'out', '0', 'B-01'],
  ['ING-HOPS-01', 'Citra hops', 'ingredient', '30', '0', '100', '250', 'critical', '0', 'A-02'],
  ['SP-GASKET-01', 'Tri-clamp gasket', 'spare', '12', '2', '10', '—', 'low', '2', 'C-01'],
  ['ING-MALT-01', 'Pale malt', 'ingredient', '500', '100', '200', '—', 'healthy', '0', 'A-01'],
  ['CN-CIP-01', 'CIP caustic', 'consumable', '40', '0', '10', '—', 'healthy', '1', 'C-02'],
  // Stocked under a catalog MODEL's sku: no name from either source, so
  // the name is the SKU; kind from the prefix fallback; Used by "—".
  ['DM-KEG-1', 'DM-KEG-1', 'spare', '3', '0', '1', '—', 'healthy', '—', 'D-01'],
];
const SKUS = ROWS.map((r) => r[0]!);

const HEADINGS = [
  'Part SKU', 'Name', 'Kind', 'On hand', 'Allocated', 'Reorder pt', 'On order', 'Status', 'Used by', 'Bin',
];

/// The shell with the parts module on (installSmokeMocks' MODULES_ON)
/// and the four reads answered from the fixtures above; a spec
/// re-routes any one of them after this to change it.
async function installParts(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(MODELS, (r) => json(r, MODELS_BODY));
  await page.route(ITEMS, (r) => json(r, ITEMS_BODY));
  await page.route(ORDERS, (r) => json(r, ORDERS_BODY));
  await page.route(CATALOG_PARTS, (r) => json(r, CATALOG_PARTS_BODY));
}

/// The shell's own non-GET: App.svelte records every route open
/// (shell/surface-opens.ts). It is the chrome's write, not this page's.
const SHELL_WRITES: ReadonlySet<string> = new Set(['/api/surface-opens']);

/// Every read of the page's four paths, and every non-GET it sends.
function watch(page: Page): { reads: string[]; writes: Request[] } {
  const seen = { reads: [] as string[], writes: [] as Request[] };
  page.on('request', (req) => {
    const url = new URL(req.url());
    if (!url.pathname.startsWith('/api/')) return;
    if (req.method() !== 'GET') {
      if (!SHELL_WRITES.has(url.pathname)) seen.writes.push(req);
      return;
    }
    if (READ_PATHS.has(url.pathname)) seen.reads.push(url.pathname);
  });
  return seen;
}

const body = (page: Page) => page.locator('.catalog');
const button = (page: Page, name: string | RegExp) =>
  body(page).getByRole('button', { name, exact: typeof name === 'string' });
const search = (page: Page) => body(page).getByPlaceholder('SKU, name…');
const skuColumn = (page: Page) => body(page).locator('tbody tr td:first-child');
const status = (page: Page) => body(page).locator('.list-section p.empty');

async function mountParts(page: Page): Promise<void> {
  await mountPage(page, PATH);
  await expect(body(page).locator('tbody tr')).toHaveCount(ROWS.length);
}

/// The manifest as a SERVED page carries it: the gateway inlines it into
/// index.html as `window.__BOSS_TENANT_MANIFEST__` (5578e42d), so the
/// shell is `ready` before its first paint. The mocked dev-server does
/// not inline, so without this the route mounts PartsList while the
/// manifest is still loading (a loading manifest hides nothing,
/// manifest-inline.ts `moduleOn`) — pinned separately below.
async function inlineManifest(page: Page, modules: Readonly<Record<string, boolean>>): Promise<void> {
  await page.addInitScript((m) => {
    (globalThis as { __BOSS_TENANT_MANIFEST__?: unknown }).__BOSS_TENANT_MANIFEST__ = {
      display_name: 'Algedonic, LLC', tenant_id: 'algedonic', modules: m, labels: {},
    };
  }, modules);
}

test.describe('/ux/parts — State A, the parts module off (the live instance)', () => {
  for (const [name, modules] of [
    ['a manifest listing no modules', MODULES_LIVE],
    // The live manifest's own two flags, read 2026-09-23.
    ['a manifest with parts = false and warehouse = false', { parts: false, warehouse: false }],
  ] as const) {
    test(`${name} renders ModuleDisabled, and its one button goes home and back`, async ({ page }) => {
      const seen = watch(page);
      await installParts(page);
      await installTenantManifest(page, modules);
      await inlineManifest(page, modules);
      await mountPage(page, PATH);

      const notice = page.locator('.module-disabled');
      await expect(notice.locator('h1')).toHaveText('Not enabled for this tenant');
      await expect(notice.locator('strong')).toHaveText(ROUTE_CATALOG.parts.label);
      await expect(notice.locator('strong')).toHaveText('Ingredients & parts');
      await expect(notice).toContainText(
        "The Ingredients & parts module is turned off in this tenant's tenant.toml. The page exists in the platform — the active tenant just doesn't surface it.",
      );
      // Gap 3 (fa838818): the one instruction names examples/<tenant>,
      // and this instance's tenant is not in examples/.
      await expect(notice).toContainText(
        'To enable: set parts = true in examples/<tenant>/seeds/tenant.toml under [modules], redeploy, and the page comes back.',
      );
      await expect(notice.getByRole('button')).toHaveCount(1);
      await expect(notice.locator('a')).toHaveCount(0);
      await expect(page.locator('.catalog-filters')).toHaveCount(0);

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

  // Measured while writing this spec: with no inlined manifest (the
  // fetch fallback), the page behind the gate mounts first and makes
  // its four reads before the notice replaces it. On a served page the
  // gateway inlines the manifest, so this is the fallback's behaviour,
  // not the live instance's.
  test('without an inlined manifest the page behind the gate reads once before the notice replaces it', async ({ page }) => {
    const seen = watch(page);
    await installParts(page);
    await installTenantManifest(page, { parts: false, warehouse: false });
    await mountPage(page, PATH);
    await expect(page.locator('.module-disabled h1')).toHaveText('Not enabled for this tenant');
    await expect(page.locator('.catalog-filters')).toHaveCount(0);
    expect(await settledReads(page, () => seen.reads.length, 4)).toBe(4);
    expect([...seen.reads].sort()).toEqual([...READ_PATHS].sort());
  });
});

test.describe('/ux/parts — State B, the module on: the list', () => {
  test('mount makes the four reads once, writes nothing, and paints the header, the groups and every row', async ({ page }) => {
    const seen = watch(page);
    await installParts(page);
    await mountParts(page);

    expect(await settledReads(page, () => seen.reads.length, 4)).toBe(4);
    expect([...seen.reads].sort()).toEqual([...READ_PATHS].sort());

    await expect(page.locator('.exec-eyebrow')).toHaveText('Inventory');
    // The label fallback: the mocked manifest carries no labels.
    await expect(body(page).locator('h1.exec-title')).toHaveText('6 parts');
    await expect(body(page).locator('header p')).toHaveText('3 need attention · 1 out · 1 critical');
    await expect(body(page).locator('.filter-label')).toHaveText(['Search', 'Stock status', 'Kind']);

    // 6 stock-status buttons + all 4 kind buttons (each kind is present),
    // and nothing else on the page is a button.
    await expect(body(page).locator('.filter-group').nth(1).getByRole('button')).toHaveText([
      'Needs attention (3)', 'All (6)', 'Out of stock (1)', 'Critical (1)', 'Low (1)', 'Healthy (3)',
    ]);
    await expect(body(page).locator('.filter-group').nth(2).getByRole('button')).toHaveText([
      'Ingredients (2)', 'Packaging (1)', 'Spare parts (2)', 'Consumables (1)',
    ]);
    await expect(body(page).getByRole('button')).toHaveCount(10);
    // Default filter is All.
    await expect(body(page).locator('.filter-btn-active')).toHaveText('All (6)');
    await expect(search(page)).toHaveValue('');

    await expect(body(page).locator('thead th')).toHaveText(HEADINGS);
    const rows = body(page).locator('tbody tr');
    for (const [i, cells] of ROWS.entries()) {
      await expect(rows.nth(i).locator('td')).toHaveText([...cells]);
    }
    // Gap 8 (4cb8c06a): a catalogued part with no inventory row is not a
    // row, and the title counts inventory rows only.
    await expect(body(page)).not.toContainText('ING-YEAST-01');
    await expect(body(page)).not.toContainText('Ale yeast');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });

  test('the brewery label names the title', async ({ page }) => {
    await installParts(page);
    await page.route(/\/api\/tenant\/manifest$/, (r) =>
      json(r, {
        display_name: 'Algedonic Ales', tenant_id: 'brewery', modules: { parts: true },
        labels: { 'parts.page_title': 'ingredients & packaging items' },
      }),
    );
    await mountParts(page);
    await expect(body(page).locator('h1.exec-title')).toHaveText('6 ingredients & packaging items');
  });

  test('a brewery-only inventory shows only the kind buttons it has', async ({ page }) => {
    await installParts(page);
    await page.route(MODELS, (r) => json(r, []));
    await page.route(ITEMS, (r) => json(r, ITEMS_BODY.filter((i) => /^(ING|PKG)-/.test(i.part_sku))));
    await mountPage(page, PATH);
    await expect(body(page).locator('tbody tr')).toHaveCount(3);

    await expect(body(page).locator('.filter-group').nth(2).getByRole('button')).toHaveText([
      'Ingredients (2)', 'Packaging (1)',
    ]);
    await expect(button(page, /^Spare parts/)).toHaveCount(0);
    await expect(button(page, /^Consumables/)).toHaveCount(0);
    // Gap 9 (e709ee79): with no device linkage every row reads "used by 0".
    await expect(body(page).locator('tbody tr td:nth-child(9)')).toHaveText(['0', '0', '0']);
  });
});

test.describe('/ux/parts — State B: every filter button does what its label says', () => {
  const EXPECT: ReadonlyArray<readonly [string, ReadonlyArray<string>]> = [
    ['Needs attention (3)', ['PKG-CAN-01', 'ING-HOPS-01', 'SP-GASKET-01']],
    ['Out of stock (1)', ['PKG-CAN-01']],
    ['Critical (1)', ['ING-HOPS-01']],
    ['Low (1)', ['SP-GASKET-01']],
    ['Healthy (3)', ['ING-MALT-01', 'CN-CIP-01', 'DM-KEG-1']],
    ['Ingredients (2)', ['ING-HOPS-01', 'ING-MALT-01']],
    ['Packaging (1)', ['PKG-CAN-01']],
    ['Spare parts (2)', ['SP-GASKET-01', 'DM-KEG-1']],
    ['Consumables (1)', ['CN-CIP-01']],
    ['All (6)', SKUS],
  ];

  test('each of the ten narrows the table to its set, client-side, without a read or a history entry', async ({ page }) => {
    const seen = watch(page);
    await installParts(page);
    await mountParts(page);
    expect(await settledReads(page, () => seen.reads.length, 4)).toBe(4);
    const depth = await page.evaluate(() => window.history.length);

    for (const [label, skus] of EXPECT) {
      await button(page, label).click();
      await expect(body(page).locator('.filter-btn-active')).toHaveText(label);
      await expect(skuColumn(page), label).toHaveText([...skus]);
    }

    expect(seen.reads, 'filters are client-side').toHaveLength(4);
    expect(new URL(page.url()).pathname).toBe(PATH);
    expect(new URL(page.url()).search).toBe('');
    expect(await page.evaluate(() => window.history.length)).toBe(depth);
    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });

  test('a filter that matches nothing, and the search that empties it, share one sentence', async ({ page }) => {
    await installParts(page);
    await mountParts(page);
    await button(page, 'Healthy (3)').click();
    await search(page).fill('hops');
    await expect(status(page)).toHaveText('No parts match those filters.');
    await expect(body(page).locator('table')).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    // The counts on the buttons are over every row, not the visible set.
    await expect(button(page, 'Healthy (3)')).toBeVisible();
  });
});

test.describe('/ux/parts — State B: the search input', () => {
  test('matches SKU, name and description, case-insensitively, and submits nothing', async ({ page }) => {
    const seen = watch(page);
    await installParts(page);
    await mountParts(page);
    expect(await settledReads(page, () => seen.reads.length, 4)).toBe(4);

    for (const [q, skus] of [
      ['pkg-', ['PKG-CAN-01']], //               SKU
      ['CITRA', ['ING-HOPS-01']], //             name, any case
      ['2-row', ['ING-MALT-01']], //             description
      ['gasket', ['SP-GASKET-01']], //           device-linkage name
      ['clean-in-place', ['CN-CIP-01']], //      device-linkage description
      ['01', ['PKG-CAN-01', 'ING-HOPS-01', 'SP-GASKET-01', 'ING-MALT-01', 'CN-CIP-01']],
    ] as const) {
      await search(page).fill(q);
      await expect(skuColumn(page), q).toHaveText([...skus]);
    }
    await search(page).fill('no-such-part');
    await expect(status(page)).toHaveText('No parts match those filters.');
    await search(page).fill('');
    await expect(skuColumn(page)).toHaveText(SKUS);

    expect(seen.reads, 'search is client-side').toHaveLength(4);
    expect(new URL(page.url()).search).toBe('');
    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });

  // Gap 10 (2361ac45), second half: "Search" is a div, not a <label>, so
  // the input's only name is its placeholder.
  test('the "Search" group label is not a label element', async ({ page }) => {
    await installParts(page);
    await mountParts(page);
    await expect(body(page).locator('label')).toHaveCount(0);
    await expect(body(page).getByLabel('Search', { exact: true })).toHaveCount(0);
    await expect(search(page)).toHaveAttribute('type', 'search');
  });
});

test.describe('/ux/parts — State B: the SKU link, and back', () => {
  test('every row links its SKU under the catalogued parts path; following one lands on the part and back returns', async ({ page }) => {
    await installParts(page);
    await mountParts(page);

    const links = body(page).locator('tbody a');
    await expect(links).toHaveCount(ROWS.length);
    for (const [i, sku] of SKUS.entries()) {
      await expect(links.nth(i)).toHaveText(sku);
      await expect(links.nth(i)).toHaveAttribute('href', `${PATH}/${encodeURIComponent(sku)}`);
    }
    // The one link kind on the page, and it is the SKU cell's.
    await expect(body(page).locator('a')).toHaveCount(ROWS.length);

    await body(page).getByRole('link', { name: 'SP-GASKET-01', exact: true }).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${PATH}/SP-GASKET-01`);
    await expect(page.locator('h1.detail-title')).toHaveText('Tri-clamp gasket');

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(skuColumn(page)).toHaveText(SKUS);
  });

  // Gap 10 (2361ac45): every row is styled as a link (data-table-row-link:
  // pointer cursor, hover wash) but only the SKU cell navigates.
  test('a click on the row outside the SKU cell goes nowhere', async ({ page }) => {
    await installParts(page);
    await mountParts(page);
    const row = body(page).locator('tbody tr').first();
    await expect(row).toHaveClass(/data-table-row-link/);
    await row.locator('td').nth(1).click();
    await row.locator('td').nth(7).click();
    await page.waitForTimeout(300);
    expect(new URL(page.url()).pathname).toBe(PATH);
  });
});

test.describe('/ux/parts — State B: empty, loading, and a failed read', () => {
  test('an empty backend paints the filters sentence, not a failure (gap 6)', async ({ page }) => {
    await installParts(page);
    for (const re of [MODELS, ITEMS, ORDERS, CATALOG_PARTS]) await page.route(re, (r) => json(r, []));
    await mountPage(page, PATH);

    // Gap 6 (bc38daa8): "nothing here" is told to loosen filters it
    // never set — the same line a too-narrow filter paints.
    await expect(status(page)).toHaveText('No parts match those filters.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(body(page).locator('table')).toHaveCount(0);
    await expect(body(page).locator('h1.exec-title')).toHaveText('0 parts');
    await expect(body(page).locator('header p')).toHaveText('0 need attention · 0 out · 0 critical');
    await expect(body(page).getByRole('button')).toHaveText([
      'Needs attention (0)', 'All (0)', 'Out of stock (0)', 'Critical (0)', 'Low (0)', 'Healthy (0)',
    ]);
  });

  // Gap 5 (f867d71c): while loading, the header and buttons count zeros.
  test('a pending read paints "Loading…" beside a zero header', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await installParts(page);
    await page.route(ITEMS, async (r) => {
      await held;
      await json(r, ITEMS_BODY);
    });
    await mountPage(page, PATH);

    await expect(status(page)).toHaveText('Loading…');
    await expect(body(page).locator('h1.exec-title')).toHaveText('0 parts');
    await expect(body(page).locator('header p')).toHaveText('0 need attention · 0 out · 0 critical');
    await expect(button(page, 'All (0)')).toBeVisible();
    release();
    await expect(skuColumn(page)).toHaveText(SKUS);
    await expect(page.getByText('Loading…', { exact: true })).toHaveCount(0);
  });

  for (const [name, re] of [
    ['GET /api/catalog/models', MODELS],
    ['GET /api/inventory/items', ITEMS],
    ['GET /api/catalog/parts', CATALOG_PARTS],
  ] as const) {
    test(`a failed ${name} is said, never drawn as an empty list`, async ({ page }) => {
      await installParts(page);
      await page.route(re, (r) => json(r, { error: 'down' }, 503));
      await mountPage(page, PATH);

      const failed = page.locator(FAILURE_MARKER);
      await expect(failed).toHaveText("Couldn't load parts — HTTP 503");
      await expect(failed).toHaveAttribute('role', 'alert');
      await expect(body(page).locator('table')).toHaveCount(0);
      await expect(page.getByText('No parts match those filters.')).toHaveCount(0);
    });
  }

  // Gap 5 (f867d71c): a failed row source counts zero above an honest alert.
  test('a failed inventory read paints "0 need attention" above the alert', async ({ page }) => {
    await installParts(page);
    await page.route(ITEMS, (r) => json(r, { error: 'down' }, 500));
    await mountPage(page, PATH);
    await expect(page.locator(FAILURE_MARKER)).toHaveText("Couldn't load parts — HTTP 500");
    await expect(body(page).locator('h1.exec-title')).toHaveText('0 parts');
    await expect(body(page).locator('header p')).toHaveText('0 need attention · 0 out · 0 critical');
    await expect(button(page, 'Needs attention (0)')).toBeVisible();
  });

  test('a network failure names the browser\'s own message', async ({ page }) => {
    await installParts(page);
    await page.route(ITEMS, (r) => r.abort('failed'));
    await mountPage(page, PATH);
    await expect(page.locator(FAILURE_MARKER)).toHaveText("Couldn't load parts — Failed to fetch");
    await expect(body(page).locator('table')).toHaveCount(0);
  });

  // Gap 4 (61c16b17): the purchase-order read is not a primary; its
  // outage paints "—" in every On order cell, the paint of "nothing on
  // order", with no failure line.
  test('a failed purchase-order read is swallowed: every On order cell reads "—"', async ({ page }) => {
    await installParts(page);
    await page.route(ORDERS, (r) => json(r, { error: 'down' }, 503));
    await mountParts(page);
    await expect(body(page).locator('tbody tr td:nth-child(7)')).toHaveText(Array(ROWS.length).fill('—'));
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  // Gap 7 (b6b74115): a 200 whose body is neither a list nor {data} is
  // read as empty, with no failure line.
  test('a 200 with an unexpected body paints as empty; a {data} envelope is read', async ({ page }) => {
    await installParts(page);
    await page.route(ITEMS, (r) => json(r, { error: 'contract changed' }));
    await mountPage(page, PATH);
    await expect(status(page)).toHaveText('No parts match those filters.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    await page.route(ITEMS, (r) => json(r, { data: ITEMS_BODY }));
    await page.reload();
    await expect(skuColumn(page)).toHaveText(SKUS);
  });
});
