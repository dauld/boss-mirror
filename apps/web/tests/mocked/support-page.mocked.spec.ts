// /ux/support — the page's controls and its module gate, pinned
// (backlog 5b2f3240, gap 10 of the /ux/support page audit 9876ef0d).
//
// Before this spec the route was reached only by the two crawls:
// route-smoke renders it and outage-crawl asserts a failure marker.
// Nothing clicked a tab or sorted a column, and every mocked spec ran
// under MODULES_ON — a manifest with every module listed true, the one
// shape under which a module gate cannot be seen. That is how
// /ux/support stayed gated on 'shipping' and announced itself as
// "Shipments is not enabled" (gap 3, fixed by f9b43965) with no spec
// able to notice. The gate legs below render against a manifest listing
// no modules, against a manifest that lists ONLY support — so a gate
// keyed on any other module id fails here by name — and against
// MODULES_LIVE, the recorded live manifest, which lists support true.
// This file said the live gateway served `"modules":{}`; it did on
// 2026-09-19 and stopped, and nothing here could notice (41454ce1).
//
// EVERY CONTROL, as the page behaves TODAY (the audit's `test` step,
// 2026-09-25). The gap-10 legs above pinned the gate, the tabs, two
// sorts and the three failure lines; the legs from "the words" down
// pin the rest of the procedure: each link against nav-catalog.ts,
// following one and Back, the empty paint against the failed paint,
// no write and no re-fetch, the capped banners, the escalation
// boundary, and the words themselves. The page, counted
// (SupportPage.svelte):
//   reads    — 3, each issued once on mount and never again: the
//              department's cases, the account directory, /api/assets;
//   writes   — 0 (the only non-GET is the shell's surface-open record);
//   links    — 4 sites: Active Cases' Job, Account and Subject cells,
//              Account Health's Account cell; plus each row, which
//              navigates to its job or its account;
//   controls — 3 tabs and 11 sortable headers (6 + 5), each a
//              `th tabindex=0` that sorts on click, Enter or Space;
//   banners  — up to 3 OverflowBanners and one devices alert.
//
// Lines that pin a FILED gap's current behaviour name it: f5c1e556
// (Support has no protocol layer, gap 2, the one gap of eleven still
// open). Lines marked UNFILED pin behaviour this spec found and the
// audit did not list; the car's report names them, so the car that
// answers one edits the expectation here instead of passing silently.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage, openedRequests, readsSettled, recordPageRequests } from './_helpers';
import {
  installSmokeMocks, installTenantManifest, LIVE_MANIFEST_RECORDED_AT, MODULES_LIVE, MODULES_NONE,
} from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { parseRoute } from '../../src/router';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { sectionForRoute } from '../../src/shell/sections';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// The page's three reads, as SupportPage.svelte spells them. Accounts is
/// ACCOUNTS_LIST_URL (src/accounts/api.ts), a paged read since 807d47bb.
const CASES = /\/api\/jobs\?department=support&limit=5000$/;
const ACCOUNTS = /\/api\/people\/accounts\?limit=1000$/;
const ASSETS = /\/api\/assets\?limit=1000$/;

const job = (id: string, title: string, account: string, opened_on: string, status = 'open') => ({
  id, kind: 'support-case', title, owner_id: 'emp-001', status, priority: 'standard',
  subject: { subject_kind: 'account', id: account },
  opened_on, due_on: null, closed_on: null, metadata: {}, tags: [],
});

/// Three open cases whose title order, age order and id order all
/// differ, so each sort is distinguishable from the others; one closed
/// case, which no count may include.
const JOBS = [
  job('job-aaaa0001', 'Beta chiller fault', 'acct-zed', '2026-03-01'),
  job('job-bbbb0002', 'Alpha keg leak', 'acct-anchor', '2026-01-01'),
  job('job-cccc0003', 'Gamma line clean', 'acct-zed', '2026-02-01'),
  job('job-dddd0004', 'Delta closed long ago', 'acct-anchor', '2025-12-01', 'closed'),
];

/// Open cases per account: Zed 2, Anchor 1, Quiet 0 — so the default
/// open-first order (Zed, Anchor, Quiet) differs from name order
/// (Anchor, Quiet, Zed).
const ACCOUNT_ROWS = [
  { id: 'acct-zed', name: 'Zed Taproom', tier: 'gold' },
  { id: 'acct-anchor', name: 'Anchor Pub', tier: 'silver' },
  { id: 'acct-quiet', name: 'Quiet Bar', tier: 'platinum' },
];

type Reads = Readonly<{
  jobs?: ReadonlyArray<unknown>;
  accounts?: ReadonlyArray<unknown>;
  assets?: ReadonlyArray<unknown>;
  /// A server total past the rows sent: the page's window is capped.
  totals?: Readonly<{ jobs?: number; accounts?: number; assets?: number }>;
}>;

async function installSupportReads(page: Page, reads: Reads = {}): Promise<void> {
  const jobs = reads.jobs ?? JOBS;
  const accounts = reads.accounts ?? ACCOUNT_ROWS;
  const assets = reads.assets ?? [];
  const totals = reads.totals ?? {};
  await installSmokeMocks(page);
  await page.route(CASES, (r) =>
    json(r, { data: jobs, total: totals.jobs ?? jobs.length, limit: 5000, offset: 0 }));
  await page.route(ACCOUNTS, (r) =>
    json(r, { data: accounts, total: totals.accounts ?? accounts.length, limit: 1000, offset: 0 }),
  );
  await page.route(ASSETS, (r) =>
    json(r, { data: assets, total: totals.assets ?? assets.length, limit: 1000, offset: 0 }));
}

/// The text of one column, top to bottom, in the visible table.
async function column(page: Page, index: number): Promise<string[]> {
  return page.locator('table.data-table tbody tr').evaluateAll(
    (rows, i) => rows.map((row) => (row.children[i] as HTMLElement).innerText.trim()),
    index,
  );
}

test.describe('/ux/support module gate — against the live-shaped manifest', () => {
  test('a manifest listing no modules renders the notice, naming Support', async ({ page }) => {
    await installSupportReads(page);
    await installTenantManifest(page, MODULES_NONE);
    await mountPage(page, '/ux/support');

    const notice = page.locator('.module-disabled');
    await expect(notice.locator('h1')).toHaveText('Not enabled for this tenant');
    await expect(notice.locator('strong')).toHaveText('Support');
    // The label the drifted gate produced (gap 3).
    await expect(page.getByText('Shipments')).toHaveCount(0);
    await expect(page.getByText('open cases')).toHaveCount(0);
  });

  test('a manifest listing ONLY support renders the page', async ({ page }) => {
    await installSupportReads(page);
    await installTenantManifest(page, { support: true });
    await mountPage(page, '/ux/support', { titleMatch: /3 open cases/ });

    await expect(page.locator('.module-disabled')).toHaveCount(0);
  });

  test(`the live manifest recorded ${LIVE_MANIFEST_RECORDED_AT} (support = true) renders the page`, async ({ page }) => {
    expect(MODULES_LIVE['support'], 'the recording lists support').toBe(true);
    await installSupportReads(page);
    await installTenantManifest(page, MODULES_LIVE);
    await mountPage(page, '/ux/support', { titleMatch: /3 open cases/ });

    await expect(page.locator('.module-disabled')).toHaveCount(0);
  });
});

test.describe('/ux/support controls', () => {
  test('the tabs switch the view, and the overview counts only open cases', async ({ page }) => {
    await installSupportReads(page);
    await mountPage(page, '/ux/support', { titleMatch: /3 open cases/ });

    const tab = (name: string) => page.getByRole('tab', { name });
    await expect(tab('Overview')).toHaveAttribute('aria-selected', 'true');
    const kv = page.locator('dl.kv');
    await expect(kv.first()).toContainText('Total open service jobs');
    await expect(kv.first().locator('dd')).toHaveText('3');
    await expect(kv.nth(1).locator('dd').first()).toHaveText('2');

    await tab('Active Cases').click();
    await expect(tab('Active Cases')).toHaveAttribute('aria-selected', 'true');
    await expect(tab('Overview')).toHaveAttribute('aria-selected', 'false');
    await expect(page.locator('dl.kv')).toHaveCount(0);
    await expect(page.locator('table.data-table tbody tr')).toHaveCount(3);

    await tab('Account Health').click();
    await expect(tab('Account Health')).toHaveAttribute('aria-selected', 'true');
    await expect(page.locator('table.data-table tbody tr')).toHaveCount(3);
    await expect(page.getByRole('columnheader', { name: /Tier/ })).toBeVisible();

    await tab('Overview').click();
    await expect(page.locator('dl.kv').first()).toBeVisible();
  });

  test('Active Cases lands oldest first, and a column header re-sorts it both ways', async ({ page }) => {
    await installSupportReads(page);
    await mountPage(page, '/ux/support', { titleMatch: /3 open cases/ });
    await page.getByRole('tab', { name: 'Active Cases' }).click();

    const TITLE = 5;
    await expect.poll(() => column(page, TITLE)).toEqual(['Alpha keg leak', 'Gamma line clean', 'Beta chiller fault']);
    await expect(page.getByRole('columnheader', { name: /Days open/ })).toHaveAttribute('aria-sort', 'descending');

    const title = page.getByRole('columnheader', { name: /Title/ });
    await title.click();
    await expect(title).toHaveAttribute('aria-sort', 'ascending');
    await expect.poll(() => column(page, TITLE)).toEqual(['Alpha keg leak', 'Beta chiller fault', 'Gamma line clean']);

    await title.click();
    await expect(title).toHaveAttribute('aria-sort', 'descending');
    await expect.poll(() => column(page, TITLE)).toEqual(['Gamma line clean', 'Beta chiller fault', 'Alpha keg leak']);
  });

  test('Account Health lands busiest first, and the Account header sorts by name', async ({ page }) => {
    await installSupportReads(page);
    await mountPage(page, '/ux/support', { titleMatch: /3 open cases/ });
    await page.getByRole('tab', { name: 'Account Health' }).click();

    await expect.poll(() => column(page, 0)).toEqual(['Zed Taproom', 'Anchor Pub', 'Quiet Bar']);
    await expect.poll(() => column(page, 2)).toEqual(['2', '1', '0']);
    await expect(page.getByRole('columnheader', { name: /Open jobs/ })).toHaveAttribute('aria-sort', 'descending');

    const account = page.getByRole('columnheader', { name: /Account/ });
    await account.click();
    await expect(account).toHaveAttribute('aria-sort', 'ascending');
    await expect.poll(() => column(page, 0)).toEqual(['Anchor Pub', 'Quiet Bar', 'Zed Taproom']);
  });

  test('a failed cases read is said on every tab, never drawn as an empty list', async ({ page }) => {
    await installSupportReads(page);
    await page.route(CASES, (r) => json(r, { error: 'jobs down' }, 503));
    await page.goto('/ux/support');

    const failure = "Couldn't load cases — /api/jobs?department=support&limit=5000: HTTP 503";
    for (const name of ['Overview', 'Active Cases', 'Account Health']) {
      await page.getByRole('tab', { name }).click();
      await expect(page.locator('.catalog [role=alert]')).toHaveText(failure);
    }
    await expect(page.getByText('No open cases.')).toHaveCount(0);
    await expect(page.getByText('No account data.')).toHaveCount(0);
  });

  // The two SECONDARY reads (gaps 4 and 5). Each used to collapse its
  // failure into the page's normal shape — an empty account list, a
  // column of dashes — and reads.ts now names what each failure means.
  test('a failed accounts read empties Account Health with a reason, not "No account data."', async ({ page }) => {
    await installSupportReads(page);
    await page.route(ACCOUNTS, (r) => json(r, { error: 'people down' }, 503));
    await mountPage(page, '/ux/support', { titleMatch: /3 open cases/ });
    await page.getByRole('tab', { name: 'Account Health' }).click();

    await expect(page.locator('.catalog [role=alert]')).toContainText(
      "Couldn't load accounts — /api/people/accounts?limit=1000: HTTP 503",
    );
    await expect(page.getByText('No account data.')).toHaveCount(0);
    await expect(page.locator('table.data-table')).toHaveCount(0);
  });

  test('a failed devices read says what a dash in a device cell means', async ({ page }) => {
    await installSupportReads(page);
    await page.route(ASSETS, (r) => json(r, { error: 'assets down' }, 503));
    await mountPage(page, '/ux/support', { titleMatch: /3 open cases/ });

    const alert = page.locator('.catalog [role=alert]');
    await expect(alert).toContainText("Couldn't load devices — /api/assets?limit=1000: HTTP 503");
    await expect(alert).toContainText('this page could not read the device, not that the case has none');
    // The cases still render: devices only enrich the rows.
    await page.getByRole('tab', { name: 'Active Cases' }).click();
    await expect(page.locator('table.data-table tbody tr')).toHaveCount(3);
  });
});

// ── Every control, as the page behaves today (the audit's `test` step) ──

const PATH = ROUTE_CATALOG.support.path;

const tab = (page: Page, name: string) => page.locator('.catalog [role=tab]', { hasText: name });
const row = (page: Page, text: string) => page.locator('.catalog tbody tr', { hasText: text });
const pathname = (page: Page): string => new URL(page.url()).pathname;
const historyDepth = (page: Page): Promise<number> => page.evaluate(() => window.history.length);

/// The section a path lights, when that section is a ROUTE_CATALOG key.
/// Read from nav-catalog.ts and sections.ts, never from a second list.
function catalogued(path: string): string | null {
  const section = sectionForRoute(parseRoute(path));
  return section in ROUTE_CATALOG ? section : null;
}

/// The visible table's header labels, without the sort arrow.
async function headerLabels(page: Page): Promise<string[]> {
  return page.locator('.catalog thead th').evaluateAll((ths) =>
    ths.map((th) => (th.textContent ?? '').replace(/[↑↓]/g, '').trim()));
}

const onAsset = (id: string, title: string, asset: string, opened_on: string) => ({
  ...job(id, title, 'unused', opened_on),
  subject: { subject_kind: 'asset', id: asset },
});

/// Four open cases drawing every cell a row can: an account the
/// directory knows, a device the assets read knows, and one of each
/// that neither knows — so every link and every dash is on screen at
/// once. Oldest first: Orphan, Unknown device, Glycol, Tap handle.
const LINK_JOBS = [
  job('job-eeee0005', 'Tap handle loose', 'acct-zed', '2026-03-01', 'waiting-on-customer'),
  onAsset('job-ffff0006', 'Glycol pump noise', 'asset-glycol-01', '2026-02-01'),
  job('job-gggg0007', 'Orphan case', 'acct-missing', '2026-01-15'),
  onAsset('job-hhhh0008', 'Unknown device', 'asset-gone', '2026-01-20'),
];
const LINK_ASSETS = [{ asset_id: 'asset-glycol-01', account_id: 'acct-zed' }];

test.describe('/ux/support — the words', () => {
  test('the header, the tabs and the Overview say what they say today', async ({ page }) => {
    await installSupportReads(page);
    await mountPage(page, PATH, { titleMatch: /3 open cases/ });

    await expect(page.locator('.catalog .exec-eyebrow')).toHaveText('Customer Support');
    await expect(page.locator('.catalog h1.exec-title')).toHaveText('3 open cases');
    // UNFILED: the subtitle names field-service, the kind the page
    // stopped reading at 423a531d. The read is department=support, and
    // that department has no protocol yet (f5c1e556).
    await expect(page.locator('.catalog header.exec-header p')).toHaveText('Field-service Jobs');
    await expect(page.locator('.catalog [role=tab]')).toHaveText(['Overview', 'Active Cases', 'Account Health']);
    await expect(page.locator('.catalog h3')).toHaveText(['Case volume', 'Coverage']);
    await expect(page.locator('.catalog dl.kv dt')).toHaveText([
      'Total open service jobs', 'Accounts with open jobs', 'Escalated (>14d open)',
    ]);
  });

  test('the eleven column headers are focusable headers, not buttons, and sort from the keyboard', async ({ page }) => {
    await installSupportReads(page);
    await mountPage(page, PATH, { titleMatch: /3 open cases/ });

    await tab(page, 'Active Cases').click();
    await expect.poll(() => headerLabels(page)).toEqual(['Job', 'Account', 'Subject', 'Days open', 'Status', 'Title']);
    await expect(page.locator('.catalog thead th[tabindex="0"]')).toHaveCount(6);
    // The audit's measure: each announces as a column header, and none
    // carries a button role.
    await expect(page.locator('.catalog thead button, .catalog thead [role=button]')).toHaveCount(0);

    const title = page.getByRole('columnheader', { name: /Title/ });
    await title.focus();
    await page.keyboard.press('Enter');
    await expect(title).toHaveAttribute('aria-sort', 'ascending');
    await expect.poll(() => column(page, 5)).toEqual(['Alpha keg leak', 'Beta chiller fault', 'Gamma line clean']);
    await page.keyboard.press('Space');
    await expect(title).toHaveAttribute('aria-sort', 'descending');
    await expect.poll(() => column(page, 5)).toEqual(['Gamma line clean', 'Beta chiller fault', 'Alpha keg leak']);

    await tab(page, 'Account Health').click();
    await expect.poll(() => headerLabels(page)).toEqual(['Account', 'Tier', 'Open jobs', 'Equipment', 'Last job']);
    await expect(page.locator('.catalog thead th[tabindex="0"]')).toHaveCount(5);
  });
});

test.describe('/ux/support — every link lands on a catalogued route', () => {
  test('Active Cases links each case, its account and its device; a case with neither draws dashes', async ({ page }) => {
    await installSupportReads(page, { jobs: LINK_JOBS, assets: LINK_ASSETS });
    await mountPage(page, PATH, { titleMatch: /4 open cases/ });
    // The Overview draws numbers, never a link.
    await expect(page.locator('.catalog a')).toHaveCount(0);

    await tab(page, 'Active Cases').click();
    await expect(page.locator('.catalog tbody tr')).toHaveCount(4);
    await expect.poll(() => column(page, 5)).toEqual([
      'Orphan case', 'Unknown device', 'Glycol pump noise', 'Tap handle loose',
    ]);

    const jobs = ROUTE_CATALOG.jobs.path;
    const accounts = ROUTE_CATALOG.accounts.path;
    const assets = ROUTE_CATALOG.assets.path;
    const rows = [
      { title: 'Orphan case', job: 'job-gggg0007', account: null, asset: null, status: 'open' },
      { title: 'Unknown device', job: 'job-hhhh0008', account: null, asset: null, status: 'open' },
      { title: 'Glycol pump noise', job: 'job-ffff0006', account: null, asset: 'asset-glycol-01', status: 'open' },
      { title: 'Tap handle loose', job: 'job-eeee0005', account: ['acct-zed', 'Zed Taproom'], asset: null, status: 'waiting on customer' },
    ] as const;
    for (const e of rows) {
      const td = row(page, e.title).locator('td');
      await expect(td.nth(0).locator('a')).toHaveText(e.job);
      await expect(td.nth(0).locator('a')).toHaveAttribute('href', `${jobs}/${e.job}`);
      if (e.account) {
        await expect(td.nth(1).locator('a')).toHaveText(e.account[1]);
        await expect(td.nth(1).locator('a')).toHaveAttribute('href', `${accounts}/${e.account[0]}`);
      } else {
        await expect(td.nth(1)).toHaveText('—');
        await expect(td.nth(1).locator('a')).toHaveCount(0);
      }
      if (e.asset) {
        await expect(td.nth(2).locator('a')).toHaveText(e.asset);
        await expect(td.nth(2).locator('a')).toHaveAttribute('href', `${assets}/${e.asset}`);
      } else {
        await expect(td.nth(2)).toHaveText('—');
        await expect(td.nth(2).locator('a')).toHaveCount(0);
      }
      await expect(td.nth(4)).toHaveText(e.status);
    }

    // Six links in all — four cases, one account, one device — and
    // every one lights a catalogued row of nav-catalog.ts.
    const hrefs = await page.locator('.catalog a').evaluateAll((as) => as.map((a) => a.getAttribute('href') ?? ''));
    expect(hrefs).toHaveLength(6);
    for (const h of hrefs) expect(catalogued(h), `${h} lands on a catalogued route`).not.toBeNull();
    expect([...new Set(hrefs.map(catalogued))].sort()).toEqual(['accounts', 'assets', 'jobs']);
  });

  test('Account Health links every account, including one with no case', async ({ page }) => {
    await installSupportReads(page, { jobs: LINK_JOBS, assets: LINK_ASSETS });
    await mountPage(page, PATH, { titleMatch: /4 open cases/ });
    await tab(page, 'Account Health').click();

    await expect.poll(() => column(page, 0)).toEqual(['Zed Taproom', 'Anchor Pub', 'Quiet Bar']);
    await expect.poll(() => column(page, 2)).toEqual(['1', '0', '0']);
    await expect.poll(() => column(page, 3)).toEqual(['1', '0', '0']);
    await expect.poll(() => column(page, 4)).toEqual(['2026-03-01', '—', '—']);

    const links = page.locator('.catalog tbody a');
    await expect(links).toHaveCount(ACCOUNT_ROWS.length);
    for (const [i, id] of ['acct-zed', 'acct-anchor', 'acct-quiet'].entries()) {
      await expect(links.nth(i)).toHaveAttribute('href', `${ROUTE_CATALOG.accounts.path}/${id}`);
      expect(catalogued(`${ROUTE_CATALOG.accounts.path}/${id}`)).toBe('accounts');
    }
    await expect(page.locator('.catalog a')).toHaveCount(ACCOUNT_ROWS.length);
  });
});

test.describe('/ux/support — following a link, and Back', () => {
  test('the case link lands on its job once, and one Back returns here — on Overview', async ({ page }) => {
    await installSupportReads(page, { jobs: LINK_JOBS, assets: LINK_ASSETS });
    await mountPage(page, PATH, { titleMatch: /4 open cases/ });
    await tab(page, 'Active Cases').click();

    const depth = await historyDepth(page);
    await row(page, 'Tap handle loose').getByRole('link', { name: 'job-eeee0005' }).click();
    await expect.poll(() => pathname(page)).toBe(`${ROUTE_CATALOG.jobs.path}/job-eeee0005`);
    await expect.poll(() => historyDepth(page)).toBe(depth + 1);

    await page.goBack();
    await expect.poll(() => pathname(page)).toBe(PATH);
    await expect(page.locator('.catalog h1.exec-title')).toHaveText('4 open cases');
    // UNFILED: the tab is component state, not in the URL, so Back
    // lands on Overview rather than on the Active Cases the reader left.
    await expect(tab(page, 'Overview')).toHaveAttribute('aria-selected', 'true');
  });

  test('a click elsewhere on a case row lands on its job once, and one Back returns', async ({ page }) => {
    await installSupportReads(page, { jobs: LINK_JOBS, assets: LINK_ASSETS });
    await mountPage(page, PATH, { titleMatch: /4 open cases/ });
    await tab(page, 'Active Cases').click();

    const depth = await historyDepth(page);
    await row(page, 'Orphan case').locator('td').nth(3).click();
    await expect.poll(() => pathname(page)).toBe(`${ROUTE_CATALOG.jobs.path}/job-gggg0007`);
    await expect.poll(() => historyDepth(page)).toBe(depth + 1);

    await page.goBack();
    await expect.poll(() => pathname(page)).toBe(PATH);
    await expect(page.locator('.catalog h1.exec-title')).toHaveText('4 open cases');
  });

  // One navigation per test: a push after a Back replaces the forward
  // entry rather than adding one, so a second leg could not count.
  test('an Account Health link lands on the account once, and one Back returns', async ({ page }) => {
    await installSupportReads(page, { jobs: LINK_JOBS, assets: LINK_ASSETS });
    await mountPage(page, PATH, { titleMatch: /4 open cases/ });
    await tab(page, 'Account Health').click();

    const depth = await historyDepth(page);
    await page.locator('.catalog tbody').getByRole('link', { name: 'Zed Taproom', exact: true }).click();
    await expect.poll(() => pathname(page)).toBe(`${ROUTE_CATALOG.accounts.path}/acct-zed`);
    await expect.poll(() => historyDepth(page)).toBe(depth + 1);

    await page.goBack();
    await expect.poll(() => pathname(page)).toBe(PATH);
    await expect(page.locator('.catalog h1.exec-title')).toHaveText('4 open cases');
  });

  test('a click elsewhere on an Account Health row lands on the account once, and one Back returns', async ({ page }) => {
    await installSupportReads(page, { jobs: LINK_JOBS, assets: LINK_ASSETS });
    await mountPage(page, PATH, { titleMatch: /4 open cases/ });
    await tab(page, 'Account Health').click();

    const depth = await historyDepth(page);
    await row(page, 'Anchor Pub').locator('td').nth(2).click();
    await expect.poll(() => pathname(page)).toBe(`${ROUTE_CATALOG.accounts.path}/acct-anchor`);
    await expect.poll(() => historyDepth(page)).toBe(depth + 1);

    await page.goBack();
    await expect.poll(() => pathname(page)).toBe(PATH);
    await expect(page.locator('.catalog h1.exec-title')).toHaveText('4 open cases');
  });

  // UNFILED: the page reads /api/assets and links each device whatever
  // the manifest says, while the device route is gated on `equipment`,
  // which the live manifest lists false. So on the live instance a
  // Subject link opens a "not enabled" notice.
  test('under the live manifest a device link lands on the Assets "not enabled" notice', async ({ page }) => {
    expect(MODULES_LIVE['equipment'], 'the recording lists equipment off').toBe(false);
    await installSupportReads(page, { jobs: LINK_JOBS, assets: LINK_ASSETS });
    await installTenantManifest(page, MODULES_LIVE);
    await mountPage(page, PATH, { titleMatch: /4 open cases/ });
    await tab(page, 'Active Cases').click();

    await row(page, 'Glycol pump noise').getByRole('link', { name: 'asset-glycol-01' }).click();
    await expect.poll(() => pathname(page)).toBe(`${ROUTE_CATALOG.assets.path}/asset-glycol-01`);
    await expect(page.locator('.module-disabled strong')).toHaveText(ROUTE_CATALOG.assets.label);

    await page.goBack();
    await expect.poll(() => pathname(page)).toBe(PATH);
    await expect(page.locator('.catalog h1.exec-title')).toHaveText('4 open cases');
  });
});

test.describe('/ux/support — an empty backend is not a failed one', () => {
  test('empty reads paint "nothing here" on every tab, with no failure marker and no link', async ({ page }) => {
    await installSupportReads(page, { jobs: [], accounts: [], assets: [] });
    await mountPage(page, PATH, { titleMatch: /0 open cases/ });

    await expect(page.locator('.catalog dl.kv dd')).toHaveText(['0', '0', '0']);
    const said: Record<string, string | null> = {
      Overview: null, 'Active Cases': 'No open cases.', 'Account Health': 'No account data.',
    };
    for (const [name, empty] of Object.entries(said)) {
      await tab(page, name).click();
      if (empty) await expect(page.locator('.catalog p.empty')).toHaveText(empty);
      await expect(page.locator(`.catalog ${FAILURE_MARKER}`)).toHaveCount(0);
      await expect(page.locator('.catalog a')).toHaveCount(0);
      await expect(page.locator('.catalog .overflow-banner')).toHaveCount(0);
    }
  });

  test('failed reads paint the failure marker on every tab, and never an empty sentence', async ({ page }) => {
    await installSupportReads(page);
    for (const read of [CASES, ACCOUNTS, ASSETS]) {
      await page.route(read, (r) => json(r, { error: 'down' }, 503));
    }
    await page.goto(PATH);
    await expect(page.locator('.catalog [role=tab]')).toHaveCount(3);

    const cases = "Couldn't load cases — /api/jobs?department=support&limit=5000: HTTP 503";
    for (const name of ['Overview', 'Active Cases', 'Account Health']) {
      await tab(page, name).click();
      await expect(page.locator(`.catalog ${FAILURE_MARKER}`, { hasText: cases })).toHaveCount(1);
      await expect(page.locator(`.catalog ${FAILURE_MARKER}`, { hasText: "Couldn't load devices" })).toHaveCount(1);
      await expect(page.locator('.catalog p.empty:not(.load-failed)')).toHaveCount(0);
      await expect(page.locator('.catalog dl.kv')).toHaveCount(0);
    }
    // UNFILED: the title is computed from the rows it could not read, so
    // over the failure line the page's h1 still says a count of zero.
    await expect(page.locator('.catalog h1.exec-title')).toHaveText('0 open cases');
  });
});

test.describe('/ux/support — a capped window says so', () => {
  test('each capped read names its window, and the title says window-only', async ({ page }) => {
    await installSupportReads(page, {
      jobs: LINK_JOBS, assets: LINK_ASSETS, totals: { jobs: 7000, accounts: 1500, assets: 2000 },
    });
    await mountPage(page, PATH, { titleMatch: /open cases/ });

    await expect(page.locator('.catalog h1.exec-title')).toHaveText('4+ open cases (window-only)');
    // `.text`: the banner's icon glyph is aria-hidden, and the words are what we pin.
    const banners = page.locator('.catalog .overflow-banner .text');
    await expect(banners).toHaveText([
      'Showing 4 of 7,000 service jobs loaded. Open-case + account-health counts on this page only consider this window; raise the cap or narrow by date.',
      'Showing 3 of 1,500 accounts loaded. Account Health lists only these; a case whose account is past the cap shows its id.',
      "Showing 1 of 2,000 devices loaded for case-to-device joins. Some active cases may show their device as '—' when its row is past the cap.",
    ]);

    // UNFILED: the accounts banner says a case whose account is past the
    // cap "shows its id"; the Account cell draws a dash instead.
    await tab(page, 'Active Cases').click();
    await expect(row(page, 'Orphan case').locator('td').nth(1)).toHaveText('—');
  });
});

// The threshold is ESCALATION_DAYS in src/support/reads.ts (e0a40c81),
// strictly greater than: pinned here through the page, at its edge,
// against a fixed clock rather than the day the suite happens to run.
test.describe('/ux/support — the escalation line', () => {
  test('a case open 15 days is escalated and one open 14 is not, on the tile and in the row', async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-03-15T12:00:00Z'));
    await installSupportReads(page, {
      jobs: [
        job('job-esc00015', 'Open fifteen days', 'acct-zed', '2026-02-28'),
        job('job-esc00014', 'Open fourteen days', 'acct-anchor', '2026-03-01'),
        job('job-esc00005', 'Open five days', 'acct-zed', '2026-03-10'),
      ],
    });
    await mountPage(page, PATH, { titleMatch: /3 open cases/ });

    await expect(page.locator('.catalog dl.kv').nth(1).locator('dd')).toHaveText(['2', '1']);

    await tab(page, 'Active Cases').click();
    await expect.poll(() => column(page, 3)).toEqual(['15d', '14d', '5d']);
    const days = page.locator('.catalog tbody td.num');
    await expect(days.nth(0).locator('strong')).toHaveText('15d');
    await expect(days.nth(1).locator('strong')).toHaveCount(0);
    await expect(days.nth(2).locator('strong')).toHaveCount(0);
  });
});

test.describe('/ux/support — reads once, writes nothing', () => {
  // The shell's own non-GET: App.svelte records every route open
  // (shell/surface-opens.ts). It is the chrome's write, not this page's.
  const SHELL_WRITE = /^\/api\/surface-opens$/;

  test('every tab and every header, clicked, sends no write and re-reads nothing', async ({ page }) => {
    await recordPageRequests(page);
    await installSupportReads(page, { jobs: LINK_JOBS, assets: LINK_ASSETS });
    await mountPage(page, PATH, { titleMatch: /4 open cases/ });
    await readsSettled(page);

    for (const name of ['Active Cases', 'Account Health']) {
      await tab(page, name).click();
      const headers = page.locator('.catalog thead th');
      await expect(headers).toHaveCount(name === 'Active Cases' ? 6 : 5);
      for (const th of await headers.all()) await th.click();
    }
    await tab(page, 'Overview').click();
    await readsSettled(page);

    const opened = await openedRequests(page);
    const writes = opened.filter((o) => o.method !== 'GET' && !SHELL_WRITE.test(o.path));
    expect(writes.map((w) => `${w.method} ${w.path}`)).toEqual([]);
    for (const read of [CASES, ACCOUNTS, ASSETS]) {
      expect(opened.filter((o) => o.method === 'GET' && read.test(o.url)), `${read} read once`).toHaveLength(1);
    }
  });
});
