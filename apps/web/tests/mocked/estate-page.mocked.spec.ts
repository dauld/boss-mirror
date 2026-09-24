// /it/estate — THE LOOPS (backlog 0d9b2960; page audit 2cff1d6e, GAP 10).
//
// The page showed none of the estate's own working or finished packets,
// so "did the loop run" had no answer on it — while the log held ~2,000
// of each kind (measured at filing: ops-request 1956, observe-units
// 1890, observe-host 643, cluster-watchdog 1868). This spec pins the
// section that answers it: per loop, and per host where the packet names
// one, the newest terminal (outcome and age) and any open packet, each
// linked to the packet.
//
// Before it, /it/estate was reached only by the crawls (route-smoke,
// outage-crawl), under the `[]` catch-all.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PATH = '/it/estate';

/// The loops as the page renders them TODAY, in order — written out
/// rather than imported, so a change to the list shows up here as a
/// changed expectation (estate.test.ts holds the list to the registry).
const LOOP_KINDS = [
  'maintenance-forge-converge',
  'maintenance-boss-gcp-converge',
  'maintenance-cluster-converge',
  'maintenance-cluster-watchdog',
  'maintenance-estate-observe-host',
  'maintenance-estate-observe-units',
];

/// How late the loops' reads answer — the manual-page fix's figure
/// (0eda772f): long enough that a snapshot straight after mount always
/// wins the race, short beside the suite's stated expect budget.
const LOOPS_READ_DELAY_MS = 750;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// Two hosts declare the runner role; the worker declares nothing, so it
/// gets no ops-request row.
const NODES = [
  { id: 'forge', label: 'forge', role: 'forge', roles: ['cluster-operator', 'ops-runner'], retired: false },
  { id: 'boss-gcp', label: 'boss-gcp', role: 'gateway-host', roles: ['ops-runner', 'off-cluster-observer'], retired: false },
  { id: 'w-1', label: 'w-1', role: 'talos-worker', roles: [], retired: false },
];

/// Stamps relative to the test's own clock, so the ages read the same on
/// any day this runs — taken when the read is ANSWERED, not when the
/// file loads. The page rounds an age to the nearest minute, so a stamp
/// taken at load read "9m ago" as "10m ago" once 30 s had passed, and a
/// worker that loads this file and then runs its tests for longer than
/// that under load (or under --repeat-each) failed on the clock, not the
/// page: measured 4 in 120 runs while fixing a9c76cf7.
const ago = (minutes: number): string => new Date(Date.now() - minutes * 60_000).toISOString();

const packet = (
  id: string, status: string, md: Record<string, unknown>, runMd: Record<string, unknown> = {},
): Record<string, unknown> => ({
  id, status, opened_at: md.opened_at ?? ago(60), metadata: md,
  steps: [{ spec_slug: 'run', metadata: runMd }],
});

/// The closed read per (kind, host) — the newest terminal. Missing key:
/// a kind with no finished run at all.
const closed = (): Readonly<Record<string, Record<string, unknown>>> => ({
  'maintenance-forge-converge': packet('f0f0f0f0-0000-0000-0000-000000000001', 'closed',
    { outcome: 'failed', closed_at: ago(7) }, { node_id: 'forge', result: 'exit-code' }),
  'maintenance-boss-gcp-converge': packet('b0b0b0b0-0000-0000-0000-000000000002', 'closed',
    { outcome: 'completed', closed_at: ago(12) }, { node_id: 'boss-gcp', result: 'ok' }),
  'maintenance-cluster-watchdog': packet('c0c0c0c0-0000-0000-0000-000000000003', 'closed',
    { outcome: 'completed', closed_at: ago(3) }),
  'maintenance-estate-observe-host': packet('d0d0d0d0-0000-0000-0000-000000000004', 'closed',
    { outcome: 'completed', closed_at: ago(180) }),
  'maintenance-estate-observe-units': packet('e0e0e0e0-0000-0000-0000-000000000005', 'closed',
    { outcome: 'completed', closed_at: ago(4) }),
  'ops-request:forge': packet('a1a1a1a1-0000-0000-0000-000000000006', 'closed',
    { outcome: 'answered', host: 'forge', closed_at: ago(9) }),
  'ops-request:boss-gcp': packet('a2a2a2a2-0000-0000-0000-000000000007', 'closed',
    { outcome: 'refused', host: 'boss-gcp', closed_at: ago(30) }),
});

/// The open read: one observe-units run in flight.
const open = (): Readonly<Record<string, Record<string, unknown>>> => ({
  'maintenance-estate-observe-units': packet('e1e1e1e1-0000-0000-0000-000000000008', 'open',
    { opened_at: ago(2) }),
});

/// Which fixture a jobs read asks for: `kind`, or `kind:host` when the
/// read carries the metadata filter.
function keyOf(url: URL): string {
  const kind = url.searchParams.get('kind') ?? '';
  const md = url.searchParams.get('metadata');
  const host = md ? (JSON.parse(md) as { host?: string }).host : undefined;
  return host ? `${kind}:${host}` : kind;
}

async function install(page: Page, jobs: 'fixtures' | 'down'): Promise<void> {
  await installSmokeMocks(page);
  await page.route(/\/api\/estate\/nodes$/, (r) => json(r, NODES));
  // Only the loops' reads — the chrome's own job reads keep the floor.
  //
  // They answer LATE on purpose (backlog a9c76cf7). The page paints its
  // h1 at once and the loops table only when every estate read has
  // answered, so mountPage returns before there is a row to read. This
  // spec used to snapshot the cells straight after mount and redded gate
  // a7b1b3df with [] on 2026-09-24; the delay makes that race the
  // ordinary case, so a snapshot taken before the table is drawn fails
  // every run rather than one run in a busy hour.
  await page.route(/\/api\/jobs\?kind=(maintenance-|ops-request)/, async (r) => {
    await new Promise((ok) => setTimeout(ok, LOOPS_READ_DELAY_MS));
    if (jobs === 'down') return r.fulfill({ status: 502, contentType: 'text/plain', body: 'jobs upstream unavailable' });
    const url = new URL(r.request().url());
    const table = url.searchParams.get('status') === 'open' ? open() : closed();
    const row = table[keyOf(url)];
    const data = row ? [row] : [];
    return json(r, { data, total: data.length });
  });
}

const loopsTable = (page: Page) => page.locator('table.estate-loops');
const row = (page: Page, kind: string, nth = 0) => loopsTable(page).locator(`tr[data-loop="${kind}"]`).nth(nth);

/// A row's cells as a LOCATOR, not a snapshot of their text (backlog
/// a9c76cf7): `toHaveText` on it retries until the table is drawn,
/// where `expect(await allInnerTexts()).toEqual(...)` read whatever was
/// there the instant mountPage returned — `[]` under gate load. Inner
/// text, as the snapshot read it.
const cells = (page: Page, kind: string, nth = 0) => row(page, kind, nth).locator('td');
const TEXT = { useInnerText: true } as const;

test.describe('/it/estate — the loops', () => {
  test('one row per declared loop, then one ops-request row per host declaring the runner role', async ({ page }) => {
    await install(page, 'fixtures');
    await mountPage(page, PATH);
    await expect(page.getByText('02 — THE LOOPS')).toBeVisible();
    // Polled, not snapshotted: the order is read again until it holds.
    await expect
      .poll(() => loopsTable(page).locator('tbody tr').evaluateAll((rows) =>
        rows.map((r) => r.getAttribute('data-loop')),
      ))
      .toEqual([...LOOP_KINDS, 'ops-request', 'ops-request']);
  });

  test('each row reads its newest terminal as outcome and age, linked to the packet', async ({ page }) => {
    await install(page, 'fixtures');
    await mountPage(page, PATH);

    // The watchdog: its packet names no host, and the page says so.
    await expect(cells(page, 'maintenance-cluster-watchdog')).toHaveText([
      'cluster watchdog', 'not named on the packet', 'completed 3m ago', 'none',
    ], TEXT);
    const done = row(page, 'maintenance-cluster-watchdog').locator('td.estate-loop-latest a');
    await expect(done).toHaveAttribute('href', '/ux/jobs/c0c0c0c0-0000-0000-0000-000000000003');
    await expect(done).toHaveClass(/estate-ok/);

    // A converge names its host on the run step; a failed run LOOKS failed.
    await expect(cells(page, 'maintenance-forge-converge')).toHaveText([
      'forge converge', 'forge', 'failed 7m ago', 'none',
    ], TEXT);
    await expect(row(page, 'maintenance-forge-converge').locator('td.estate-loop-latest a')).toHaveClass(/estate-drift/);
    await expect(cells(page, 'maintenance-boss-gcp-converge').nth(1)).toHaveText('boss-gcp', TEXT);

    // Hours past the hour mark, not "today".
    await expect(cells(page, 'maintenance-estate-observe-host').nth(2)).toHaveText('completed 3h ago', TEXT);
  });

  test('an open packet is shown and linked beside the newest terminal', async ({ page }) => {
    await install(page, 'fixtures');
    await mountPage(page, PATH);
    await expect(cells(page, 'maintenance-estate-observe-units')).toHaveText([
      'observe units', 'not named on the packet', 'completed 4m ago', 'open 2m ago',
    ], TEXT);
    await expect(row(page, 'maintenance-estate-observe-units').locator('td.estate-loop-open a'))
      .toHaveAttribute('href', '/ux/jobs/e1e1e1e1-0000-0000-0000-000000000008');
  });

  test('a loop with no finished run says so, rather than rendering blank', async ({ page }) => {
    await install(page, 'fixtures');
    await mountPage(page, PATH);
    await expect(cells(page, 'maintenance-cluster-converge').nth(2)).toHaveText('no finished run recorded', TEXT);
  });

  test('the ops-request loop is split per host by the server filter, and a refusal reads as trouble', async ({ page }) => {
    await install(page, 'fixtures');
    await mountPage(page, PATH);
    await expect(cells(page, 'ops-request', 0)).toHaveText(['ops-request', 'forge', 'answered 9m ago', 'none'], TEXT);
    await expect(cells(page, 'ops-request', 1)).toHaveText(['ops-request', 'boss-gcp', 'refused 30m ago', 'none'], TEXT);
    await expect(row(page, 'ops-request', 1).locator('td.estate-loop-latest a')).toHaveClass(/estate-drift/);
  });

  test('an unreachable jobs API marks every loop cell failed — never a loop that did not run', async ({ page }) => {
    await install(page, 'down');
    await mountPage(page, PATH);
    const rows = loopsTable(page).locator('tbody tr');
    await expect(rows).toHaveCount(LOOP_KINDS.length + 2);
    await expect(loopsTable(page).locator(FAILURE_MARKER)).toHaveCount((LOOP_KINDS.length + 2) * 2);
    await expect(loopsTable(page).getByText('no finished run recorded')).toHaveCount(0);
    await expect(loopsTable(page).getByText('none', { exact: true })).toHaveCount(0);
  });
});
