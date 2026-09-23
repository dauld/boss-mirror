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
// able to notice. The gate legs below render against MODULES_LIVE, the
// manifest the live gateway served on 2026-09-19 (`"modules":{}`), and
// against a manifest that lists ONLY support — so a gate keyed on any
// other module id fails here by name.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks, installTenantManifest, MODULES_LIVE } from './_smokeMocks';

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

async function installSupportReads(page: Page): Promise<void> {
  await installSmokeMocks(page);
  await page.route(CASES, (r) => json(r, { data: JOBS, total: JOBS.length, limit: 5000, offset: 0 }));
  await page.route(ACCOUNTS, (r) =>
    json(r, { data: ACCOUNT_ROWS, total: ACCOUNT_ROWS.length, limit: 1000, offset: 0 }),
  );
  await page.route(ASSETS, (r) => json(r, { data: [], total: 0, limit: 1000, offset: 0 }));
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
    await installTenantManifest(page, MODULES_LIVE);
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
