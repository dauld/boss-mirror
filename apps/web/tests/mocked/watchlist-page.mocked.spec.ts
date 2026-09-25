// /watchlist — "Churn watchlist" (department sales), every control and
// render state pinned as the page behaves TODAY (page audit 08b0c4f8,
// step `test`).
//
// Before this spec no spec clicked a Risk bucket button, sorted a
// column, typed a search that matched, or followed the account link
// (gap 13, backlog 8c5664ea): route-smoke paints the chrome and the
// interaction crawl clicks whatever an empty mock paints. Three sibling
// specs each pin ONE state and are not repeated here:
//   watchlist-denied           — a 403 is a refusal, not an empty list
//   watchlist-directory-read   — a failed or capped accounts directory
//   watchlist-tier-filter      — the Tier buttons come from the Classes
//
// The page, counted (WatchlistPage.svelte):
//   links    — 1 kind: the account name, one per row, under the
//              catalogued accounts path;
//   buttons  — 4 Risk bucket (All, High 50+, Mid 25–49, Low 0–24), Tier
//              All + one per (account, tier) Class + No tier when a
//              scored account has none, 5 sortable column headers (Top
//              factor and Contract do not sort);
//   inputs   — 1 search (account, top factor, city), no form;
//   reads    — the risk scores, the accounts directory, the account
//              Classes (for the Tier buttons);
//   writes   — 0, so there is no refused write to show; the watch below
//              pins that the page sends none.
//
// The five sort headers were `<th onclick>` with no role, tabindex or
// key handling, so a keyboard could not sort (gap 13, 8c5664ea — its
// fix_shape asks for the fix in this car). They are now the shared
// SortHeader (libs/web-kit), which was extracted FROM this page's
// hand-rolled sort and never adopted back by it.
//
// Lines that pin a FILED gap's current behaviour name the gap and its
// item, to be edited by the car that answers it, so the answer shows up
// here as a changed expectation instead of a silently-passing one:
//   gap 6  9289e682  the empty line blames the filters with none set
//   gap 8  9269d612  a 200-row cap is presented as "N accounts scored"
//   gap 12 8ddaefcd  no as-of time on the scores

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { parseRoute } from '../../src/router';

const PATH = ROUTE_CATALOG.watchlist.path;
const ACCOUNTS_PATH = ROUTE_CATALOG.accounts.path;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// The page's two own reads, as WatchlistPage.svelte spells them.
const RISK_URL = '/api/people/accounts/risk-scores?limit=200&min_score=0';
const DIRECTORY_URL = '/api/people/accounts?limit=1000';
const RISK = /\/api\/people\/accounts\/risk-scores\?/;
const DIRECTORY = /\/api\/people\/accounts\?limit=1000$/;

// ── Fixtures ────────────────────────────────────────────────────────

const account = (id: string, name: string, city: string | null, tier: string | null) => ({
  id, name, director: null, city, state: null, tier, customer_since: null, territory_rep_id: null,
});

/// The directory: three tiered accounts and the live instance's one
/// account, the untiered Anonymous Sponsor.
const ZED = account('acct-zed', 'Zed Taproom', 'Seattle', 'platinum');
const ANCHOR = account('acct-anchor', 'Anchor Pub', 'Portland', 'gold');
const MILL = account('acct-mill', 'Millrace Tavern', 'Bend', 'silver');
const SPONSOR = account('acct-anonymous-sponsor', 'Anonymous Sponsor', null, null);
const DIRECTORY_ROWS = [ZED, ANCHOR, MILL, SPONSOR];

const tierClass = (code: string, display_name: string, sort_order: number) => ({
  subject_kind: 'account', code, display_name, parent_code: null,
  member_attribute: 'tier', metadata: {}, sort_order, retired_at: null,
});
/// The three seeded tier Classes (01-registries.sql).
const TIER_CLASSES = [
  tierClass('platinum', 'Platinum', 30), tierClass('gold', 'Gold', 31), tierClass('silver', 'Silver', 32),
];

const score = (
  a: { id: string; name: string }, n: number, top_factor: string,
  invoice: number | null, tickets: number, contract: boolean, note: number | null,
) => ({
  account_id: a.id, account_name: a.name, score: n, top_factor,
  factors: {
    days_since_last_invoice: invoice, open_ticket_count: tickets,
    has_active_contract: contract, days_since_last_note: note,
  },
});

/// Two High, one Mid, one Low. Every sortable column holds four
/// DISTINCT values (a null counts once), so each sort has one right
/// order. Served out of score order, so the default sort is the page's.
const SCORES = [
  score(ANCHOR, 55, 'open service tickets', 30, 5, true, 7),
  score(SPONSOR, 4, 'no recent invoice', null, 0, false, null),
  score(ZED, 82, 'no invoice in 120 days', 120, 3, false, 45),
  score(MILL, 31, 'no contact in 90 days', 10, 1, true, 90),
];

const HEADINGS = [
  'Account', 'Score ↓', 'Top factor', 'Days since invoice', 'Open SRs', 'Contract', 'Days since contact',
];
/// The table, cell by cell, in the default order (Score, descending).
const TABLE = [
  ['Zed Taproom', '82', 'no invoice in 120 days', '120d', '3', 'none', '45d'],
  ['Anchor Pub', '55', 'open service tickets', '30d', '5', 'active', '7d'],
  ['Millrace Tavern', '31', 'no contact in 90 days', '10d', '1', 'active', '90d'],
  ['Anonymous Sponsor', '4', 'no recent invoice', '—', '0', 'none', '—'],
];

// ── Mocks and readers ───────────────────────────────────────────────

type Reads = Readonly<{ risk?: (r: Route) => Promise<void>; directory?: (r: Route) => Promise<void> }>;

async function install(page: Page, reads: Reads = {}): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/classes\?subject_kind=account$/, (r) => json(r, TIER_CLASSES));
  await page.route(RISK, reads.risk ?? ((r) => json(r, { accounts: SCORES, total_scored: SCORES.length })));
  await page.route(DIRECTORY, reads.directory ?? ((r) =>
    json(r, { data: DIRECTORY_ROWS, total: DIRECTORY_ROWS.length, limit: 1000, offset: 0 })));
}

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
    const pathAndQuery = `${url.pathname}${url.search}`;
    if (pathAndQuery === RISK_URL || pathAndQuery === DIRECTORY_URL) seen.reads.push(pathAndQuery);
  });
  return seen;
}

const bodyRows = (page: Page) => page.locator('table.risk-table tbody tr');
const empty = (page: Page) => page.locator('section.list-section p.empty');
const button = (page: Page, name: string) => page.getByRole('button', { name, exact: true });
const header = (page: Page, name: string) =>
  page.locator('table.risk-table thead th').filter({ hasText: new RegExp(`^\\s*${name}(\\s|$)`) });
const searchbox = (page: Page) => page.locator('aside.catalog-filters').getByRole('searchbox');

async function table(page: Page): Promise<string[][]> {
  return bodyRows(page).evaluateAll((rows) =>
    rows.map((row) => Array.from(row.children).map((td) => (td as HTMLElement).innerText.trim())),
  );
}
async function names(page: Page): Promise<string[]> {
  return (await table(page)).map((row) => row[0]!);
}

async function expectHeader(page: Page, eyebrow: string, title: string, subtitle?: string): Promise<void> {
  await expect(page.locator('.exec-eyebrow')).toHaveText(eyebrow);
  await expect(page.locator('h1.exec-title')).toHaveText(title);
  if (subtitle !== undefined) await expect(page.locator('.exec-header p')).toHaveText(subtitle);
}

async function mountScored(page: Page): Promise<void> {
  await mountPage(page, PATH, { titleMatch: /^Churn watchlist$/ });
  await expect(bodyRows(page)).toHaveCount(SCORES.length);
}

// ── The render ──────────────────────────────────────────────────────

test.describe('/watchlist — fixtures', () => {
  test('renders every column, count and word, from its reads and no writes', async ({ page }) => {
    const seen = watch(page);
    await install(page);
    await mountScored(page);

    // Gap 12 (8ddaefcd): the subtitle is the whole of what the header
    // says about the scores — no as-of time.
    await expectHeader(page, 'Customers', 'Churn watchlist', '4 accounts scored · 4 shown');
    await expect(page.locator('.filter-label')).toHaveText(['Search', 'Risk bucket', 'Tier']);
    await expect(searchbox(page)).toHaveAttribute('placeholder', 'Account, factor, city…');
    await expect(page.locator('table.risk-table thead th')).toHaveText(HEADINGS);
    expect(await table(page)).toEqual(TABLE);

    // The score chip's tone is the bucket the score falls in.
    await expect(bodyRows(page).locator('.risk-chip')).toHaveClass([
      /risk-chip-high/, /risk-chip-high/, /risk-chip-mid/, /risk-chip-low/,
    ]);

    await expect(page.locator('aside.catalog-filters button.filter-btn')).toHaveText([
      'All (4)', 'High 50+ (2)', 'Mid 25–49 (1)', 'Low 0–24 (1)',
      'All', 'Platinum (1)', 'Gold (1)', 'Silver (1)', 'No tier (1)',
    ]);
    await expect(page.locator('button.filter-btn-active')).toHaveText(['All (4)', 'All']);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    // Two reads of its own, each once, and nothing written.
    expect(await settledReads(page, () => seen.reads.length, 2)).toBe(2);
    expect([...seen.reads].sort()).toEqual([DIRECTORY_URL, RISK_URL].sort());
    expect(seen.writes).toHaveLength(0);
  });

  test('while the scores are pending the header says "Loading…"', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => { release = resolve; });
    await install(page, { risk: async (r) => { await held; await json(r, { accounts: SCORES }); } });
    await mountPage(page, PATH);

    await expectHeader(page, 'Churn watchlist', 'Loading…');
    await expect(page.locator('table.risk-table')).toHaveCount(0);
    release();
    await expectHeader(page, 'Customers', 'Churn watchlist', '4 accounts scored · 4 shown');
    await expect(bodyRows(page)).toHaveCount(4);
  });
});

// ── Risk bucket buttons ─────────────────────────────────────────────

test.describe('/watchlist — the Risk bucket buttons', () => {
  test('each bucket shows exactly the scores its label names, and All restores them', async ({ page }) => {
    await install(page);
    await mountScored(page);

    await button(page, 'High 50+ (2)').click();
    expect(await names(page)).toEqual(['Zed Taproom', 'Anchor Pub']);
    await expect(page.locator('button.filter-btn-active')).toHaveText(['High 50+ (2)', 'All']);
    // The subtitle counts what is shown against what was scored.
    await expect(page.locator('.exec-header p')).toHaveText('4 accounts scored · 2 shown');

    await button(page, 'Mid 25–49 (1)').click();
    expect(await names(page)).toEqual(['Millrace Tavern']);
    await button(page, 'Low 0–24 (1)').click();
    expect(await names(page)).toEqual(['Anonymous Sponsor']);

    await button(page, 'All (4)').click();
    expect(await names(page)).toEqual(TABLE.map((r) => r[0]));
    await expect(page.locator('.exec-header p')).toHaveText('4 accounts scored · 4 shown');
  });

  test('the bucket edges are 50 and 25: 50 is High, 49 and 25 are Mid, 24 is Low', async ({ page }) => {
    await install(page, {
      risk: (r) => json(r, { accounts: [
        score(ZED, 50, 'edge', null, 0, false, null),
        score(ANCHOR, 49, 'edge', null, 0, false, null),
        score(MILL, 25, 'edge', null, 0, false, null),
        score(SPONSOR, 24, 'edge', null, 0, false, null),
      ] }),
    });
    await mountScored(page);

    await expect(page.locator('aside.catalog-filters button.filter-btn').first()).toHaveText('All (4)');
    await button(page, 'High 50+ (1)').click();
    expect(await names(page)).toEqual(['Zed Taproom']);
    await button(page, 'Mid 25–49 (2)').click();
    expect(await names(page)).toEqual(['Anchor Pub', 'Millrace Tavern']);
    await button(page, 'Low 0–24 (1)').click();
    expect(await names(page)).toEqual(['Anonymous Sponsor']);
  });
});

// ── Tier buttons ────────────────────────────────────────────────────

test.describe('/watchlist — the Tier buttons', () => {
  test('each tier shows its accounts, composes with the bucket, and All restores', async ({ page }) => {
    await install(page);
    await mountScored(page);

    for (const [name, want] of [
      ['Platinum (1)', 'Zed Taproom'], ['Gold (1)', 'Anchor Pub'],
      ['Silver (1)', 'Millrace Tavern'], ['No tier (1)', 'Anonymous Sponsor'],
    ] as const) {
      await button(page, name).click();
      expect(await names(page)).toEqual([want]);
      await expect(page.locator('button.filter-btn-active')).toHaveText(['All (4)', name]);
    }

    // Silver's one account is Mid, so Silver + High shows nothing: the
    // empty line, and the subtitle says none of the four is shown.
    await button(page, 'Silver (1)').click();
    await button(page, 'High 50+ (2)').click();
    await expect(bodyRows(page)).toHaveCount(0);
    await expect(empty(page)).toHaveText('No accounts match those filters.');
    await expect(page.locator('.exec-header p')).toHaveText('4 accounts scored · 0 shown');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);

    // The bucket counts are over every scored row, not the tier's.
    await expect(button(page, 'High 50+ (2)')).toBeVisible();

    await button(page, 'All').click();
    expect(await names(page)).toEqual(['Zed Taproom', 'Anchor Pub']);
    await button(page, 'All (4)').click();
    expect(await names(page)).toEqual(TABLE.map((r) => r[0]));
  });
});

// ── Search ──────────────────────────────────────────────────────────

test.describe('/watchlist — the search', () => {
  test('narrows on account, top factor and city, case-insensitively, and sends nothing', async ({ page }) => {
    const seen = watch(page);
    await install(page);
    await mountScored(page);
    expect(await settledReads(page, () => seen.reads.length, 2)).toBe(2);

    const search = searchbox(page);
    await search.fill('zed');
    expect(await names(page)).toEqual(['Zed Taproom']);
    await search.fill('SERVICE TICKETS');
    expect(await names(page)).toEqual(['Anchor Pub']);
    // The city comes from the directory, not from the score row.
    await search.fill('bend');
    expect(await names(page)).toEqual(['Millrace Tavern']);
    await expect(page.locator('.exec-header p')).toHaveText('4 accounts scored · 1 shown');
    await search.fill('no such account');
    await expect(bodyRows(page)).toHaveCount(0);
    await expect(empty(page)).toHaveText('No accounts match those filters.');
    await search.fill('');
    await expect(bodyRows(page)).toHaveCount(4);

    // Client-side only: no read beyond the two at mount, no write.
    expect(seen.reads).toHaveLength(2);
    expect(seen.writes).toHaveLength(0);
  });

  test('composes with the buttons', async ({ page }) => {
    await install(page);
    await mountScored(page);

    await searchbox(page).fill('invoice');
    expect(await names(page)).toEqual(['Zed Taproom', 'Anonymous Sponsor']);
    await button(page, 'Low 0–24 (1)').click();
    expect(await names(page)).toEqual(['Anonymous Sponsor']);
  });
});

// ── Sort headers ────────────────────────────────────────────────────

test.describe('/watchlist — the sort headers', () => {
  test('each sortable header sorts by its column, and a second click reverses it', async ({ page }) => {
    await install(page);
    await mountScored(page);

    // Default: Score, highest first, and the header says so.
    await expect(header(page, 'Score')).toHaveText('Score ↓');
    await expect(header(page, 'Score')).toHaveAttribute('aria-sort', 'descending');

    await header(page, 'Score').click();
    expect(await names(page)).toEqual(['Anonymous Sponsor', 'Millrace Tavern', 'Anchor Pub', 'Zed Taproom']);
    await expect(header(page, 'Score')).toHaveText('Score ↑');
    await expect(header(page, 'Score')).toHaveAttribute('aria-sort', 'ascending');

    // Account opens A to Z, and the Score arrow goes.
    await header(page, 'Account').click();
    expect(await names(page)).toEqual(['Anchor Pub', 'Anonymous Sponsor', 'Millrace Tavern', 'Zed Taproom']);
    await expect(header(page, 'Account')).toHaveText('Account ↑');
    await expect(header(page, 'Score')).toHaveText('Score');
    await expect(header(page, 'Score')).not.toHaveAttribute('aria-sort', /./);
    await header(page, 'Account').click();
    expect(await names(page)).toEqual(['Zed Taproom', 'Millrace Tavern', 'Anonymous Sponsor', 'Anchor Pub']);

    // The factor columns open largest first. A null (no invoice, no
    // note on record) sorts below every day count, so it leads the
    // ascending order and ends the descending one.
    await header(page, 'Days since invoice').click();
    expect(await names(page)).toEqual(['Zed Taproom', 'Anchor Pub', 'Millrace Tavern', 'Anonymous Sponsor']);
    await expect(header(page, 'Days since invoice')).toHaveText('Days since invoice ↓');
    await header(page, 'Days since invoice').click();
    expect(await names(page)).toEqual(['Anonymous Sponsor', 'Millrace Tavern', 'Anchor Pub', 'Zed Taproom']);

    await header(page, 'Open SRs').click();
    expect(await names(page)).toEqual(['Anchor Pub', 'Zed Taproom', 'Millrace Tavern', 'Anonymous Sponsor']);
    await header(page, 'Open SRs').click();
    expect(await names(page)).toEqual(['Anonymous Sponsor', 'Millrace Tavern', 'Zed Taproom', 'Anchor Pub']);

    await header(page, 'Days since contact').click();
    expect(await names(page)).toEqual(['Millrace Tavern', 'Zed Taproom', 'Anchor Pub', 'Anonymous Sponsor']);
    await header(page, 'Days since contact').click();
    expect(await names(page)).toEqual(['Anonymous Sponsor', 'Anchor Pub', 'Zed Taproom', 'Millrace Tavern']);
  });

  test('Top factor and Contract do not sort', async ({ page }) => {
    await install(page);
    await mountScored(page);

    for (const name of ['Top factor', 'Contract']) {
      await header(page, name).click();
      expect(await table(page)).toEqual(TABLE);
      await expect(header(page, name)).not.toHaveAttribute('tabindex', /./);
      await expect(header(page, name)).not.toHaveAttribute('aria-sort', /./);
    }
    await expect(header(page, 'Score')).toHaveText('Score ↓');
  });

  // Gap 13 (8c5664ea), answered in this car: the headers were `<th
  // onclick>`, which a keyboard cannot reach.
  test('a keyboard reaches every sortable header and sorts with Enter or Space', async ({ page }) => {
    await install(page);
    await mountScored(page);

    for (const name of ['Account', 'Score', 'Days since invoice', 'Open SRs', 'Days since contact']) {
      await expect(header(page, name)).toHaveAttribute('tabindex', '0');
    }

    await header(page, 'Account').focus();
    await expect(header(page, 'Account')).toBeFocused();
    await page.keyboard.press('Enter');
    expect(await names(page)).toEqual(['Anchor Pub', 'Anonymous Sponsor', 'Millrace Tavern', 'Zed Taproom']);
    await expect(header(page, 'Account')).toHaveAttribute('aria-sort', 'ascending');
    await page.keyboard.press(' ');
    expect(await names(page)).toEqual(['Zed Taproom', 'Millrace Tavern', 'Anonymous Sponsor', 'Anchor Pub']);
    await expect(header(page, 'Account')).toHaveAttribute('aria-sort', 'descending');

    // Tab moves on to the next sortable header, skipping nothing it
    // could sort by and landing on nothing it could not.
    await page.keyboard.press('Tab');
    await expect(header(page, 'Score')).toBeFocused();
    await page.keyboard.press('Tab');
    await expect(header(page, 'Days since invoice')).toBeFocused();
  });
});

// ── The account link and back ───────────────────────────────────────

async function installDetail(page: Page, row: ReturnType<typeof account>): Promise<void> {
  const q = encodeURIComponent(row.id);
  const paged = (limit: number) => ({ data: [], total: 0, limit, offset: 0 });
  await page.route(new RegExp(`/api/people/accounts/${q}$`), (r) => json(r, row));
  for (const re of [
    new RegExp(`/api/assets\\?account_id=${q}&`),
    new RegExp(`/api/commerce/invoices\\?account_id=${q}&`),
    new RegExp(`/api/jobs\\?subject_id=${q}&`),
    new RegExp(`/api/shipping/shipments\\?account_id=${q}&`),
  ]) {
    await page.route(re, (r) => json(r, paged(500)));
  }
}

test.describe('/watchlist — the account link, and back', () => {
  test('every row links its account under the catalogued accounts path; one Back returns', async ({ page }) => {
    await install(page);
    await installDetail(page, ZED);
    await mountScored(page);

    const links = page.locator('table.risk-table tbody a');
    await expect(links).toHaveCount(SCORES.length);
    for (const [i, row] of TABLE.entries()) {
      const id = DIRECTORY_ROWS.find((a) => a.name === row[0])!.id;
      await expect(links.nth(i)).toHaveText(row[0]!);
      await expect(links.nth(i)).toHaveAttribute('href', `${ACCOUNTS_PATH}/${encodeURIComponent(id)}`);
    }
    // The one link kind on the page, and where the router sends it: the
    // account detail under the catalogued Accounts route.
    await expect(page.locator('section.list-section a')).toHaveCount(SCORES.length);
    expect(parseRoute(`${ACCOUNTS_PATH}/acct-zed`)).toMatchObject({ kind: 'account' });

    const depth = await page.evaluate(() => window.history.length);
    await links.first().click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(`${ACCOUNTS_PATH}/acct-zed`);
    await expect(page.locator('h1.exec-title')).toHaveText('Zed Taproom');
    expect(await page.evaluate(() => window.history.length)).toBe(depth + 1);

    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(PATH);
    await expect(page.locator('h1.exec-title')).toHaveText('Churn watchlist');
    expect(await table(page)).toEqual(TABLE);
  });
});

// ── Empty and failed reads: never the same paint ─────────────────────

test.describe('/watchlist — an empty backend and a failed read', () => {
  test('nothing scored paints the empty line, with no failure marker', async ({ page }) => {
    // The live instance on 2026-09-23 15:38Z (the audit's measure step):
    // `{accounts: [], total_scored: 0}` over a directory of one sponsor.
    await install(page, {
      risk: (r) => json(r, { accounts: [], total_scored: 0 }),
      directory: (r) => json(r, { data: [SPONSOR], total: 1, limit: 1000, offset: 0 }),
    });
    await mountPage(page, PATH, { titleMatch: /^Churn watchlist$/ });

    await expectHeader(page, 'Customers', 'Churn watchlist', '0 accounts scored · 0 shown');
    // Gap 6 (9289e682): no filter is set, and the line blames filters.
    await expect(page.locator('button.filter-btn-active')).toHaveText(['All (0)', 'All']);
    await expect(searchbox(page)).toHaveValue('');
    await expect(empty(page)).toHaveText('No accounts match those filters.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.locator('table.risk-table')).toHaveCount(0);
  });

  test('a failed scores read paints the failure marker, never the empty line', async ({ page }) => {
    await install(page, { risk: (r) => json(r, { error: 'boom' }, 500) });
    await mountPage(page, PATH);

    await expectHeader(page, 'Churn watchlist', "Couldn't load watchlist");
    await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText('Error: 500');
    await expect(page.getByText('No accounts match those filters.')).toHaveCount(0);
    await expect(page.getByText(/accounts scored/)).toHaveCount(0);
    await expect(page.locator('aside.catalog-filters')).toHaveCount(0);
  });

  test('a scores payload of the wrong shape is a failure, not an empty list', async ({ page }) => {
    await install(page, { risk: (r) => json(r, []) });
    await mountPage(page, PATH);

    await expectHeader(page, 'Churn watchlist', "Couldn't load watchlist");
    await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      'Error: unexpected risk-score payload',
    );
    await expect(page.getByText('No accounts match those filters.')).toHaveCount(0);
  });

  // Gap 10 (4b981df2), answered: a row without its factors used to parse
  // and then throw in render, past the error state.
  test('a score row without its factors is the same failure, not a crash', async ({ page }) => {
    const { factors: _dropped, ...bare } = SCORES[0]!;
    await install(page, { risk: (r) => json(r, { accounts: [bare, ...SCORES.slice(1)] }) });
    await mountPage(page, PATH);

    await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      'Error: unexpected risk-score payload',
    );
    await expect(page.locator('table.risk-table')).toHaveCount(0);
  });

  test('an unreachable scores read is a failure too', async ({ page }) => {
    await install(page, { risk: (r) => r.abort('connectionrefused') });
    await mountPage(page, PATH);

    await expectHeader(page, 'Churn watchlist', "Couldn't load watchlist");
    await expect(page.locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(/^TypeError: /);
  });
});

// ── The cap ─────────────────────────────────────────────────────────

test.describe('/watchlist — the 200-row cap', () => {
  // Gap 8 (9269d612): the read asks for at most 200 scores and the
  // server clamps there; the page counts the rows it got as the number
  // scored, and says nothing about a cap.
  test('200 rows back reads as "200 accounts scored", with no cap notice', async ({ page }) => {
    const seen = watch(page);
    const many = Array.from({ length: 200 }, (_, i) =>
      score({ id: `acct-${i}`, name: `Account ${String(i).padStart(3, '0')}` }, i % 100, 'f', null, 0, false, null));
    await install(page, { risk: (r) => json(r, { accounts: many, total_scored: 200 }) });
    await mountPage(page, PATH, { titleMatch: /^Churn watchlist$/ });

    await expect(page.locator('.exec-header p')).toHaveText('200 accounts scored · 200 shown');
    await expect(bodyRows(page)).toHaveCount(200);
    await expect(page.locator('.overflow-banner')).toHaveCount(0);
    expect(await settledReads(page, () => seen.reads.length, 2)).toBe(2);
    expect(seen.reads).toContain(RISK_URL);
  });
});
