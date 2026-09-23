// /ux/vendors — "Vendors" (finance), every control and render state
// pinned as the page behaves TODAY (page audit 6f282875, step `test`).
//
// Before this spec the route was reached by the two crawls only:
// route-smoke renders it, interaction-crawl clicks whatever the empty
// mock paints, and outage-crawl lists it as SILENT. No spec rendered a
// vendor row, clicked a filter, typed a search, followed the vendor
// link, or failed one of its three reads.
//
// What the page is (controls_md on the packet): one link template (the
// vendor name, one per row), two fixed filter buttons and two per-value
// button families, one search input, no form, three reads, ZERO writes —
// so there is no refused write to show, and the watch below pins that
// the page sends none. All three reads are served by boss-inventory-api,
// which the launcher starts only for module `warehouse`.
//
// Two renders, because the live instance and the fixtures disagree:
//   The served instance — the manifest the gateway served on 2026-09-23
//             lists warehouse = false, and the catalog entry has no
//             `module` key, so the page mounts anyway and its three
//             reads meet the gateway's no-upstream answer (proxy.rs:
//             502, "inventory upstream unavailable").
//   Fixtures — vendors, POs and vendor invoices, so every column, count,
//             filter and the link can be read.
//
// Lines that pin a FILED gap's current behaviour name the gap and its
// item. They are meant to be edited by the car that fixes it, so the fix
// shows up here as a changed expectation instead of a silently-passing
// one:
//   gap 1 223ebcd6  orders + vendor-invoices fold an outage into zeros
//   gap 2 c3e4edcc  the one failure line is not `.load-failed`
//   gap 3 003f4db2  the route is not gated by the warehouse module
//   gap 4 35aeb30d  the empty line blames filters when there are none
//   gap 5 d18b68cf  a null payment_terms paints a blank cell
//   gap 6 e9c8e28d  the vendor link lands on an uncatalogued route
//   gap 7 8d564435  the open/unpaid predicates and the NAME join are
//                   client copies nothing else pins

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { installSmokeMocks, installTenantManifest } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { parseRoute } from '../../src/router';

const PATH = '/ux/vendors';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const VENDORS = /\/api\/inventory\/vendors$/;
const ORDERS = /\/api\/inventory\/orders$/;
const INVOICES = /\/api\/inventory\/vendor-invoices$/;
const READ_PATHS = ['/api/inventory/vendors', '/api/inventory/orders', '/api/inventory/vendor-invoices'];

/// The tenant manifest the live gateway served on 2026-09-23 (read by
/// the measure step of this audit, HTTP 200): finance on, warehouse off.
const MANIFEST_2026_09_23: Readonly<Record<string, boolean>> = {
  calendar: false, equipment: false, exec: true, finance: true, parts: false, qa: false,
  shipping: false, shop: false, sim: false, support: true, warehouse: false,
};

/// The gateway's answer for a proxied route whose upstream does not
/// answer (crates/core/boss-gateway/src/proxy.rs `handle`): 502, plain
/// text naming the service. With warehouse off nothing serves
/// /api/inventory/*, so this is what each of the page's reads gets.
const noUpstream = (r: Route): Promise<void> =>
  r.fulfill({ status: 502, contentType: 'text/plain', body: 'inventory upstream unavailable' });

const vendor = (
  id: string, name: string | null, contact_name: string | null, city: string | null,
  state: string | null, lead_time_days: number, payment_terms: string | null,
  category: string | null,
) => ({ id, name, contact_name, contact_email: null, city, state, lead_time_days, payment_terms, category });

/// Four vendors: two categories and two states, so each filter family
/// has more than one button; one with a null category, city, state AND
/// payment_terms (gap 5); one with a null name, whose link label falls
/// back to its id.
const VENDOR_ROWS = [
  vendor('vnd-hops-001', 'Cascade Hop Farm', 'Rhea Okafor', 'Yakima', 'WA', 14, 'Net 30', 'hop-supplier'),
  vendor('vnd-malt-002', 'Prairie Maltings', 'Ben Lusk', 'Great Falls', 'MT', 21, 'Net 45', 'grain-supplier'),
  vendor('vnd-glass-003', 'Anchor Glassworks', 'Iris Vale', null, null, 7, null, null),
  vendor('vnd-bare-004', null, null, 'Spokane', 'WA', 10, 'Net 15', 'hop-supplier'),
];

const po = (id: string, vendorRef: string | null, status: string) => ({
  id, vendor: vendorRef, status, placed_on: '2026-09-01', expected_on: null, received_on: null, lines: [],
});

/// Cascade: two open (placed, in-transit) of three by name. Prairie: one
/// closed by name, and one OPEN PO that names it by id — which the
/// page's name join does not count (gap 7). One draft with no vendor —
/// which the join `po.vendor === v.name` hands to the vendor whose name
/// is null (null === null), so the unnamed vendor shows an open PO that
/// names nobody. Found writing this spec; pinned as it behaves.
const ORDER_ROWS = [
  po('po-1', 'Cascade Hop Farm', 'placed'),
  po('po-2', 'Cascade Hop Farm', 'in-transit'),
  po('po-3', 'Cascade Hop Farm', 'received'),
  po('po-4', 'Prairie Maltings', 'closed'),
  po('po-5', 'vnd-malt-002', 'placed'),
  po('po-6', null, 'draft'),
];

const bill = (id: string, vendorRef: string, status: string, amount_cents: number) => ({
  id, po_id: 'po-1', vendor: vendorRef, vendor_invoice_no: `INV-${id}`, amount_cents, currency: 'USD',
  received_on: '2026-09-05', matched_on: null, approved_on: null, paid_on: null, status,
  discrepancy_cents: null, discrepancy_kind: null,
});

/// Cascade: $1,250 unpaid and a paid bill. Prairie: $45.50 unpaid. One
/// unpaid bill that names Cascade by id, not counted (gap 7).
const INVOICE_ROWS = [
  bill('b-1', 'Cascade Hop Farm', 'received', 125_000),
  bill('b-2', 'Cascade Hop Farm', 'paid', 50_000),
  bill('b-3', 'Prairie Maltings', 'approved', 4_550),
  bill('b-4', 'vnd-hops-001', 'received', 99_900),
];

type Reads = Readonly<{ vendors: unknown; orders: unknown; invoices: unknown }>;

/// The shell (installSmokeMocks), then the page's three reads. A read
/// given a function is routed to it; anything else is its 200 body.
async function install(
  page: Page,
  reads: Reads,
  manifest?: Readonly<Record<string, boolean>>,
): Promise<void> {
  await installSmokeMocks(page);
  if (manifest) await installTenantManifest(page, manifest);
  const serve = (body: unknown) => (r: Route) =>
    typeof body === 'function' ? (body as (r: Route) => Promise<void>)(r) : json(r, body);
  await page.route(VENDORS, serve(reads.vendors));
  await page.route(ORDERS, serve(reads.orders));
  await page.route(INVOICES, serve(reads.invoices));
}

const FIXTURES: Reads = { vendors: VENDOR_ROWS, orders: ORDER_ROWS, invoices: INVOICE_ROWS };

/// The shell's own non-GET: App.svelte records every route open
/// (shell/surface-opens.ts). It is the chrome's write, not this page's.
const SHELL_WRITES: ReadonlySet<string> = new Set(['/api/surface-opens']);

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

const list = (page: Page) => page.locator('section.list-section');
const empty = (page: Page) => list(page).locator('p.empty');
const button = (page: Page, name: string) => page.getByRole('button', { name, exact: true });
const bodyRows = (page: Page) => page.locator('table.data-table tbody tr');

/// Every visible row's cells, as text.
async function table(page: Page): Promise<string[][]> {
  return bodyRows(page).evaluateAll((rows) =>
    rows.map((row) => Array.from(row.children).map((td) => (td as HTMLElement).innerText.trim())),
  );
}

/// The first column (the link labels), top to bottom.
async function names(page: Page): Promise<string[]> {
  return (await table(page)).map((row) => row[0]);
}

async function expectHeader(page: Page, title: string, subtitle: string): Promise<void> {
  await expect(page.locator('.exec-eyebrow')).toHaveText('Know');
  await expect(page.locator('h1.exec-title')).toHaveText(title);
  await expect(page.locator('.exec-header p')).toHaveText(subtitle);
}

const ZERO_SUBTITLE = '0 open POs · $0.00 outstanding across all vendors';
const OPEN_SUBTITLE = '3 open POs · $1,296 outstanding across all vendors';

/// The page's own search box — the shell's bar carries a second one.
const searchbox = (page: Page) => page.locator('aside.catalog-filters').getByRole('searchbox');

test.describe('/ux/vendors — the served instance (warehouse off, no inventory upstream)', () => {
  test('the page mounts ungated and paints its one failure line, which is not the shared marker', async ({ page }) => {
    const seen = watch(page);
    await install(page, { vendors: noUpstream, orders: noUpstream, invoices: noUpstream }, MANIFEST_2026_09_23);
    await mountPage(page, PATH);

    // Gap 3 (003f4db2): the catalog entry declares no module, so the
    // route gate never fires — no ModuleDisabled notice, whatever the
    // manifest says about the service the page reads.
    expect(ROUTE_CATALOG.vendors.path).toBe(PATH);
    expect(ROUTE_CATALOG.vendors.app).toBe('finance');
    expect(ROUTE_CATALOG.vendors.module).toBeUndefined();
    expect(ROUTE_CATALOG.warehouse.module).toBe('warehouse');
    await expect(page.locator('.module-disabled')).toHaveCount(0);

    await expect(empty(page)).toHaveText("Couldn't load vendors: vendors HTTP 502");
    // Gap 2 (c3e4edcc): a `p.empty`, not the FAILURE_MARKER the outage
    // crawl reads — and the header above it still states zeros as fact.
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expectHeader(page, '0 vendors', ZERO_SUBTITLE);
    await expect(button(page, 'All (0)')).toBeVisible();
    await expect(page.locator('table.data-table')).toHaveCount(0);

    expect(await settledReads(page, () => seen.reads.length, 3)).toBe(3);
    expect([...seen.reads].sort()).toEqual([...READ_PATHS].sort());
    expect(seen.writes).toHaveLength(0);
  });
});

test.describe('/ux/vendors — fixtures', () => {
  test('the page renders every column, count and word, from three reads and no writes', async ({ page }) => {
    const seen = watch(page);
    await install(page, FIXTURES);
    await mountPage(page, PATH, { titleMatch: /4 vendors/ });

    // Gap 7 (8d564435): 3 open POs = Cascade's placed + in-transit,
    // plus the vendorless draft the unnamed vendor absorbs; po-5 names
    // Prairie by id and is not counted. $1,296 = $1,250 + $45.50
    // rounded to whole dollars; b-4 names Cascade by id and is not
    // counted.
    await expectHeader(page, '4 vendors', OPEN_SUBTITLE);
    await expect(page.locator('.filter-label')).toHaveText(['Search', 'Category', 'State']);
    await expect(searchbox(page)).toHaveAttribute('placeholder', 'Vendor, contact…');
    await expect(page.locator('table.data-table thead th')).toHaveText([
      'Vendor', 'Category', 'Location', 'Terms', 'Lead time', 'Open POs', 'Unpaid bills', 'Outstanding',
    ]);

    expect(await table(page)).toEqual([
      ['Cascade Hop Farm', 'hop supplier', 'Yakima, WA', 'Net 30', '14d', '2/ 3', '1', '$1,250'],
      ['Prairie Maltings', 'grain supplier', 'Great Falls, MT', 'Net 45', '21d', '0', '1', '$45.50'],
      // Gap 5 (d18b68cf): every other null column says `—`; a null
      // payment_terms paints an empty Terms cell.
      ['Anchor Glassworks', '—', '—, —', '', '7d', '0', '0', '—'],
      // A null name: the label falls back to the id, and the NAME join
      // matches the PO whose vendor is null (see ORDER_ROWS).
      ['vnd-bare-004', 'hop supplier', 'Spokane, WA', 'Net 15', '10d', '1/ 1', '0', '—'],
    ]);

    // Categories sorted, hyphens shown as spaces, each with its count;
    // states sorted; the null category and state get no button.
    const filters = page.locator('aside.catalog-filters button.filter-btn');
    await expect(filters).toHaveText([
      'All (4)', 'grain supplier (1)', 'hop supplier (2)', 'All', 'MT', 'WA',
    ]);
    await expect(page.locator('button.filter-btn-active')).toHaveText(['All (4)', 'All']);

    expect(await settledReads(page, () => seen.reads.length, 3)).toBe(3);
    expect([...seen.reads].sort()).toEqual([...READ_PATHS].sort());
    expect(seen.writes).toHaveLength(0);
  });

  test('a list read is still pending: "Loading…"', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await install(page, {
      ...FIXTURES,
      vendors: async (r: Route) => { await held; await json(r, VENDOR_ROWS); },
    });
    await mountPage(page, PATH);

    await expect(empty(page)).toHaveText('Loading…');
    await expect(page.locator('h1.exec-title')).toHaveText('0 vendors');
    release();
    await expect(page.locator('h1.exec-title')).toHaveText('4 vendors');
    await expect(bodyRows(page)).toHaveCount(4);
  });

  test('the Category buttons narrow the rows to their category, and All restores them', async ({ page }) => {
    await install(page, FIXTURES);
    await mountPage(page, PATH, { titleMatch: /4 vendors/ });

    await button(page, 'hop supplier (2)').click();
    expect(await names(page)).toEqual(['Cascade Hop Farm', 'vnd-bare-004']);
    await expect(page.locator('button.filter-btn-active')).toHaveText(['hop supplier (2)', 'All']);
    // A filter narrows the rows only; the header counts every vendor.
    await expectHeader(page, '4 vendors', OPEN_SUBTITLE);

    await button(page, 'grain supplier (1)').click();
    expect(await names(page)).toEqual(['Prairie Maltings']);

    // The vendor with no category is reachable only under All.
    await button(page, 'All (4)').click();
    expect(await names(page)).toEqual([
      'Cascade Hop Farm', 'Prairie Maltings', 'Anchor Glassworks', 'vnd-bare-004',
    ]);
  });

  test('the State buttons narrow the rows to their state, and compose with Category', async ({ page }) => {
    await install(page, FIXTURES);
    await mountPage(page, PATH, { titleMatch: /4 vendors/ });

    await button(page, 'WA').click();
    expect(await names(page)).toEqual(['Cascade Hop Farm', 'vnd-bare-004']);
    await expect(page.locator('button.filter-btn-active')).toHaveText(['All (4)', 'WA']);

    await button(page, 'MT').click();
    expect(await names(page)).toEqual(['Prairie Maltings']);

    // Two filters that exclude each other: the empty line.
    await button(page, 'hop supplier (2)').click();
    await expect(bodyRows(page)).toHaveCount(0);
    await expect(empty(page)).toHaveText('No vendors match those filters.');

    await button(page, 'All').click();
    expect(await names(page)).toEqual(['Cascade Hop Farm', 'vnd-bare-004']);
  });

  test('the search narrows on id, name and contact, case-insensitively, and sends nothing', async ({ page }) => {
    const seen = watch(page);
    await install(page, FIXTURES);
    await mountPage(page, PATH, { titleMatch: /4 vendors/ });
    expect(await settledReads(page, () => seen.reads.length, 3)).toBe(3);

    const search = searchbox(page);
    await search.fill('rhea');
    expect(await names(page)).toEqual(['Cascade Hop Farm']);
    await search.fill('VND-MALT');
    expect(await names(page)).toEqual(['Prairie Maltings']);
    await search.fill('glassworks');
    expect(await names(page)).toEqual(['Anchor Glassworks']);
    await search.fill('no such vendor');
    await expect(empty(page)).toHaveText('No vendors match those filters.');
    await search.fill('');
    await expect(bodyRows(page)).toHaveCount(4);

    // Client-side only: no read beyond the three at mount, no write.
    expect(seen.reads).toHaveLength(3);
    expect(seen.writes).toHaveLength(0);
  });

  test('the vendor link opens the vendor page, and back returns to the list', async ({ page }) => {
    await install(page, FIXTURES);
    await mountPage(page, PATH, { titleMatch: /4 vendors/ });

    const link = bodyRows(page).first().getByRole('link', { name: 'Cascade Hop Farm', exact: true });
    await expect(link).toHaveAttribute('href', '/ux/vendors/vnd-hops-001');
    // The label is the name; the id rides as the tooltip.
    await expect(link.locator('span')).toHaveAttribute('title', 'vnd-hops-001');
    // A null name: the label falls back to the id.
    await expect(bodyRows(page).nth(3).getByRole('link')).toHaveText('vnd-bare-004');
    await expect(bodyRows(page).nth(3).getByRole('link')).toHaveAttribute('href', '/ux/vendors/vnd-bare-004');

    // Gap 6 (e9c8e28d): the router serves the detail path, but the
    // catalog lists only the list route, so no crawl visits it.
    expect(parseRoute('/ux/vendors/vnd-hops-001')).toMatchObject({ kind: 'vendor' });
    const catalogued = Object.values(ROUTE_CATALOG).map((entry) => entry.path);
    expect(catalogued).toContain(PATH);
    expect(catalogued.filter((p) => p.startsWith('/ux/vendors/'))).toEqual([]);

    await link.click();
    await expect.poll(() => new URL(page.url()).pathname).toBe('/ux/vendors/vnd-hops-001');
    await expect(page.locator('h1.detail-title')).toHaveText('Cascade Hop Farm');

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(page.locator('h1.exec-title')).toHaveText('4 vendors');
    await expect(bodyRows(page)).toHaveCount(4);
  });
});

test.describe('/ux/vendors — empty and failed reads', () => {
  test('an empty backend paints the filters line, with no filter set', async ({ page }) => {
    await install(page, { vendors: [], orders: [], invoices: [] });
    await mountPage(page, PATH, { titleMatch: /0 vendors/ });

    await expectHeader(page, '0 vendors', ZERO_SUBTITLE);
    // Gap 4 (35aeb30d): no filter is set, and the line blames filters.
    await expect(page.locator('button.filter-btn-active')).toHaveText(['All (0)', 'All']);
    await expect(searchbox(page)).toHaveValue('');
    await expect(empty(page)).toHaveText('No vendors match those filters.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.locator('table.data-table')).toHaveCount(0);
  });

  test('a refused orders read and a refused invoices read paint as zeros, with no failure line', async ({ page }) => {
    await install(page, { vendors: VENDOR_ROWS, orders: noUpstream, invoices: noUpstream });
    await mountPage(page, PATH, { titleMatch: /4 vendors/ });

    // Gap 1 (223ebcd6): both refusals fold into `[]`. Every row reads
    // as a vendor with no POs and no bills, the header states $0.00,
    // and nothing on the page says two of its three reads failed.
    await expect(bodyRows(page)).toHaveCount(4);
    expect((await table(page)).map((row) => row.slice(5))).toEqual([
      ['0', '0', '—'], ['0', '0', '—'], ['0', '0', '—'], ['0', '0', '—'],
    ]);
    await expectHeader(page, '4 vendors', ZERO_SUBTITLE);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.getByText("Couldn't load vendors")).toHaveCount(0);
  });

  test('a refused vendors read alone still names only vendors', async ({ page }) => {
    await install(page, { ...FIXTURES, vendors: (r: Route) => json(r, 'forbidden', 403) });
    await mountPage(page, PATH);

    await expect(empty(page)).toHaveText("Couldn't load vendors: vendors HTTP 403");
    await expectHeader(page, '0 vendors', ZERO_SUBTITLE);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('an unreachable orders read is reported as the vendors failing', async ({ page }) => {
    await install(page, { ...FIXTURES, orders: (r: Route) => r.abort('connectionrefused') });
    await mountPage(page, PATH);

    // Gap 2 (c3e4edcc): the network rejection lands in the one catch,
    // whose line names vendors whichever read failed.
    await expect(empty(page)).toHaveText(/^Couldn't load vendors: /);
    await expect(page.locator('table.data-table')).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('an invoices body that is not JSON is reported as the vendors failing', async ({ page }) => {
    await install(page, {
      ...FIXTURES,
      invoices: (r: Route) => r.fulfill({ status: 200, contentType: 'application/json', body: 'not json' }),
    });
    await mountPage(page, PATH);

    await expect(empty(page)).toHaveText(/^Couldn't load vendors: /);
    await expect(page.locator('table.data-table')).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });
});
