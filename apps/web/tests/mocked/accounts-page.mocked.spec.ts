// /ux/accounts — "Accounts" (department sales), every control and
// render state pinned as the page behaves TODAY (page audit 030de959,
// step `test`).
//
// Before this spec the route was reached by the generic crawls only
// (route-smoke, interaction-crawl, and outage-crawl, which lists it in
// SILENT): nothing clicked a tier or state button, searched, followed
// an account link, failed ONE of its reads, or rendered it under the
// manifest the live instance serves (MODULES_LIVE, the recorded live
// manifest: `support` is on, so the ticket read is made). This header
// said the live manifest was `"modules":{}` with the ticket read
// skipped; that was true on 2026-09-19 and stopped (41454ce1).
//
// The page, counted (AccountsList.svelte):
//   reads  — 3 with the support module off, 4 with it on (the live
//            instance): the account directory, /api/assets,
//            /api/jobs?department=support (support on only), and
//            /api/commerce/open-ar (the service's per-account sum —
//            it was /api/commerce/invoices?limit=10000 until 5257bfa9);
//   writes — 0;
//   controls — 1 search input; tier buttons: All + one per (account,
//            tier) Class + No tier when an account has none (backlog
//            d2c9e79f: the page hard-coded the seeded three);
//            state buttons: All states + one per distinct state; one
//            account link per row, and the row itself navigates to the
//            same place (the link owns its own click, so that is ONE
//            navigation — backlog 18890a16); up to
//            3 OverflowBanners (accounts, devices, service jobs — the
//            open-AR read is an aggregate and is never truncated).
//
// Lines that pin a FILED gap's current behaviour name the gap: 4ddeb106
// (the page is built for the used-device-shop tenant — a DESIGN question).
// 5257bfa9 (open AR was computed from a capped invoice list client-side)
// is answered: its lines below now pin the answer.
// They are meant to be edited by the car that answers the gap, so the
// answer shows up here as a changed expectation instead of a
// silently-passing one. Lines marked UNFILED pin behaviour this spec
// found and the audit did not list; the car's report names them.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import {
  installSmokeMocks, installTenantManifest, LIVE_MANIFEST_RECORDED_AT, MODULES_LIVE,
} from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG.accounts.path;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const paged = (data: ReadonlyArray<unknown>, limit: number, total = data.length) => ({
  data, total, limit, offset: 0,
});

/// The page's four reads, as AccountsList.svelte spells them. The
/// directory is ACCOUNTS_LIST_URL (src/accounts/api.ts), a paged read
/// since 2d1d298e.
const ACCOUNTS_URL = '/api/people/accounts?limit=1000';
const ASSETS_URL = '/api/assets?limit=1000';
const JOBS_URL = '/api/jobs?department=support&limit=5000';
const OPEN_AR_URL = '/api/commerce/open-ar';
const PAGE_READS: ReadonlySet<string> = new Set([ACCOUNTS_URL, ASSETS_URL, JOBS_URL, OPEN_AR_URL]);

const ACCOUNTS = /\/api\/people\/accounts\?limit=1000$/;
const ASSETS = /\/api\/assets\?limit=1000$/;
const JOBS = /\/api\/jobs\?department=support&limit=5000$/;
const OPEN_AR = /\/api\/commerce\/open-ar$/;

// ── Fixtures ────────────────────────────────────────────────────────

const account = (
  id: string, name: string | null, director: string | null, city: string | null,
  state: string | null, tier: string | null, customer_since: string | null,
) => ({ id, name, director, city, state, tier, customer_since, territory_rep_id: null });

/// The (account, tier) Classes as `GET /api/classes?subject_kind=account`
/// serves them: the three 01-registries.sql seeds, plus a note-kind
/// Class, which is an account Class too and is not a tier. A spec that
/// needs a tenant's fourth tier passes it in `extra`.
const tierClass = (code: string, display_name: string, sort_order: number) => ({
  subject_kind: 'account', code, display_name, parent_code: null,
  member_attribute: 'tier', metadata: {}, sort_order, retired_at: null,
});
const SEEDED_TIERS = [
  tierClass('platinum', 'Platinum', 30), tierClass('gold', 'Gold', 31), tierClass('silver', 'Silver', 32),
];
async function installAccountClasses(page: Page, extra: ReadonlyArray<unknown> = []): Promise<void> {
  await page.route(/\/api\/classes\?subject_kind=account$/, (r) => json(r, [
    { subject_kind: 'account', code: 'call', display_name: 'Call', parent_code: null, member_attribute: 'note-kind', metadata: {}, sort_order: 1, retired_at: null },
    ...SEEDED_TIERS,
    ...extra,
  ]));
}

/// The live directory, read 2026-09-22 by the audit's measure step: ONE
/// account, the shared Anonymous Sponsor the sponsor-reconcile policy
/// folds every unidentified sponsor into. Every descriptive field is
/// unset (identity-first).
const SPONSOR = account('acct-anonymous-sponsor', 'Anonymous Sponsor', null, null, null, null, null);

/// A device-fleet directory, the shape the page was built for (4ddeb106):
/// two enriched accounts in server order, plus the sponsor.
const ANCHOR = account('acct-anchor', 'Anchor Pub', 'Ada Brewer', 'Portland', 'OR', 'gold', '2024-03-01');
const ZED = account('acct-zed', 'Zed Taproom', 'Zoe Tap', 'Seattle', 'WA', 'platinum', '2023-01-15');
const FLEET = [ANCHOR, ZED, SPONSOR];

const asset = (asset_id: string, phase: string, account_id: string | null) => ({
  asset_id, sku: 'DM-FERM-1', phase, account_id, warranty_through: null, open_ticket_count: 0,
  first_seen: '2026-01-01', last_event_at: '2026-09-01', oem_serial: null,
});
/// Three assets: two on Anchor (one installed, one received), one owned
/// by nobody. The subtitle's "installed devices" is the READ's total
/// (3), not the installed count (1) — UNFILED, see the subtitle test.
const ASSETS_BODY = [
  asset('a-1', 'installed', 'acct-anchor'),
  asset('a-2', 'received', 'acct-anchor'),
  asset('a-3', 'installed', null),
];

const job = (id: string, account_id: string, status: string) => ({
  id, kind: 'support-case', title: id, owner_id: 'emp-001', status, priority: 'standard',
  subject: { subject_kind: 'account', id: account_id },
  opened_on: '2026-09-01', due_on: null, closed_on: null, metadata: {}, tags: [],
});
/// Zed has one open case and one closed; Anchor's only case is cancelled.
const JOBS_BODY = [
  job('job-1', 'acct-zed', 'open'),
  job('job-2', 'acct-zed', 'closed'),
  job('job-3', 'acct-anchor', 'cancelled'),
];

/// One row of `GET /api/commerce/open-ar` (AccountOpenAr): the service's
/// sum over every invoice the account still owes.
const openAr = (account_id: string, open_ar_cents: number, open_count: number) => ({
  account_id, open_ar_cents, open_count,
});
/// Zed owes $1,250 on one open invoice; Anchor owes nothing, so the
/// service has no row for it and its Open AR cell is a dash.
const OPEN_AR_BODY = [openAr('acct-zed', 125_000, 1)];

/// The fleet table, cell by cell, in server order.
const FLEET_HEADINGS = [
  'Account', 'Tier', 'Location', 'Primary contact', 'Equipment', 'Open SRs', 'Open AR', 'Customer since',
];
const FLEET_ROWS: ReadonlyArray<ReadonlyArray<string>> = [
  ['Anchor Pub', 'Gold', 'Portland, OR', 'Ada Brewer', '2', '0', '—', '2024-03-01'],
  ['Zed Taproom', 'Platinum', 'Seattle, WA', 'Zoe Tap', '0', '1', '$1,250', '2023-01-15'],
  ['Anonymous Sponsor', 'untiered', '—, —', '—', '0', '0', '—', '—'],
];
const FLEET_NAMES = FLEET_ROWS.map((r) => r[0]!);

// ── Mocks ───────────────────────────────────────────────────────────

/// The shell (installSmokeMocks' MODULES_ON, so support is on) and the
/// four reads answered with the fleet; a spec re-routes any one after.
async function installFleet(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await installAccountClasses(page);
  await page.route(ACCOUNTS, (r) => json(r, paged(FLEET, 1000)));
  await page.route(ASSETS, (r) => json(r, paged(ASSETS_BODY, 1000)));
  await page.route(JOBS, (r) => json(r, paged(JOBS_BODY, 5000)));
  await page.route(OPEN_AR, (r) => json(r, paged(OPEN_AR_BODY, OPEN_AR_BODY.length)));
}

/// The live instance: the manifest it serves (MODULES_LIVE, support on),
/// INLINED as the gateway inlines it into index.html
/// (`window.__BOSS_TENANT_MANIFEST__`, 5578e42d) so the shell is ready
/// before first paint, and the live directory. Assets and open AR
/// answer empty: neither service is on the machine door's port table,
/// so their live volume is undetermined (the audit's measure step), and
/// empty is the honest stand-in. The ticket read answers empty because
/// it IS empty: `/api/jobs?department=support&limit=5000` answered
/// total 0 on 2026-09-24, on the read whose accounts control answered
/// the one sponsor below.
async function installLive(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await installTenantManifest(page, MODULES_LIVE, { inline: true });
  await installAccountClasses(page);
  await page.route(ACCOUNTS, (r) => json(r, paged([SPONSOR], 1000)));
  await page.route(ASSETS, (r) => json(r, paged([], 1000)));
  await page.route(JOBS, (r) => json(r, paged([], 5000)));
  await page.route(OPEN_AR, (r) => json(r, paged([], 0)));
}

/// The shell's own non-GET: App.svelte records every route open
/// (shell/surface-opens.ts). It is the chrome's write, not this page's.
const SHELL_WRITES: ReadonlySet<string> = new Set(['/api/surface-opens']);

/// Every read of the page's four URLs, and every non-GET it sends.
function watch(page: Page): { reads: string[]; writes: Request[] } {
  const seen = { reads: [] as string[], writes: [] as Request[] };
  page.on('request', (req) => {
    const url = new URL(req.url());
    if (!url.pathname.startsWith('/api/')) return;
    if (req.method() !== 'GET') {
      if (!SHELL_WRITES.has(url.pathname)) seen.writes.push(req);
      return;
    }
    const read = `${url.pathname}${url.search}`;
    if (PAGE_READS.has(read)) seen.reads.push(read);
  });
  return seen;
}

const body = (page: Page) => page.locator('.catalog');
const title = (page: Page) => body(page).locator('h1.exec-title');
const subtitle = (page: Page) => body(page).locator('header p');
const group = (page: Page, i: number) => body(page).locator('.filter-group').nth(i);
const button = (page: Page, name: string) => body(page).getByRole('button', { name, exact: true });
const search = (page: Page) => body(page).getByPlaceholder('Account, doctor, city…');
const nameColumn = (page: Page) => body(page).locator('tbody tr td:first-child');
const status = (page: Page) => body(page).locator('.list-section p.empty');
const banners = (page: Page) => body(page).locator('.overflow-banner');
const bannerText = (page: Page) => banners(page).locator('.text');

async function mountFleet(page: Page): Promise<void> {
  await mountPage(page, PATH);
  await expect(body(page).locator('tbody tr')).toHaveCount(FLEET_ROWS.length);
}

// ── The live instance ───────────────────────────────────────────────

test.describe(`/ux/accounts — the live instance (the manifest recorded ${LIVE_MANIFEST_RECORDED_AT}, one account)`, () => {
  test('mount makes the four reads, the ticket read among them, writes nothing, and paints one row', async ({ page }) => {
    expect(MODULES_LIVE['support'], 'the recording lists support').toBe(true);
    const seen = watch(page);
    await installLive(page);
    await mountPage(page, PATH);
    await expect(body(page).locator('tbody tr')).toHaveCount(1);

    expect(await settledReads(page, () => seen.reads.length, 4)).toBe(4);
    expect([...seen.reads].sort()).toEqual([...PAGE_READS].sort());
    // Gap 4ddeb106: three of the four reads are a device fleet's (assets),
    // a service desk's (tickets) and an AR ledger's (open AR); on this
    // instance they answer nothing an account row shows, so their four
    // columns stay hidden.

    await expect(page.locator('.module-disabled')).toHaveCount(0);
    await expect(body(page).locator('.exec-eyebrow')).toHaveText('Customers');
    // UNFILED: the count is not pluralised — "1 accounts".
    await expect(title(page)).toHaveText('1 accounts');
    await expect(subtitle(page)).toHaveCount(0);
    await expect(body(page).locator('.filter-label')).toHaveText(['Search', 'Tier', 'State']);
    await expect(group(page, 1).getByRole('button')).toHaveText([
      'All (1)', 'Platinum (0)', 'Gold (0)', 'Silver (0)', 'No tier (1)',
    ]);
    await expect(group(page, 2).getByRole('button')).toHaveText(['All states']);
    await expect(body(page).getByRole('button')).toHaveCount(6);
    await expect(body(page).locator('.filter-btn-active')).toHaveText(['All (1)', 'All states']);
    await expect(search(page)).toHaveValue('');

    await expect(body(page).locator('thead th')).toHaveText([
      'Account', 'Tier', 'Location', 'Primary contact', 'Customer since',
    ]);
    await expect(body(page).locator('tbody tr').first().locator('td')).toHaveText([
      'Anonymous Sponsor', 'untiered', '—, —', '—', '—',
    ]);
    await expect(banners(page)).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });

  // Backlog d2c9e79f: the untiered sponsor was reachable only under All,
  // because the page's three hand-written buttons had no No tier.
  test('every tier Class button empties the list, and No tier shows the untiered sponsor', async ({ page }) => {
    await installLive(page);
    await mountPage(page, PATH);
    await expect(nameColumn(page)).toHaveText(['Anonymous Sponsor']);

    for (const label of ['Platinum (0)', 'Gold (0)', 'Silver (0)']) {
      await button(page, label).click();
      await expect(body(page).locator('.filter-btn-active').first()).toHaveText(label);
      await expect(status(page), label).toHaveText('No accounts match those filters.');
    }
    await button(page, 'No tier (1)').click();
    await expect(nameColumn(page)).toHaveText(['Anonymous Sponsor']);
    await button(page, 'All (1)').click();
    await expect(nameColumn(page)).toHaveText(['Anonymous Sponsor']);
  });
});

// ── The fleet shape: the list ───────────────────────────────────────

test.describe('/ux/accounts — the fleet shape (support on): the list', () => {
  test('mount makes the four reads once, writes nothing, and paints every column and row', async ({ page }) => {
    const seen = watch(page);
    await installFleet(page);
    await mountFleet(page);

    expect(await settledReads(page, () => seen.reads.length, 4)).toBe(4);
    expect([...seen.reads].sort()).toEqual([...PAGE_READS].sort());

    await expect(title(page)).toHaveText('3 accounts');
    // UNFILED: "installed devices" is /api/assets' total — every asset in
    // every phase, owned by any account or none (3 here) — not the
    // installed count (1); "service jobs" is the support department's
    // total including the closed and the cancelled (3), not open ones.
    await expect(subtitle(page)).toHaveText('3 installed devices · 3 service jobs');
    await expect(group(page, 1).getByRole('button')).toHaveText([
      'All (3)', 'Platinum (1)', 'Gold (1)', 'Silver (0)', 'No tier (1)',
    ]);
    await expect(group(page, 2).getByRole('button')).toHaveText(['All states', 'OR (1)', 'WA (1)']);

    await expect(body(page).locator('thead th')).toHaveText(FLEET_HEADINGS);
    const rows = body(page).locator('tbody tr');
    for (const [i, cells] of FLEET_ROWS.entries()) {
      await expect(rows.nth(i).locator('td')).toHaveText([...cells]);
    }
    // Only the non-zero cells are emphasised.
    await expect(rows.nth(1).locator('td').nth(5).locator('strong')).toHaveText('1');
    await expect(rows.nth(1).locator('td').nth(6).locator('strong')).toHaveText('$1,250');
    await expect(banners(page)).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });

  // Backlog 5257bfa9, answered: Open AR was summed in the browser from
  // `/api/commerce/invoices?limit=10000` — a list the service clamps to
  // 1,000 rows — so past a thousand invoices the money figure was short
  // and no banner said so. The service sums every invoice now
  // (boss-commerce tests/open_ar.rs pins the sum past the clamp); the
  // page shows the account's figure as served and reads no invoice list.
  test('Open AR is the service\'s per-account sum, read once, and no invoice list is read', async ({ page }) => {
    const seen = watch(page);
    const invoiceLists: string[] = [];
    page.on('request', (req) => {
      const url = new URL(req.url());
      if (url.pathname === '/api/commerce/invoices') invoiceLists.push(`${url.pathname}${url.search}`);
    });
    await installFleet(page);
    await page.route(OPEN_AR, (r) => json(r, paged([openAr('acct-zed', 127_550, 2)], 1)));
    await mountFleet(page);
    // 127,550 cents, whole-dollar precision.
    await expect(body(page).locator('tbody tr').nth(1).locator('td').nth(6)).toHaveText('$1,276');
    expect(await settledReads(page, () => seen.reads.length, 4)).toBe(4);
    expect(seen.reads.filter((r) => r === OPEN_AR_URL)).toHaveLength(1);
    expect(invoiceLists).toEqual([]);
  });

  test('columns whose value is zero for every row are hidden, one by one', async ({ page }) => {
    await installFleet(page);
    await page.route(ASSETS, (r) => json(r, paged([asset('a-3', 'installed', null)], 1000)));
    await page.route(JOBS, (r) => json(r, paged([job('job-2', 'acct-zed', 'closed')], 5000)));
    await page.route(OPEN_AR, (r) => json(r, paged([], 0)));
    await mountFleet(page);

    await expect(body(page).locator('thead th')).toHaveText([
      'Account', 'Tier', 'Location', 'Primary contact', 'Customer since',
    ]);
    await expect(subtitle(page)).toHaveCount(0);
  });

  test('with support off the ticket read is never made and Open SRs never shows', async ({ page }) => {
    const seen = watch(page);
    await installFleet(page);
    await installTenantManifest(page, {}, { inline: true });
    await mountFleet(page);

    expect(await settledReads(page, () => seen.reads.length, 3)).toBe(3);
    expect(seen.reads).not.toContain(JOBS_URL);
    await expect(body(page).locator('thead th')).toHaveText([
      'Account', 'Tier', 'Location', 'Primary contact', 'Equipment', 'Open AR', 'Customer since',
    ]);
    await expect(subtitle(page)).toHaveText('3 installed devices');
  });
});

// ── The filter buttons ──────────────────────────────────────────────

test.describe('/ux/accounts — every filter button does what its label says', () => {
  test('each tier and state button narrows the table to its set, client-side, without a read or a history entry', async ({ page }) => {
    const seen = watch(page);
    await installFleet(page);
    await mountFleet(page);
    expect(await settledReads(page, () => seen.reads.length, 4)).toBe(4);
    const depth = await page.evaluate(() => window.history.length);

    for (const [label, names] of [
      ['Platinum (1)', ['Zed Taproom']],
      ['Gold (1)', ['Anchor Pub']],
      ['No tier (1)', ['Anonymous Sponsor']],
      ['All (3)', FLEET_NAMES],
    ] as const) {
      await button(page, label).click();
      await expect(group(page, 1).locator('.filter-btn-active')).toHaveText(label);
      await expect(nameColumn(page), label).toHaveText([...names]);
    }
    for (const [label, names] of [
      ['OR (1)', ['Anchor Pub']],
      ['WA (1)', ['Zed Taproom']],
      ['All states', FLEET_NAMES],
    ] as const) {
      await button(page, label).click();
      await expect(group(page, 2).locator('.filter-btn-active')).toHaveText(label);
      await expect(nameColumn(page), label).toHaveText([...names]);
    }

    expect(seen.reads, 'filters are client-side').toHaveLength(4);
    expect(new URL(page.url()).pathname).toBe(PATH);
    expect(new URL(page.url()).search).toBe('');
    expect(await page.evaluate(() => window.history.length)).toBe(depth);
    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });

  test('tier and state compose; a combination that matches nothing says so, and the counts stay over every row', async ({ page }) => {
    await installFleet(page);
    await mountFleet(page);
    await button(page, 'Gold (1)').click();
    await button(page, 'WA (1)').click();
    await expect(status(page)).toHaveText('No accounts match those filters.');
    await expect(body(page).locator('table')).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(button(page, 'Gold (1)')).toBeVisible();
    await expect(button(page, 'WA (1)')).toBeVisible();

    await button(page, 'Silver (0)').click();
    await button(page, 'All states').click();
    await expect(status(page)).toHaveText('No accounts match those filters.');
  });
});

// ── The search input ────────────────────────────────────────────────

test.describe('/ux/accounts — the search input', () => {
  test('matches name, primary contact and city, case-insensitively, and submits nothing', async ({ page }) => {
    const seen = watch(page);
    await installFleet(page);
    await mountFleet(page);
    expect(await settledReads(page, () => seen.reads.length, 4)).toBe(4);

    // Gap 4ddeb106: the placeholder names a "doctor" — the device-shop
    // tenant's customer. The field it searches is `director`, shown as
    // "Primary contact".
    await expect(search(page)).toHaveAttribute('type', 'search');

    for (const [q, names] of [
      ['anchor', ['Anchor Pub']], //        name
      ['ZOE', ['Zed Taproom']], //          primary contact, any case
      ['seattle', ['Zed Taproom']], //      city
      ['sponsor', ['Anonymous Sponsor']],
      // State is NOT searched: "wa" finds nothing in WA's row.
      ['wa', []],
    ] as const) {
      await search(page).fill(q);
      if (names.length === 0) {
        await expect(status(page), q).toHaveText('No accounts match those filters.');
      } else {
        await expect(nameColumn(page), q).toHaveText([...names]);
      }
    }
    await search(page).fill('');
    await expect(nameColumn(page)).toHaveText(FLEET_NAMES);

    expect(seen.reads, 'search is client-side').toHaveLength(4);
    expect(new URL(page.url()).search).toBe('');
    expect(seen.writes.map((r) => `${r.method()} ${r.url()}`)).toEqual([]);
  });
});

// ── The account link, the row, and back ────────────────────────────

/// The account detail's reads (loadAccountBundle), answered empty and
/// well-formed so the detail page paints its header.
async function installDetail(page: Page, id: string, row: ReturnType<typeof account>): Promise<void> {
  const q = encodeURIComponent(id);
  await page.route(new RegExp(`/api/people/accounts/${q}$`), (r) => json(r, row));
  for (const re of [
    new RegExp(`/api/assets\\?account_id=${q}&`),
    new RegExp(`/api/commerce/invoices\\?account_id=${q}&`),
    new RegExp(`/api/jobs\\?subject_id=${q}&`),
    new RegExp(`/api/shipping/shipments\\?account_id=${q}&`),
  ]) {
    await page.route(re, (r) => json(r, paged([], 500)));
  }
}

test.describe('/ux/accounts — the account link, the row, and back', () => {
  // Backlog 18890a16: the link sits inside a row that navigates on
  // click, and Link.svelte prevented the default without stopping the
  // click bubbling, so ONE click on the name navigated twice — two
  // history entries for the same account — and the first Back landed
  // on the account again. The link now owns its click: one entry, one
  // Back.
  test('every row links its account under the catalogued accounts path; following one lands on it once, and one Back returns', async ({ page }) => {
    await installFleet(page);
    await installDetail(page, 'acct-zed', ZED);
    await mountFleet(page);

    const links = body(page).locator('tbody a');
    await expect(links).toHaveCount(FLEET.length);
    for (const [i, a] of FLEET.entries()) {
      await expect(links.nth(i)).toHaveText(a.name ?? a.id);
      await expect(links.nth(i)).toHaveAttribute('href', `${PATH}/${encodeURIComponent(a.id)}`);
    }
    // The one link kind on the page.
    await expect(body(page).locator('a')).toHaveCount(FLEET.length);

    const depth = await page.evaluate(() => window.history.length);
    await body(page).getByRole('link', { name: 'Zed Taproom', exact: true }).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${PATH}/acct-zed`);
    await expect(page.locator('h1.exec-title')).toHaveText('Zed Taproom');
    expect(await page.evaluate(() => window.history.length)).toBe(depth + 1);

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(nameColumn(page)).toHaveText(FLEET_NAMES);
  });

  // The modified-click half of the same bubbling: a ctrl/cmd-click on
  // the name is the browser's open-in-new-tab, which Link lets through
  // — and the row, reached by the same click, used to navigate THIS
  // tab as well. The link's click is the link's alone either way.
  test('a modified click on the account link leaves this tab on the list', async ({ page }) => {
    await installFleet(page);
    await installDetail(page, 'acct-zed', ZED);
    await mountFleet(page);

    const depth = await page.evaluate(() => window.history.length);
    const popup = page.context().waitForEvent('page');
    await body(page)
      .getByRole('link', { name: 'Zed Taproom', exact: true })
      .click({ modifiers: ['ControlOrMeta'] });
    await (await popup).close();
    expect(new URL(page.url()).pathname).toBe(PATH);
    expect(await page.evaluate(() => window.history.length)).toBe(depth);
    await expect(nameColumn(page)).toHaveText(FLEET_NAMES);
  });

  test('a click anywhere else on the row lands on the same account once, and one Back returns', async ({ page }) => {
    await installFleet(page);
    await installDetail(page, 'acct-anchor', ANCHOR);
    await mountFleet(page);

    const row = body(page).locator('tbody tr').first();
    await expect(row).toHaveClass(/data-table-row-link/);
    const depth = await page.evaluate(() => window.history.length);
    await row.locator('td').nth(2).click();
    await expect.poll(() => page.evaluate(() => window.history.length)).toBe(depth + 1);
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${PATH}/acct-anchor`);
    await expect(page.locator('h1.exec-title')).toHaveText('Anchor Pub');

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(nameColumn(page)).toHaveText(FLEET_NAMES);
  });
});

// ── A tier a tenant added ───────────────────────────────────────────

// Backlog d2c9e79f: `accounts.tier` is plain TEXT (22-accounts.sql) and
// a tier is an (account, tier) Class row, so a tenant adds one without
// a deploy. The web typed it as platinum | gold | silver everywhere: the
// account detail's schema refused the account ("Server returned an
// unexpected payload shape"), and the list had no button for it.
test.describe('/ux/accounts — a fourth tier, added as one Class row', () => {
  const BRONZE = account('acct-bronze', 'Bronze Brewhouse', 'Bo Barley', 'Boise', 'ID', 'bronze', '2025-05-05');

  async function installBronze(page: Page): Promise<void> {
    await installFleet(page);
    await installAccountClasses(page, [tierClass('bronze', 'Bronze Reserve', 33)]);
    await page.route(ACCOUNTS, (r) => json(r, paged([...FLEET, BRONZE], 1000)));
  }

  test('the list gives it a counted button, in registry order, and labels its chip from the Class', async ({ page }) => {
    await installBronze(page);
    await mountPage(page, PATH);
    await expect(group(page, 1).getByRole('button')).toHaveText([
      'All (4)', 'Platinum (1)', 'Gold (1)', 'Silver (0)', 'Bronze Reserve (1)', 'No tier (1)',
    ]);
    await button(page, 'Bronze Reserve (1)').click();
    await expect(nameColumn(page)).toHaveText(['Bronze Brewhouse']);
    await expect(body(page).locator('tbody tr td').nth(1)).toHaveText('Bronze Reserve');
  });

  test('the account detail loads it and shows the Class label', async ({ page }) => {
    await installBronze(page);
    await installDetail(page, 'acct-bronze', BRONZE);
    await mountPage(page, `${PATH}/acct-bronze`);
    await expect(page.locator('h1.exec-title')).toHaveText('Bronze Brewhouse');
    await expect(page.locator('dl.kv dt', { hasText: 'Tier' }).locator('xpath=following-sibling::dd[1]'))
      .toHaveText('Bronze Reserve');
  });
});

// ── Overflow banners ────────────────────────────────────────────────

test.describe('/ux/accounts — a capped read says so', () => {
  test('a capped directory, device read and ticket read each raise their banner', async ({ page }) => {
    await installFleet(page);
    await page.route(ACCOUNTS, (r) => json(r, paged(FLEET, 1000, 1_500)));
    await page.route(ASSETS, (r) => json(r, paged(ASSETS_BODY, 1000, 2_000)));
    await page.route(JOBS, (r) => json(r, paged(JOBS_BODY, 5000, 6_000)));
    await mountFleet(page);

    await expect(bannerText(page)).toHaveText([
      'Showing 3 of 1,500 accounts. The list and its filters cover only the accounts loaded.',
      'Showing 3 of 2,000 installed devices loaded for per-account rollups. Per-row device counts may undercount; raise the cap or filter.',
      'Showing 3 of 6,000 service jobs loaded for per-account rollups. Per-row open-ticket counts may undercount; raise the cap or filter.',
    ]);
    await expect(banners(page).first()).toHaveAttribute('role', 'status');
    // The title counts the rows loaded, not the directory's total.
    await expect(title(page)).toHaveText('3 accounts');
  });

  // Backlog 5257bfa9 pinned here that a capped invoices read raised no
  // banner, so Open AR was short in silence. There is no capped read
  // left to disclose: the open-AR read is one row per owing account,
  // summed by the service over every invoice, and is never truncated
  // (the Open AR test above pins that no invoice list is read).
});

// ── Empty, loading, and failed reads ────────────────────────────────

test.describe('/ux/accounts — empty, loading, and a failed read', () => {
  // UNFILED: an empty directory paints the filter-mismatch sentence,
  // though no filter is set.
  test('an empty backend paints the filters sentence and a zero header, not a failure', async ({ page }) => {
    await installFleet(page);
    await page.route(ACCOUNTS, (r) => json(r, paged([], 1000)));
    await page.route(ASSETS, (r) => json(r, paged([], 1000)));
    await page.route(JOBS, (r) => json(r, paged([], 5000)));
    await page.route(OPEN_AR, (r) => json(r, paged([], 0)));
    await mountPage(page, PATH);

    await expect(status(page)).toHaveText('No accounts match those filters.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(body(page).locator('table')).toHaveCount(0);
    await expect(title(page)).toHaveText('0 accounts');
    await expect(subtitle(page)).toHaveCount(0);
    await expect(body(page).getByRole('button')).toHaveText([
      'All (0)', 'Platinum (0)', 'Gold (0)', 'Silver (0)', 'All states',
    ]);
  });

  test('a pending directory read paints "Loading…" beside a zero header', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await installFleet(page);
    await page.route(ACCOUNTS, async (r) => {
      await held;
      await json(r, paged(FLEET, 1000));
    });
    await mountPage(page, PATH);

    await expect(status(page)).toHaveText('Loading…');
    await expect(title(page)).toHaveText('0 accounts');
    await expect(button(page, 'All (0)')).toBeVisible();
    release();
    await expect(nameColumn(page)).toHaveText(FLEET_NAMES);
    await expect(page.getByText('Loading…', { exact: true })).toHaveCount(0);
  });

  // Sweep c3e4edcc, answered: the failure line was honest in its WORDS
  // but painted as the empty state — `p.empty`, no FAILURE_MARKER and no
  // role=alert, so outage-crawl listed this route in SILENT — under a
  // header that counted "0 accounts". It wears the marker now, and the
  // header states no count.
  test('a failed directory read names the read, on the shared failure marker', async ({ page }) => {
    await installFleet(page);
    await page.route(ACCOUNTS, (r) => json(r, { error: 'people down' }, 503));
    await mountPage(page, PATH);

    await expect(status(page)).toHaveText(`Couldn't load accounts: ${ACCOUNTS_URL}: HTTP 503`);
    await expect(page.getByText('No accounts match those filters.')).toHaveCount(0);
    await expect(body(page).locator('table')).toHaveCount(0);
    await expect(body(page).locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      `Couldn't load accounts: ${ACCOUNTS_URL}: HTTP 503`,
    );
    await expect(title(page)).toHaveText('Accounts');
    await expect(subtitle(page)).toHaveText('Account count unknown — the read failed');
  });

  test('a network failure of the directory names the browser\'s own message', async ({ page }) => {
    await installFleet(page);
    await page.route(ACCOUNTS, (r) => r.abort('failed'));
    await mountPage(page, PATH);
    await expect(status(page)).toHaveText("Couldn't load accounts: Failed to fetch");
    await expect(body(page).locator('table')).toHaveCount(0);
  });

  // Backlogs 223ebcd6 / e30ee8b9, answered: the three secondary reads
  // still degrade their column by hiding it — there is nothing true to
  // put in it — but each now says so with the shared failure line,
  // naming the read and the column. Before, the page painted "no
  // account has any", and a failed invoices read read as "nobody owes
  // anything".
  for (const [name, re, url, what, gone] of [
    ['GET /api/assets', ASSETS, ASSETS_URL, 'installed devices', 'Equipment'],
    ['GET /api/jobs?department=support', JOBS, JOBS_URL, 'service jobs', 'Open SRs'],
    ['GET /api/commerce/open-ar', OPEN_AR, OPEN_AR_URL, 'open receivables', 'Open AR'],
  ] as const) {
    test(`a failed ${name} hides the ${gone} column and says why`, async ({ page }) => {
      await installFleet(page);
      await page.route(re, (r) => json(r, { error: 'down' }, 503));
      await mountFleet(page);

      const headings = FLEET_HEADINGS.filter((h) => h !== gone);
      await expect(body(page).locator('thead th')).toHaveText(headings);
      await expect(nameColumn(page)).toHaveText(FLEET_NAMES);
      await expect(body(page).locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
        `Couldn't load ${what} — ${url}: HTTP 503. The ${gone} column is not shown: its counts are unknown, not zero.`,
      );
    });
  }
});
