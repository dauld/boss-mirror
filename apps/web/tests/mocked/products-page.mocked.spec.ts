// /ux/products — "Products" (department production), every control and
// render state pinned as the page behaves TODAY (page audit 6b4e43a1,
// step `test`; gap 2 of that audit is this spec).
//
// Before this spec the route was reached by the generic crawls only
// (route-smoke, outage-crawl, interaction-crawl), under an all-modules
// manifest and with no /api/products fixture: every mocked run painted
// "No products yet", so no row, no SKU link, no detail read and no
// retired row was ever drawn under test (gap 5, 50ab513c), and nothing
// rendered the module-disabled notice the live instance actually
// shows — its manifest has `parts: false` (read 2026-09-23).
//
// Two renders, because the live instance and the page disagree:
//   State A — the parts module off (the live instance, 2026-09-23):
//             ModuleDisabled, 1 button, 0 links, 0 reads, 0 writes.
//   State B — the module on (the playground shape): ProductsList —
//             0 buttons, 1 search input, 0 forms, 1 link per row (the
//             SKU cell), 1 + N reads (the list, then one detail read
//             per product), 0 writes.
//
// Lines that pin a FILED gap's current behaviour name the gap. They are
// meant to be edited by the car that fixes it, so the fix shows up here
// as a changed expectation instead of a silently-passing one.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { answerRead, mountPage, openedRequests, recordPageRequests, settledReads } from './_helpers';
import {
  installSmokeMocks, installTenantManifest, LIVE_MANIFEST_RECORDED_AT, MODULES_LIVE, MODULES_NONE,
  tenantManifest,
} from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG.products.path;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const LIST = /\/api\/products$/;
const DETAIL = /\/api\/products\/[^/]+$/;
const detailRoute = (sku: string) => new RegExp(`/api/products/${sku}$`);

// ── Fixtures ────────────────────────────────────────────────────────
// Four products in an arrival order that is NOT the page's sku order,
// one of them retired (active=false), one with no metadata at all (so
// the MSRP and Style cells take their empty branch).

const product = (
  sku: string, name: string, product_kind: string, package_unit: string,
  metadata: Record<string, unknown>, active = true,
) => ({ sku, name, product_kind, package_unit, description: `${name}, as brewed`, metadata, active });

const PRODUCTS = [
  product('MERCH-TEE-L', 'Brewery tee (L)', 'apparel', 'each', {}),
  product('KEG-IPA-HALF', 'Hazy IPA', 'beer', 'half-barrel keg', { msrp_cents: 18_500, style: 'New England IPA' }),
  product('CAN-STOUT-4PK', 'Oatmeal stout', 'beer', '4-pack 16oz cans', { msrp_cents: 1_599, style: 'Stout' }, false),
  product('CAN-LAGER-6PK', 'Pils', 'beer', '6-pack 12oz cans', { msrp_cents: 1_299, style: 'Pilsner' }),
];

/// Each product's rolled-up stock, served by its detail read.
const ON_HAND: Readonly<Record<string, number>> = {
  'MERCH-TEE-L': 30, 'KEG-IPA-HALF': 12, 'CAN-STOUT-4PK': 7, 'CAN-LAGER-6PK': 240,
};

const detail = (p: (typeof PRODUCTS)[number]) => ({
  ...p,
  inventory: [{ product_sku: p.sku, location_id: 'loc-cellar', on_hand: ON_HAND[p.sku]!, reserved: 0 }],
  total_on_hand: ON_HAND[p.sku]!,
});

/// The table in the page's order (sku, localeCompare), cell by cell:
/// SKU · Name · Kind · Package · Total on hand · MSRP · Style.
const ROWS: ReadonlyArray<ReadonlyArray<string>> = [
  ['CAN-LAGER-6PK', 'Pils', 'beer', '6-pack 12oz cans', '240', '$12.99', 'Pilsner'],
  ['CAN-STOUT-4PK', 'Oatmeal stout', 'beer', '4-pack 16oz cans', '7', '$15.99', 'Stout'],
  ['KEG-IPA-HALF', 'Hazy IPA', 'beer', 'half-barrel keg', '12', '$185.00', 'New England IPA'],
  ['MERCH-TEE-L', 'Brewery tee (L)', 'apparel', 'each', '30', '', ''],
];
const SKUS = ROWS.map((r) => r[0]!);
const HEADINGS = ['SKU', 'Name', 'Kind', 'Package', 'Total on hand', 'MSRP', 'Style'];
const READS_AT_MOUNT = 1 + PRODUCTS.length;

const SUBTITLE = 'Finished-product catalog with on-hand inventory across all locations.';
const EMPTY_LINE =
  "No products yet. The brewery's finished-product catalog is seeded via examples/brewery/seeds/products.toml.";

/// The shell (installSmokeMocks: MODULES_ON, so parts is on) with the
/// list read and every detail read answered from the fixtures above; a
/// spec re-routes any one of them after this to change it.
async function installProducts(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(LIST, (r) => json(r, PRODUCTS));
  await page.route(DETAIL, (r) => {
    const sku = decodeURIComponent(new URL(r.request().url()).pathname.split('/').pop()!);
    const p = PRODUCTS.find((x) => x.sku === sku);
    return p ? json(r, detail(p)) : json(r, 'not found', 404);
  });
}

/// The shell's own non-GET: App.svelte records every route open
/// (shell/surface-opens.ts). It is the chrome's write, not this page's.
const SHELL_WRITES: ReadonlySet<string> = new Set(['/api/surface-opens']);

/// Every read of the page's paths (the list and each detail), and every
/// non-GET it sends.
function watch(page: Page): { reads: string[]; writes: Request[] } {
  const seen = { reads: [] as string[], writes: [] as Request[] };
  page.on('request', (req) => {
    const url = new URL(req.url());
    if (!url.pathname.startsWith('/api/')) return;
    if (req.method() !== 'GET') {
      if (!SHELL_WRITES.has(url.pathname)) seen.writes.push(req);
      return;
    }
    if (url.pathname === '/api/products' || url.pathname.startsWith('/api/products/')) {
      seen.reads.push(url.pathname);
    }
  });
  return seen;
}

/// ProductsList's own root: the one `div.page` carrying the search toolbar.
const list = (page: Page) => page.locator('div.page:has(.toolbar)');
const search = (page: Page) => list(page).getByPlaceholder('Search SKU, name, or kind…');
const skuColumn = (page: Page) => list(page).locator('tbody tr td:first-child');
const status = (page: Page) => list(page).locator('p.empty');

async function mountProducts(page: Page): Promise<void> {
  await mountPage(page, PATH);
  await expect(list(page).locator('tbody tr')).toHaveCount(ROWS.length);
}

test.describe('/ux/products — State A, the parts module off (the live instance)', () => {
  for (const [name, modules] of [
    // The recorded live manifest (page audit 6b4e43a1 spelled it here
    // as a second copy, because MODULES_LIVE still said `{}`; 41454ce1
    // collapsed the two).
    [`the live manifest recorded ${LIVE_MANIFEST_RECORDED_AT} (parts = false)`, MODULES_LIVE],
    ['a manifest listing no modules', MODULES_NONE],
  ] as const) {
    test(`${name} renders ModuleDisabled, and its one button goes home and back`, async ({ page }) => {
      const seen = watch(page);
      await installProducts(page);
      await installTenantManifest(page, modules, { inline: true });
      await mountPage(page, PATH);

      const notice = page.locator('.module-disabled');
      await expect(notice.locator('h1')).toHaveText('Not enabled for this tenant');
      await expect(notice.locator('strong')).toHaveText(ROUTE_CATALOG.products.label);
      // Gap 3 (98e91886): the notice names a "Products" module, and the
      // flag it tells the reader to set is `parts` — the catalog entry's
      // `module`. The manifest has no `products` key.
      await expect(notice.locator('strong')).toHaveText('Products');
      await expect(notice).toContainText(
        "The Products module is turned off in this tenant's tenant.toml. The page exists in the platform — the active tenant just doesn't surface it.",
      );
      // fa838818: the one instruction names examples/<tenant>, and this
      // instance's tenant is not in examples/.
      await expect(notice).toContainText(
        'To enable: set parts = true in examples/<tenant>/seeds/tenant.toml under [modules], redeploy, and the page comes back.',
      );
      await expect(notice.locator('code').first()).toHaveText('tenant.toml');
      await expect(notice.locator('code').nth(1)).toHaveText('parts = true');
      await expect(notice.getByRole('button')).toHaveCount(1);
      await expect(notice.locator('a')).toHaveCount(0);
      await expect(notice.locator('input, form')).toHaveCount(0);
      await expect(list(page)).toHaveCount(0);

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

  // The SKU link's target is gated by the same flag (sections.ts maps
  // the `product` kind to the `products` row), so a deep link to one
  // product answers with the same notice, under the list's label.
  test('a deep link to one product renders the same notice, and reads nothing', async ({ page }) => {
    await recordPageRequests(page);
    await installProducts(page);
    await installTenantManifest(page, MODULES_LIVE, { inline: true });
    await mountPage(page, `${PATH}/KEG-IPA-HALF`);

    const notice = page.locator('.module-disabled');
    await expect(notice.locator('h1')).toHaveText('Not enabled for this tenant');
    await expect(notice.locator('strong')).toHaveText('Products');
    await expect(page.getByText('Hazy IPA')).toHaveCount(0);
    // The page's own record, read once the notice has painted: a read the
    // page opened on the way there is in it already. It slept 300 ms and
    // read Playwright's request event until backlog 840c5a76.
    const reads = (await openedRequests(page)).filter(
      (e) => e.method === 'GET' && (e.path === '/api/products' || e.path.startsWith('/api/products/')),
    );
    expect(reads.map((e) => e.path)).toEqual([]);
  });

  // a1fcee7b, measured while writing this spec: with no inlined manifest
  // (the fetch fallback), the page behind the gate mounts first and
  // makes its reads before the notice replaces it. The manifest answer
  // is HELD here until all 1 + N reads have arrived, so the order is
  // deterministic: nothing about the pending manifest stops the page
  // reading and painting its rows. On a served page the gateway inlines
  // the manifest, so this is the fallback's behaviour, not the live
  // instance's.
  test('without an inlined manifest the page behind the gate reads and paints before the notice replaces it', async ({ page }) => {
    const seen = watch(page);
    await installProducts(page);
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await page.route(/\/api\/tenant\/manifest$/, async (r) => {
      await held;
      await json(r, tenantManifest(MODULES_LIVE));
    });
    await mountPage(page, PATH);

    await expect.poll(() => seen.reads.length).toBe(READS_AT_MOUNT);
    await expect(list(page).locator('tbody tr')).toHaveCount(ROWS.length);
    await expect(list(page).locator('h1.exec-title')).toHaveText('Products');

    release();
    await expect(page.locator('.module-disabled h1')).toHaveText('Not enabled for this tenant');
    await expect(list(page)).toHaveCount(0);
    expect(await settledReads(page, () => seen.reads.length, READS_AT_MOUNT)).toBe(READS_AT_MOUNT);
  });
});

test.describe('/ux/products — State B, the module on: the list', () => {
  test('mount makes 1 + N reads once, writes nothing, and paints the header and every row in sku order', async ({ page }) => {
    const seen = watch(page);
    await installProducts(page);
    await mountProducts(page);

    expect(await settledReads(page, () => seen.reads.length, READS_AT_MOUNT)).toBe(READS_AT_MOUNT);
    expect([...seen.reads].sort()).toEqual(
      ['/api/products', ...PRODUCTS.map((p) => `/api/products/${p.sku}`)].sort(),
    );

    await expect(list(page).locator('h1.exec-title')).toHaveText('Products');
    await expect(list(page).locator('header p')).toHaveText(SUBTITLE);
    await expect(list(page).locator('.exec-eyebrow')).toHaveCount(0);
    await expect(search(page)).toHaveValue('');
    await expect(search(page)).toHaveAttribute('type', 'search');

    await expect(list(page).locator('thead th')).toHaveText(HEADINGS);
    const rows = list(page).locator('tbody tr');
    for (const [i, cells] of ROWS.entries()) {
      await expect(rows.nth(i).locator('td')).toHaveText([...cells]);
    }
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    // The inventory: 0 buttons, 1 input, 0 forms, one link per row.
    await expect(list(page).getByRole('button')).toHaveCount(0);
    await expect(list(page).locator('input')).toHaveCount(1);
    await expect(list(page).locator('form')).toHaveCount(0);
    await expect(list(page).locator('a')).toHaveCount(ROWS.length);

    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });

  // Gap 7 (f57c2f3b): a retired product is dimmed and nothing else — no
  // word, no filter, no count says it is retired.
  test('a retired product is only dimmed: no text on the page says retired', async ({ page }) => {
    await installProducts(page);
    await mountProducts(page);

    const retired = list(page).locator('tbody tr').nth(1);
    await expect(retired.locator('td').first()).toHaveText('CAN-STOUT-4PK');
    await expect(retired).toHaveClass(/\bretired\b/);
    await expect(retired).toHaveCSS('opacity', '0.5');
    await expect(list(page).locator('tbody tr.retired')).toHaveCount(1);
    await expect(list(page).getByText(/retired|inactive/i)).toHaveCount(0);
  });
});

test.describe('/ux/products — State B: the search input', () => {
  test('matches SKU, name and kind, case-insensitively, client-side, and rides in the URL as q without a history entry', async ({ page }) => {
    const seen = watch(page);
    await installProducts(page);
    await mountProducts(page);
    expect(await settledReads(page, () => seen.reads.length, READS_AT_MOUNT)).toBe(READS_AT_MOUNT);
    const depth = await page.evaluate(() => window.history.length);

    for (const [q, skus] of [
      ['can-', ['CAN-LAGER-6PK', 'CAN-STOUT-4PK']], // SKU
      ['HAZY', ['KEG-IPA-HALF']], //                   name, any case
      ['apparel', ['MERCH-TEE-L']], //                 kind
      ['beer', ['CAN-LAGER-6PK', 'CAN-STOUT-4PK', 'KEG-IPA-HALF']],
    ] as const) {
      await search(page).fill(q);
      await expect(skuColumn(page), q).toHaveText([...skus]);
    }

    // Style and package are not searched: a word only the Style cell
    // holds matches nothing, and the line names the query.
    await search(page).fill('Pilsner');
    await expect(status(page)).toHaveText('No products match Pilsner.');
    await expect(status(page).locator('strong')).toHaveText('Pilsner');
    await expect(list(page).locator('table')).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    await search(page).fill('');
    await expect(skuColumn(page)).toHaveText(SKUS);

    // Gap 8 (1c2db4c2), fixed: the query rides in the URL as `q` — still
    // no read and no history entry (replaceState), but a reload comes
    // back filtered, with the box holding what was typed.
    expect(seen.reads, 'search is client-side').toHaveLength(READS_AT_MOUNT);
    await search(page).fill('stout');
    await expect.poll(() => new URL(page.url()).search).toBe('?q=stout');
    expect(new URL(page.url()).pathname).toBe(PATH);
    expect(await page.evaluate(() => window.history.length)).toBe(depth);
    await page.reload();
    await expect(search(page)).toHaveValue('stout');
    await expect(skuColumn(page)).toHaveText(['CAN-STOUT-4PK']);
    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });

  test('clearing the box takes q back out of the URL, and parameters it does not own ride through', async ({ page }) => {
    await installProducts(page);
    await mountPage(page, `${PATH}?from=home`);
    await expect(list(page).locator('tbody tr')).toHaveCount(ROWS.length);

    await search(page).fill('pale ale');
    await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('pale ale');
    expect(new URL(page.url()).searchParams.get('from')).toBe('home');

    await search(page).fill('');
    await expect.poll(() => new URL(page.url()).search).toBe('?from=home');
    await expect(skuColumn(page)).toHaveText(SKUS);
  });

  test('a shared link opens the list already filtered, and mounting rewrites nothing', async ({ page }) => {
    await installProducts(page);
    await mountPage(page, `${PATH}?q=HAZY`);

    await expect(search(page)).toHaveValue('HAZY');
    await expect(skuColumn(page)).toHaveText(['KEG-IPA-HALF']);
    expect(new URL(page.url()).search).toBe('?q=HAZY');
  });
});

test.describe('/ux/products — State B: the SKU link, and back', () => {
  test('every row links its SKU under the catalogued products path; following one lands on the product and back returns', async ({ page }) => {
    await installProducts(page);
    await mountProducts(page);

    const links = list(page).locator('tbody a');
    await expect(links).toHaveCount(ROWS.length);
    for (const [i, sku] of SKUS.entries()) {
      await expect(links.nth(i)).toHaveText(sku);
      await expect(links.nth(i)).toHaveAttribute('href', `${PATH}/${encodeURIComponent(sku)}`);
    }

    await list(page).getByRole('link', { name: 'KEG-IPA-HALF', exact: true }).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${PATH}/KEG-IPA-HALF`);
    await expect(page.locator('h1.exec-title')).toHaveText('Hazy IPA');
    await expect(page.locator('.module-disabled')).toHaveCount(0);

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(skuColumn(page)).toHaveText(SKUS);
  });
});

test.describe('/ux/products — State B: empty, loading, and a failed read', () => {
  // Gap 6 (f8807354): "nothing here" names one tenant's seed file, on
  // every tenant.
  test('an empty backend paints the empty sentence, not a failure', async ({ page }) => {
    const seen = watch(page);
    await installProducts(page);
    await page.route(LIST, (r) => json(r, []));
    await mountPage(page, PATH);

    await expect(status(page)).toHaveText(EMPTY_LINE);
    await expect(status(page).locator('code')).toHaveText('examples/brewery/seeds/products.toml');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(list(page).locator('table')).toHaveCount(0);
    expect(await settledReads(page, () => seen.reads.length, 1)).toBe(1);
  });

  test('a pending read paints "Loading products…", and so does one pending detail read', async ({ page }) => {
    let releaseList: () => void = () => {};
    const listHeld = new Promise<void>((resolve) => { releaseList = resolve; });
    let releaseDetail: () => void = () => {};
    const detailHeld = new Promise<void>((resolve) => { releaseDetail = resolve; });
    await recordPageRequests(page);
    await installProducts(page);
    await page.route(LIST, async (r) => {
      await listHeld;
      await json(r, PRODUCTS);
    });
    await page.route(detailRoute('MERCH-TEE-L'), async (r) => {
      await detailHeld;
      await json(r, detail(PRODUCTS[0]!));
    });
    await mountPage(page, PATH);

    await expect(status(page)).toHaveText('Loading products…');
    await expect(list(page).locator('h1.exec-title')).toHaveText('Products');
    releaseList();
    // The list has arrived; one detail read has not. The whole table
    // waits on the slowest product. "Arrived" is the page having read the
    // list and done what it does with it — a page that painted the table
    // then has painted it now. It slept 300 ms until backlog 840c5a76.
    await answerRead(page, LIST);
    await expect(status(page)).toHaveText('Loading products…');
    await expect(list(page).locator('table')).toHaveCount(0);
    releaseDetail();
    await expect(skuColumn(page)).toHaveText(SKUS);
    await expect(page.getByText('Loading products…', { exact: true })).toHaveCount(0);
  });

  test('a failed list read is said, never drawn as an empty list', async ({ page }) => {
    await installProducts(page);
    await page.route(LIST, (r) => json(r, { error: 'down' }, 503));
    await mountPage(page, PATH);

    const failed = page.locator(FAILURE_MARKER);
    await expect(failed).toHaveText("Couldn't load products — HTTP 503");
    await expect(failed).toHaveAttribute('role', 'alert');
    await expect(list(page).locator('table')).toHaveCount(0);
    await expect(page.getByText(/No products yet/)).toHaveCount(0);
  });

  test('a network failure on the list names the browser\'s own message', async ({ page }) => {
    await installProducts(page);
    await page.route(LIST, (r) => r.abort('failed'));
    await mountPage(page, PATH);
    await expect(page.locator(FAILURE_MARKER)).toHaveText("Couldn't load products — Failed to fetch");
    await expect(list(page).locator('table')).toHaveCount(0);
  });

  // Gap 4 (35e95b89): a failed detail read — refused or thrown — is
  // painted as "0" on hand, the paint of "no stock", with no marker.
  test('a failed detail read is swallowed: that row reads Total on hand 0', async ({ page }) => {
    await installProducts(page);
    await page.route(detailRoute('KEG-IPA-HALF'), (r) => json(r, { error: 'down' }, 503));
    await page.route(detailRoute('CAN-LAGER-6PK'), (r) => r.abort('failed'));
    await mountProducts(page);

    await expect(list(page).locator('tbody tr td:nth-child(5)')).toHaveText(['0', '7', '0', '30']);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(list(page).locator('[role=alert]')).toHaveCount(0);
  });
});
