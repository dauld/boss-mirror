// /it/estate — every control the page renders, pinned (page audit
// 2cff1d6e, step `test`).
//
// estate-page.mocked.spec.ts pins THE LOOPS section (gap 10, 0d9b2960):
// each loop row's cells, its outcome colour, its open packet and a
// failed jobs read per cell. Before this spec the rest of the page — the
// machines, the observed-vs-declared rows, the verdict, the dev door,
// the loading line, the refresh timer and the loops' links as links —
// was reached only by the two crawls, under the mock's `[]` catch-all:
// route-smoke saw a header-only table, outage-crawl saw the failure
// lines, and neither read a word the page says.
//
// The inventory this spec covers, measured on origin/main on 2026-09-25
// (the measure step's controls_md was read on 2026-09-23, before the
// loops section landed with its links):
//   links     2 kinds — a loop's newest finished packet, a loop's open
//                        packet; both land on the job detail surface.
//                        Up to 2 per loop row: 6 declared loops plus one
//                        ops-request row per host declaring `ops-runner`
//   buttons   0         — no manual refresh
//   forms     0, inputs 0
//   snippets  3         — the dev door's copyable command lines
//   reads     4 estate reads (the fourth, the host comparisons scoped
//             since gap 1 — 2d8d983b — and grouped per host since
//             725532ab), plus 2 jobs reads per loop row
//   writes    0
//   timer     1         — every read again each 60 s
//
// THIS STEP PINS WHAT THE PAGE DOES TODAY; it does not fix the gaps.
// Where today's behaviour IS a filed gap, the test says so in its name
// ("CURRENT, gap N (item)") and asserts the current paint, so the car
// that fixes the gap has to flip that assertion — the fix is visible in
// this file rather than passing beside a spec that never noticed.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import { installSmokeMocks } from './_smokeMocks';
import { parseRoute } from '../../src/router';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG['system-estate'].path;
const TITLE = { titleMatch: /The estate/ };

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// The page's three estate reads, as estate.ts spells them — the query
/// is part of the match, so a read that changes shape fails here.
const NODES_READ = /\/api\/estate\/nodes$/;
const OBS_READ = /\/api\/estate\/observations\?limit=20$/;
const CMP_READ = /\/api\/estate\/comparisons\?limit=20$/;
/// The host series, read on its own (gap 1, 2d8d983b): forge compares
/// every 15 minutes and boss-gcp once a day, so the unscoped page of 20
/// almost never holds boss-gcp's row — and grouped per host on the
/// server (725532ab), because even scoped, 50 of 768 rows were forge's.
const HOST_CMP_READ = /\/api\/estate\/comparisons\?scope=host&latest_per=host&limit=50$/;
/// The loops' reads (two per row).
const LOOPS_READ = /\/api\/jobs\?kind=(maintenance-|ops-request)/;

/// Stamps relative to the test's own clock, taken when the read is
/// answered (the estate-page spec's reason: a stamp taken at file load
/// drifts under a slow worker).
const ago = (minutes: number): string => new Date(Date.now() - minutes * 60_000).toISOString();

/// Three live machines and one retired. Addresses are TEST-NET-1
/// (RFC 5737), never an estate address. w-1 declares nothing but its
/// role, so every optional cell reads its dash.
const NODES = [
  { id: 'forge', label: 'forge', role: 'forge', roles: ['cluster-operator', 'ops-runner'], address: '192.0.2.15', cpu: 16, memory_gb: 30, disk_gb: 500, notes: 'the forge host', retired: false },
  { id: 'boss-gcp', label: 'boss-gcp', role: 'gateway-host', roles: ['ops-runner'], address: '192.0.2.20', cpu: 4, memory_gb: 15, disk_gb: 60, notes: null, retired: false },
  { id: 'w-1', label: 'w-1', role: 'talos-worker', roles: [], address: null, cpu: null, memory_gb: null, disk_gb: null, notes: null, retired: false },
  { id: 'old-1', label: 'old-1', role: 'talos-worker', roles: [], address: '192.0.2.99', cpu: 2, memory_gb: 4, disk_gb: 20, notes: 'decommissioned', retired: true },
];

/// The reader serves newest-first envelopes `{payload}`.
const envelope = (payload: Record<string, unknown>) => ({ payload });

/// Newest first: a host-units row (the page discards the scope), the
/// cluster's row, forge's two host rows, then an OLDER boss-gcp host row
/// — the live shape, each row naming only the host it was taken on
/// (observe-host.sh). Newest-per-scope hid boss-gcp's 13 G until
/// 3d1678ba keyed the series per host.
const observations = () => [
  envelope({ scope: 'host-units', observer: 'boss-estate-observe-units', observed_at: ago(1), nodes: [{ id: 'boss-gcp' }] }),
  envelope({
    scope: 'kubernetes-nodes', observer: 'boss-estate-observe', observed_at: ago(3),
    nodes: [{ id: 'cp-1', disk_free_gb: 40 }, { id: 'w-1' }],
  }),
  envelope({
    scope: 'host', observer: 'boss-estate-observe-host', observed_at: ago(180),
    nodes: [{ id: 'forge', disk_free_gb: 210 }],
  }),
  envelope({
    scope: 'host', observer: 'boss-estate-observe-host', observed_at: ago(190),
    nodes: [{ id: 'forge', disk_free_gb: 211 }],
  }),
  envelope({
    scope: 'host', observer: 'boss-estate-observe-host', observed_at: ago(200),
    nodes: [{ id: 'boss-gcp', disk_free_gb: 13 }],
  }),
];

const ZERO = {
  observed: 5, participating_declared: 5, observed_not_declared: 0,
  declared_not_observed: 0, drift: 0, disk_tight: 0, disk_unmeasured: 0,
};

/// A clean cluster comparison, and a host comparison that DRIFTED. The
/// page takes its cluster verdict from this unscoped read and its host
/// lines from the scoped one below — never from this row.
const comparisons = (cluster: Record<string, unknown> = {}, findings: Record<string, unknown> = {}) => [
  envelope({ scope: 'host', observed_at: ago(2), host: 'forge', counts: { observed: 1, observed_not_declared: 0, drift: 1, disk_tight: 0 } }),
  envelope({ scope: 'kubernetes-nodes', observed_at: ago(3), counts: { ...ZERO, ...cluster }, findings }),
];

/// A host comparison as compare_host shapes it (estate_compare.rs):
/// stamped with its host, four counts, no declared total.
const hostCmp = (host: string, minutes: number, counts: Record<string, number> = {}) =>
  envelope({
    scope: 'host', observed_at: ago(minutes), host,
    counts: { observed: 1, observed_not_declared: 0, drift: 0, disk_tight: 0, ...counts },
  });

/// The live shape measured on 2026-09-23, as the grouped read serves it
/// (725532ab: `latest_per=host`, one row per host, `total` counting
/// hosts): forge drifted (memory declared 30, observed 31), and
/// boss-gcp's daily row short of disk (13 G free against a 17 G floor)
/// AND drifted.
const hostComparisons = () => ({
  data: [
    hostCmp('forge', 2, { drift: 1 }),
    hostCmp('boss-gcp', 200, { drift: 1, disk_tight: 1 }),
  ],
  total: 2,
});

/// Two loop packets, so the loops table carries both link kinds.
const WATCHDOG_DONE = 'c0c0c0c0-0000-0000-0000-000000000003';
const UNITS_OPEN = 'e1e1e1e1-0000-0000-0000-000000000008';
const loopAnswer = (url: URL) => {
  const kind = url.searchParams.get('kind');
  const open = url.searchParams.get('status') === 'open';
  if (!open && kind === 'maintenance-cluster-watchdog') {
    return [{ id: WATCHDOG_DONE, status: 'closed', metadata: { outcome: 'completed', closed_at: ago(3) }, steps: [] }];
  }
  if (open && kind === 'maintenance-estate-observe-units') {
    return [{ id: UNITS_OPEN, status: 'open', opened_at: ago(2), metadata: {}, steps: [] }];
  }
  return [];
};

type Answer = 'fixture' | 'empty' | 'down';
type Reads = Readonly<{
  nodes?: Answer; obs?: Answer; cmp?: Answer; host?: Answer;
  cmpBody?: () => unknown; hostBody?: () => unknown;
}>;

/// Every read the page makes, answered; `down` is a 503, `empty` an
/// empty list. Returns the counts of each estate read as it arrives.
async function install(page: Page, reads: Reads = {}): Promise<Record<'nodes' | 'obs' | 'cmp' | 'host', number>> {
  const seen = { nodes: 0, obs: 0, cmp: 0, host: 0 };
  await installSmokeMocks(page);
  const answer = (key: keyof typeof seen, mode: Answer, body: () => unknown) => (r: Route) => {
    seen[key] += 1;
    if (mode === 'down') return json(r, { error: 'estate upstream unavailable' }, 503);
    return json(r, mode === 'empty' ? [] : body());
  };
  await page.route(NODES_READ, answer('nodes', reads.nodes ?? 'fixture', () => NODES));
  await page.route(OBS_READ, answer('obs', reads.obs ?? 'fixture', observations));
  await page.route(CMP_READ, answer('cmp', reads.cmp ?? 'fixture', reads.cmpBody ?? (() => comparisons())));
  await page.route(HOST_CMP_READ, answer('host', reads.host ?? 'fixture', reads.hostBody ?? hostComparisons));
  await page.route(LOOPS_READ, (r) => {
    const data = loopAnswer(new URL(r.request().url()));
    return json(r, { data, total: data.length });
  });
  return seen;
}

const section = (page: Page) => page.locator('.estate-section');
const machines = (page: Page) => page.locator('table.estate-table:not(.estate-loops)');
const obsRows = (page: Page) => page.locator('.estate-obs .estate-obs-row');
const obsRow = (page: Page, scope: string) =>
  obsRows(page).filter({ has: page.locator('.estate-scope', { hasText: new RegExp(`^${scope}$`, 'i') }) });
/// One line per host the host series names (gap 1, 2d8d983b).
const hostRows = (page: Page) => obsRow(page, 'host comparison');
const loops = (page: Page) => page.locator('table.estate-loops');

/// parseRoute reads `window.location.search` for two routes; this is
/// Node, so give it the one field it reads (interaction-crawl and the
/// manual spec do the same).
function route(path: string): ReturnType<typeof parseRoute> {
  (globalThis as { window?: unknown }).window = { location: { search: '', pathname: path } };
  return parseRoute(path);
}

test.describe('/it/estate — the chrome, the loading line and the reads', () => {
  test('the page is the catalogued Estate surface, with its header and four sections in order', async ({ page }) => {
    expect(route(PATH)).toEqual({ kind: 'systemEstate' });
    await install(page);
    await mountPage(page, PATH, TITLE);

    await expect(page.locator('.exec-eyebrow')).toHaveText('IT · Hardware');
    await expect(page.locator('h1').first()).toHaveText('The estate');
    await expect(page.getByText('Declared beside observed — what we meant to have, what a look found, and the difference')).toBeVisible();
    await expect(section(page)).toHaveText([
      '00 — THE MACHINES',
      '01 — OBSERVED vs DECLARED',
      '02 — THE LOOPS',
      '03 — THE DEV WORKSPACE',
    ]);
  });

  test('the page says it is reading until the registry answers', async ({ page }) => {
    await install(page);
    let release: () => void = () => {};
    const held = new Promise<void>((r) => { release = r; });
    await page.route(NODES_READ, async (r) => { await held; await json(r, NODES); });
    await mountPage(page, PATH, TITLE);

    await expect(page.locator('.estate-root > p.estate-quiet')).toHaveText('Reading the registry…');
    await expect(section(page)).toHaveCount(0);
    release();
    await expect(machines(page).locator('tbody tr')).toHaveCount(3);
    await expect(page.getByText('Reading the registry…')).toHaveCount(0);
  });

  // CURRENT, gap 4 (75027a93): both event reads are one unscoped page of
  // 20 rows across every scope, not one scoped read per rendered scope.
  // The host comparisons are the one series read scoped (gap 1,
  // 2d8d983b), because a daily host never sat in the unscoped page, and
  // grouped per host (725532ab), because it did not sit in the scoped one.
  test('CURRENT, gap 4: four estate reads, the two event reads unscoped at limit=20 and the host comparisons scoped and grouped per host; no write, no button, no form', async ({ page }) => {
    const sent: string[] = [];
    page.on('request', (req) => {
      const u = new URL(req.url());
      if (u.pathname.startsWith('/api/estate') || u.pathname.startsWith('/api/jobs')) {
        sent.push(`${req.method()} ${u.pathname}${u.search}`);
      }
    });
    await install(page);
    await mountPage(page, PATH, TITLE);
    await expect(loops(page).locator('a')).toHaveCount(2);

    const estate = sent.filter((s) => s.includes('/api/estate')).sort();
    expect(estate).toEqual([
      'GET /api/estate/comparisons?limit=20',
      'GET /api/estate/comparisons?scope=host&latest_per=host&limit=50',
      'GET /api/estate/nodes',
      'GET /api/estate/observations?limit=20',
    ]);
    // Six declared loops plus the two ops-runner hosts, two reads each.
    expect(sent.filter((s) => s.startsWith('GET /api/jobs?kind=')).length).toBe((6 + 2) * 2);
    expect(sent.filter((s) => !s.startsWith('GET ')), 'the page wrote').toEqual([]);

    const root = page.locator('.estate-root');
    await expect(root.locator('button')).toHaveCount(0);
    await expect(root.locator('form')).toHaveCount(0);
    await expect(root.locator('input, select, textarea')).toHaveCount(0);
  });

  test('every 60 s the page reads the estate again and paints the new answer', async ({ page }) => {
    await page.clock.install();
    const seen = await install(page);
    await mountPage(page, PATH, TITLE);
    await expect(machines(page).locator('tbody tr')).toHaveCount(3);
    expect(seen).toEqual({ nodes: 1, obs: 1, cmp: 1, host: 1 });

    // The next answer declares one more machine; the page must show it.
    await page.route(NODES_READ, (r) => {
      seen.nodes += 1;
      return json(r, [...NODES, { id: 'w-2', label: 'w-2', role: 'talos-worker', roles: [], retired: false }]);
    });
    await page.clock.runFor(60_000);
    await expect.poll(() => ({ ...seen })).toEqual({ nodes: 2, obs: 2, cmp: 2, host: 2 });
    await expect(machines(page).locator('tbody tr')).toHaveCount(4);
    await expect(machines(page).locator('td.estate-id').last()).toHaveText('w-2');
  });
});

test.describe('/it/estate — 00 THE MACHINES', () => {
  // CURRENT, gap 12 (d6d39f60): a retired machine is filtered out with
  // no count of how many were.
  test('one row per live machine, in registry order, every cell verbatim; CURRENT, gap 12: retired machines vanish uncounted', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    await expect(machines(page).locator('thead th')).toHaveText(['machine', 'role', 'address', 'cpu', 'mem', 'disk']);
    const rows = machines(page).locator('tbody tr');
    await expect(rows).toHaveCount(3);
    await expect(rows.nth(0).locator('td')).toHaveText(['forge', 'forge · cluster-operator · ops-runner', '192.0.2.15', '16', '30G', '500G']);
    await expect(rows.nth(1).locator('td')).toHaveText(['boss-gcp', 'gateway-host · ops-runner', '192.0.2.20', '4', '15G', '60G']);
    // A machine that declares nothing reads a dash, never a zero.
    await expect(rows.nth(2).locator('td')).toHaveText(['w-1', 'talos-worker', '—', '—', '—', '—']);
    // The notes ride as the row's tooltip; no notes, an empty one.
    await expect(rows.nth(0)).toHaveAttribute('title', 'the forge host');
    await expect(rows.nth(1)).toHaveAttribute('title', '');

    await expect(page.locator('.estate-root').getByText('old-1')).toHaveCount(0);
    await expect(page.locator('.estate-root').getByText(/retired/i)).toHaveCount(0);
  });

  // CURRENT, gap 12 (d6d39f60): an empty registry paints a table of
  // headers and nothing else — no "0 machines declared" line. It is at
  // least not a failure.
  test('CURRENT, gap 12: an empty registry paints a header-only table, and no failure', async ({ page }) => {
    await install(page, { nodes: 'empty' });
    await mountPage(page, PATH, TITLE);

    await expect(machines(page).locator('thead th')).toHaveCount(6);
    await expect(machines(page).locator('tbody tr')).toHaveCount(0);
    await expect(page.locator('.estate-root').getByText(/0 machines/)).toHaveCount(0);
    await expect(loops(page).locator('tbody tr')).toHaveCount(6);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('an unreachable registry says so in the page\'s words, and draws no table of machines', async ({ page }) => {
    await install(page, { nodes: 'down' });
    await mountPage(page, PATH, TITLE);

    const fail = page.locator(`p.estate-fail${FAILURE_MARKER}`).first();
    await expect(fail).toHaveText(
      'The registry did not answer: /api/estate/nodes: HTTP 503. This page refuses to guess — an unreachable registry is not an empty estate.',
    );
    await expect(machines(page)).toHaveCount(0);
    // The loops still read: the runner row kept, unfiltered, with no host.
    await expect(loops(page).locator('tbody tr')).toHaveCount(7);
    await expect(loops(page).locator('tr[data-loop="ops-request"] td').nth(1)).toHaveText('not named on the packet');
  });
});

test.describe('/it/estate — 01 OBSERVED vs DECLARED', () => {
  // Gap 2 (3d1678ba), FIXED: the newest row per SCOPE won, so this test
  // asserted boss-gcp's older host row — 13G free — never showed. The
  // host series is now keyed per host, one line each.
  // CURRENT, gap 3 (d5efb80d): the host-units row is read and discarded.
  // CURRENT, gap 8 (e1eb34bc): a 3-hour-old row and a 3-minute-old one
  // both read "today".
  test('the cluster row and one host row per host, free space per machine; gap 2 fixed, CURRENT gaps 3 and 8', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    // The cluster scope, one observation line per host, the cluster
    // comparison, and one host comparison per host (gap 1, pinned below).
    await expect(obsRows(page)).toHaveCount(6);
    await expect(obsRow(page, 'kubernetes-nodes').locator('span')).toHaveText([
      'kubernetes-nodes',
      '2 machines seen by boss-estate-observe — cp-1: 40G free — w-1: free space unread',
      'today',
    ]);
    // Each host's NEWEST reading, in host order: forge's older 211G row
    // is hidden by its newer one, and boss-gcp's daily row — older than
    // both — keeps a line of its own.
    const hosts = obsRow(page, 'host');
    await expect(hosts).toHaveCount(2);
    await expect(hosts.nth(0).locator('span')).toHaveText([
      'host',
      'boss-gcp: 13G free — seen by boss-estate-observe-host',
      'today',
    ]);
    await expect(hosts.nth(1).locator('span')).toHaveText([
      'host',
      'forge: 210G free — seen by boss-estate-observe-host',
      'today',
    ]);
    await expect(page.locator('.estate-obs').getByText(/211G/)).toHaveCount(0);
    await expect(page.locator('.estate-obs').getByText(/units/)).toHaveCount(0);
  });

  test('gap 2: a host whose row carries no free-space reading says so, like a cluster node', async ({ page }) => {
    await install(page);
    await page.route(OBS_READ, (r) => json(r, [
      envelope({ scope: 'host', observer: 'boss-estate-observe-host', observed_at: ago(5), nodes: [{ id: 'forge' }] }),
    ]));
    await mountPage(page, PATH, TITLE);

    await expect(obsRow(page, 'host').locator('span').nth(1)).toHaveText(
      'forge: free space unread — seen by boss-estate-observe-host',
    );
  });

  test('a clean cluster comparison reads green "no drift"', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    const cmp = obsRow(page, 'comparison');
    await expect(cmp).toHaveCount(1);
    await expect(cmp.locator('span').nth(1)).toHaveText('5 observed, 5 declared — no drift');
    await expect(cmp.locator('span').nth(1)).toHaveClass(/\bestate-ok\b/);
    await expect(cmp.locator('span').nth(2)).toHaveText('today');
  });

  // Gap 1 (2d8d983b), FIXED: until this pin the page rendered only the
  // cluster's verdict, and this test asserted that the drifted host row
  // was NOT shown. The fixture is the live shape of 2026-09-23.
  test('gap 1: each host\'s newest comparison reads beside the cluster\'s, a drift row and a disk_tight row in amber', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    await expect(hostRows(page)).toHaveCount(2);
    const verdicts = hostRows(page).locator('span:nth-child(2)');
    await expect(verdicts).toHaveText([
      'boss-gcp: 1 drifted from declaration; 1 short of disk',
      'forge: 1 drifted from declaration',
    ]);
    for (let i = 0; i < 2; i += 1) {
      await expect(verdicts.nth(i)).toHaveClass(/\bestate-drift\b/);
    }
    // A read that is every host says nothing about its reach.
    await expect(page.locator('.estate-cover')).toHaveCount(0);
  });

  test('gap 1: a host that matches its declaration reads green, with no declared total to print as 0', async ({ page }) => {
    await install(page, {
      hostBody: () => ({ data: [hostCmp('forge', 2), hostCmp('boss-gcp', 600)], total: 2 }),
    });
    await mountPage(page, PATH, TITLE);

    const verdict = hostRows(page).filter({ hasText: 'forge:' }).locator('span:nth-child(2)');
    await expect(verdict).toHaveText('forge: 1 observed — no drift');
    await expect(verdict).toHaveClass(/\bestate-ok\b/);
  });

  // FLIPPED by 725532ab. This pin asserted a coverage line — "The newest
  // 1 of 612 host comparisons, back to …: a host whose last comparison
  // is older has no line here" — because the read was a scoped page of
  // 50 and boss-gcp's daily row fell off it about half of every day
  // (measured 2026-09-25: 50 of 768, all forge's). The read is grouped
  // per host on the server now, so a whole answer draws no coverage
  // line, and a declared host missing from it gets a line of its own.
  test('gap 1: every declared host gets its line — one with no comparison says so in amber, and a whole read draws no coverage line', async ({ page }) => {
    await install(page, { hostBody: () => ({ data: [hostCmp('forge', 2, { drift: 1 })], total: 1 }) });
    await mountPage(page, PATH, TITLE);

    const verdicts = hostRows(page).locator('span:nth-child(2)');
    await expect(verdicts).toHaveText([
      'boss-gcp: no host comparison recorded',
      'forge: 1 drifted from declaration',
    ]);
    await expect(verdicts.nth(0)).toHaveClass(/\bestate-drift\b/);
    // w-1 is a cluster node (the cluster verdict speaks for it) and
    // old-1 is retired: neither is owed a host line.
    await expect(hostRows(page).filter({ hasText: /w-1|old-1/ })).toHaveCount(0);
    await expect(page.locator('.estate-cover')).toHaveCount(0);
  });

  test('gap 1: a host read that is not every host says so, and never tells a declared host it has none', async ({ page }) => {
    // More hosts than the page held: the absence is the read's.
    await install(page, { hostBody: () => ({ data: [hostCmp('forge', 2, { drift: 1 })], total: 3 }) });
    await mountPage(page, PATH, TITLE);

    await expect(hostRows(page).locator('span:nth-child(2)')).toHaveText([
      'boss-gcp: not among the 1 of 3 hosts this read returned',
      'forge: 1 drifted from declaration',
    ]);
    await expect(page.locator('.estate-cover')).toHaveText(
      'The host read returned 1 of 3 hosts: a host past it has no comparison shown here.',
    );
  });

  test('gap 1: an empty host series gives each declared host its line', async ({ page }) => {
    await install(page, { host: 'empty' });
    await mountPage(page, PATH, TITLE);
    await expect(hostRows(page).locator('span:nth-child(2)')).toHaveText([
      'boss-gcp: no host comparison recorded',
      'forge: no host comparison recorded',
    ]);
  });

  test('gap 1: an empty host series with the registry unread says so in its own line', async ({ page }) => {
    await install(page, { host: 'empty', nodes: 'down' });
    await mountPage(page, PATH, TITLE);
    await expect(hostRows(page).locator('span')).toHaveText(['host comparison', 'no host comparison recorded yet']);
  });

  test('gap 1: a failed host read fails in the page\'s words, beside a cluster verdict that answered', async ({ page }) => {
    await install(page, { host: 'down' });
    await mountPage(page, PATH, TITLE);
    await expect(page.locator(`p.estate-fail${FAILURE_MARKER}`)).toHaveText([
      'Host comparisons unavailable: /api/estate/comparisons?scope=host&latest_per=host&limit=50: HTTP 503',
    ]);
    await expect(hostRows(page)).toHaveCount(0);
    await expect(obsRow(page, 'comparison').locator('span').nth(1)).toHaveText('5 observed, 5 declared — no drift');
  });

  test('a comparison that disagrees names every count that does, in amber', async ({ page }) => {
    await install(page, {
      cmpBody: () => comparisons({ observed_not_declared: 1, declared_not_observed: 2, drift: 3, disk_tight: 1, disk_unmeasured: 2 }),
    });
    await mountPage(page, PATH, TITLE);

    const verdict = obsRow(page, 'comparison').locator('span').nth(1);
    await expect(verdict).toHaveText(
      '1 in the cluster but undeclared; 2 declared but not seen; 3 drifted from declaration; 1 short of disk; 2 with no free-space reading',
    );
    await expect(verdict).toHaveClass(/\bestate-drift\b/);
  });

  // CURRENT, gap 5 (ea5e0e8b): not_ready is a finding with no count.
  // CURRENT, gap 6 (c2373cc4): dead_letters_unrecorded and
  // dispatcher_unread are dropped by the parser.
  test('CURRENT, gaps 5 and 6: a NotReady node and unrecorded dead letters still read green "no drift"', async ({ page }) => {
    await install(page, {
      cmpBody: () => comparisons(
        { dead_letters_unrecorded: 4 },
        { not_ready: [{ id: 'w-1' }], dispatcher_unread: [{ subject: 'dead-letter' }] },
      ),
    });
    await mountPage(page, PATH, TITLE);

    const verdict = obsRow(page, 'comparison').locator('span').nth(1);
    await expect(verdict).toHaveText('5 observed, 5 declared — no drift');
    await expect(verdict).toHaveClass(/\bestate-ok\b/);
  });

  // CURRENT, gap 12 (d6d39f60): with no cluster comparison in the page
  // there is no comparison row at all — silence, not a counted empty.
  test('empty series say "no observation recorded yet"; CURRENT, gap 12: an empty comparison read paints nothing', async ({ page }) => {
    await install(page, { obs: 'empty', cmp: 'empty', host: 'empty' });
    await mountPage(page, PATH, TITLE);

    // The two scopes and one host-comparison line per declared host
    // (gap 1; per declared host since 725532ab — forge and boss-gcp).
    await expect(obsRows(page)).toHaveCount(4);
    await expect(obsRow(page, 'host comparison').locator('span:nth-child(2)')).toHaveText([
      'boss-gcp: no host comparison recorded',
      'forge: no host comparison recorded',
    ]);
    await expect(obsRow(page, 'kubernetes-nodes').locator('span').nth(1)).toHaveText('no observation recorded yet');
    await expect(obsRow(page, 'host').locator('span').nth(1)).toHaveText('no observation recorded yet');
    await expect(obsRow(page, 'comparison')).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  // CURRENT, gap 12 (d6d39f60): the comparison line sits inside the
  // observations-ready branch, so a comparison read that ANSWERED is
  // hidden whenever observations failed — the host lines too (gap 1
  // placed them beside the cluster verdict, inside the same branch).
  test('failed observations say so; CURRENT, gap 12: they hide a comparison that answered', async ({ page }) => {
    await install(page, { obs: 'down' });
    await mountPage(page, PATH, TITLE);

    await expect(page.locator(`p.estate-fail${FAILURE_MARKER}`)).toHaveText([
      'Observations unavailable: /api/estate/observations?limit=20: HTTP 503',
    ]);
    await expect(obsRows(page)).toHaveCount(0);
    await expect(page.getByText('5 observed, 5 declared — no drift')).toHaveCount(0);
    await expect(page.getByText(/^forge: /)).toHaveCount(0);
  });

  test('failed comparisons say so beside observations that answered, never as "no drift"', async ({ page }) => {
    await install(page, { cmp: 'down' });
    await mountPage(page, PATH, TITLE);

    await expect(page.locator(`p.estate-fail${FAILURE_MARKER}`)).toHaveText([
      'Comparisons unavailable: /api/estate/comparisons?limit=20: HTTP 503',
    ]);
    // The cluster scope and one observation line per host (3d1678ba),
    // plus the host comparison lines: their series is its own read, and
    // it answered.
    await expect(obsRows(page)).toHaveCount(5);
    await expect(hostRows(page)).toHaveCount(2);
    await expect(page.getByText(/no drift/)).toHaveCount(0);
  });
});

test.describe('/it/estate — 02 THE LOOPS, as links', () => {
  test('the hint reads verbatim, and every link lands on the catalogued job detail surface', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    await expect(page.locator('p.estate-hint').first()).toHaveText(
      'Did each loop run: its newest finished packet (outcome and age) and any packet still open, each linked. The host is the one the packet names; where a packet names none, the page says so rather than guess.',
    );
    await expect(loops(page).locator('thead th')).toHaveText(['loop', 'host', 'newest finished', 'open']);
    const links = loops(page).locator('a');
    await expect(links).toHaveCount(2);
    const hrefs = await links.evaluateAll((as) => as.map((a) => a.getAttribute('href')));
    expect(hrefs).toEqual([
      `${ROUTE_CATALOG.jobs.path}/${WATCHDOG_DONE}`,
      `${ROUTE_CATALOG.jobs.path}/${UNITS_OPEN}`,
    ]);
    for (const [href, jobId] of [[hrefs[0]!, WATCHDOG_DONE], [hrefs[1]!, UNITS_OPEN]]) {
      expect(route(href)).toEqual({ kind: 'jobDetail', jobId });
    }
    await expect(links).toHaveText(['completed', 'open']);
  });

  for (const [what, name, id] of [
    ['newest finished', 'completed', WATCHDOG_DONE],
    ['open', 'open', UNITS_OPEN],
  ] as const) {
    test(`a loop's ${what} link opens the packet, and back returns to the estate`, async ({ page }) => {
      await install(page);
      await mountPage(page, PATH, TITLE);

      await loops(page).getByRole('link', { name, exact: true }).click();
      await expect(page).toHaveURL(new RegExp(`/ux/jobs/${id}$`));
      await page.goBack();
      await expect(page).toHaveURL(new RegExp(`${PATH}$`));
      await expect(page.locator('h1').first()).toHaveText('The estate');
      await expect(loops(page).getByRole('link', { name, exact: true })).toHaveAttribute('href', `/ux/jobs/${id}`);
    });
  }
});

test.describe('/it/estate — 03 THE DEV WORKSPACE', () => {
  test('three numbered steps, each with its reason and one copyable command, verbatim', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);

    const door = page.locator('.estate-door');
    await expect(door.locator('p.estate-hint').first()).toHaveText(
      'The workspace answers on dev.algedonic.dev, from anywhere, behind Cloudflare Access. There is no VPN to join and no key to install: the edge asks who you are and issues a certificate that lasts the session. Three lines, the first two once per machine.',
    );
    await expect(door.locator('p.estate-hint strong')).toHaveText([
      '1. Install cloudflared, once per machine',
      '2. Teach ssh the route, once per machine',
      '3. Open the workspace',
    ]);
    const snippets = door.locator('pre.estate-snippet');
    await expect(snippets).toHaveText([
      'cloudflared --version',
      "grep -qsF 'Match host dev.algedonic.dev ' ~/.ssh/config || cloudflared access ssh-config --hostname dev.algedonic.dev --short-lived-cert | sed '/^Add to your/d' >> ~/.ssh/config",
      'ssh root@dev.algedonic.dev',
    ]);
    // Copyable without a button: one click selects the whole line.
    for (let i = 0; i < 3; i += 1) {
      await expect(snippets.nth(i)).toHaveCSS('user-select', 'all');
    }
    await expect(door.locator('p.estate-hint').last()).toHaveText(
      'Inside: the durable tmux session is dev — attach with /work/dev-session.sh, detach with ctrl-b d. For the browser instead, run claude remote-control inside the session and drive it from claude.ai.',
    );
  });

  test('the door renders from the page itself, so it stands when every read fails', async ({ page }) => {
    await install(page, { nodes: 'down', obs: 'down', cmp: 'down', host: 'down' });
    await mountPage(page, PATH, TITLE);

    await expect(page.locator(`p.estate-fail${FAILURE_MARKER}`)).toHaveCount(2);
    await expect(page.locator('pre.estate-snippet')).toHaveCount(3);
  });
});
