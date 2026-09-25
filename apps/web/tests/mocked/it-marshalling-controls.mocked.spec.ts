// /it/operate/marshalling — every control the page renders, pinned
// (page audit 7c228914, step `test`).
//
// The route is an ALIAS: router.ts resolves it to the same
// `systemYardFloor` route as the floor page /it/yard/marshalling (the
// Marshalling Yard sidebar row until car N1 of design e765b3fc folded
// it into the Department Map row; the alias and the floor retire with
// car N3). So this spec mounts the alias and asserts it IS the floor,
// then pins the surface.
// it-region-map.mocked.spec.ts pins the region MAP's platforms, the
// overlap line and the waits count at the floor's path; this spec
// pins what nothing did — every control, each read's own failure line,
// the empty leg, and the words the page says.
//
// The inventory, measured on origin/main 507d2308 on 2026-09-25 (the
// measure step read 25f5e8b8 on 2026-09-23; since then the page gained
// a breadcrumb, the HUD frame and the region's own rails, and its
// overlap line and waits count):
//   links     3 kinds — the breadcrumb "Department Map" (→ /it); one per
//                        row of the waits table, up to 12 (→ the job
//                        detail surface); one per failed or unjudged
//                        machine in the HUD frame (→ that machine's
//                        region, selected on the map: /it?at=<region>)
//   buttons   4         — "← the world" (→ /it); the window 24 h / 3 d / 7 d
//   keys      1         — Escape (→ /it)
//   forms     0, inputs 0
//   reads     5         — /api/yard/regions, /api/yard/borders (10 s);
//                        /api/stations/load, /api/stations/flow,
//                        /api/jobs/queue-age (60 s, and on every window click)
//   writes    0
//
// THIS STEP PINS WHAT THE PAGE DOES TODAY; it does not fix the gaps.
// Where today's behaviour IS a filed gap, the test says so in its name
// ("CURRENT, gap N (item)") and asserts the current paint, so the car
// that fixes the gap has to flip that assertion — the fix is visible in
// this file rather than passing beside a spec that never noticed. The
// gaps measured on 2026-09-23 and where each stands on 507d2308:
//   1  ba1fcc6d  open    — the constraint is named from the countable queues only
//   2  6c06ef65  FIXED   — server (regions.rs marshalling_stations reads resolved steps)
//   3  140a2222  FIXED   — the overlap line; pinned here on the alias too
//   4  f49f21b7  open    — loading-dock stands on the marshalling floor
//   5  c4c77ddd  FIXED on main, item still open — the region's own rails
//                         in and out (design 62de32ae decision 7)
//   6  4142d821  FIXED   — the HUD frame's stuck and waiting, side by side
//   7  f9b75688  open    — WORKING (claimed vs ready) is not shown
//   8  18683a0a  FIXED   — the waits count line
//   9  fdc0ea0b  open    — no station opens its queue
//   10 67825067  FIXED   — a malformed 200 from any board read is its
//                         failure line (c3e4edcc's sweep had closed
//                         without reaching this page)
//   11 8dcd28ce  open    — a denied scope (server) paints as a clear yard
//   12 371aa184  open    — the failure lines are pinned HERE (that half is
//                         this car's); the envelope's window_hours and
//                         unsequenced window reads are still current

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import { YARD_BORDERS, YARD_REGIONS, installSmokeMocks } from './_smokeMocks';
import { parseRoute } from '../../src/router';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { TERRITORIES } from '../../src/it/yard/world';
import { floorHref, regionHref } from '../../src/it/yard/regions';

const PATH = '/it/operate/marshalling';
// The region's floor page. It was catalogued as the Marshalling Yard
// sidebar row until car N1 of design e765b3fc (2026-09-25), which folded
// the row into the one Department Map row; the floor still answers here
// until car N3 retires it.
const FLOOR = floorHref('marshalling');
const TITLE = { titleMatch: /IT · Marshalling/ };

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

/// The board's three reads, as marshalling.ts spells them.
const LOAD_READ = /\/api\/stations\/load$/;
const FLOW_READ = /\/api\/stations\/flow\?window_hours=\d+$/;
const WAITS_READ = /\/api\/jobs\/queue-age$/;

// ---------------------------------------------------------------------
// Fixtures — the live shape of 2026-09-23, cut down.
// ---------------------------------------------------------------------

const trend = { metric: 'served', unit: 'per day', current: 54, previous: 19, samples: 5, previous_samples: 5 };

/// Every territory, so the world draws when a control leaves for it;
/// marshalling carries the live head.
const REGIONS = {
  window_hours: 24,
  now: '2026-09-25T05:00:00Z',
  regions: TERRITORIES.map(({ name }) =>
    name === 'marshalling'
      ? { name, count: 303, unit: 'packets at stations', state: 'attention', why: '303 packets standing at 3 stations', trend }
      : { name, count: 0, state: 'clear', why: 'nothing here', trend: { ...trend, current: null, previous: null, samples: 0, previous_samples: 0 } },
  ),
  // The HUD's queue-management row: stuck and waiting side by side
  // (design cf820810 Q1, gap 6).
  thirds: [
    {
      third: 'queue-management',
      regions: ['receiving', 'marshalling'],
      balance: { unit: 'packets', in_means: 'triaged', out_means: 'taken off a station', in: 215, out: 68, net: 147, in_count: 215, out_count: 68, why: null },
      stuck: { stuck: 2, waiting: 4, unknown: [], oldest_hours: 72, regions: ['marshalling'] },
    },
  ],
  // One failed machine, so the HUD lists it as a link.
  machines: {
    running: 0, idle: 0, failed: 1, unknown: 0, total: 1,
    failed_or_unknown: [
      { region: 'marshalling', id: 'station:q.platform-admin.sign-off', name: 'sign-off station', state: 'failed', why: 'over its bound' },
    ],
  },
};

const rail = (from: string, to: string, over: Record<string, unknown>) => ({
  from, to, crossing: 'a packet crossed', state: 'clear', why: 'nothing waiting',
  rate: { metric: 'crossings', unit: 'per day', current: 5, previous: 4, samples: 5, previous_samples: 4 },
  last_crossed: null, waiting: 0, holds: [],
  machine: { name: 'nothing', kind: 'actors', last_fired: null, silent_for_minutes: null,
    expected_every_minutes: null, silent: null, why: '' },
  ...over,
});

/// Marshalling's two rails and one that is not its own.
const BORDERS = {
  window_hours: 24,
  now: '2026-09-25T05:00:00Z',
  borders: [
    rail('receiving', 'marshalling', { waiting: 12 }),
    rail('marshalling', 'shop-floor', {
      rate: { metric: 'crossings', unit: 'per day', current: 215, previous: 68, samples: 5, previous_samples: 5 },
      waiting: 303, state: 'attention', why: '303 waiting against 68 a day',
      machine: { name: 'dispatch-agent-runs', kind: 'dispatcher-rule', last_fired: '2026-09-25T04:53:00Z',
        silent_for_minutes: 7, expected_every_minutes: null, silent: false, why: '' },
    }),
    rail('gates', 'dock', {}),
  ],
};

/// Five stations: the two blind queues that hold nearly everything, a
/// counted one, the dock's station (gap 4) and a clear one. Depths sum
/// to 517 over 304 distinct packets — every agent-station packet also
/// stands in the task queue.
const LOAD = {
  data: [
    { station: 'q.platform-admin.task', kind: 'constraint', depth: 297, wip_limit: 24, over_limit: true, oldest_age_days: 6, capability_roles: ['platform-admin'], also_elsewhere: 213 },
    { station: 'a.platform-admin.opus-5-1m', kind: 'constraint', depth: 213, wip_limit: null, over_limit: false, oldest_age_days: 5, capability_roles: ['platform-admin'], also_elsewhere: 213 },
    { station: 'q.platform-admin.sign-off', kind: 'constraint', depth: 6, wip_limit: null, over_limit: false, oldest_age_days: 2, capability_roles: ['platform-admin'], also_elsewhere: 0 },
    { station: 'loading-dock', kind: 'batch', depth: 1, wip_limit: 24, over_limit: false, oldest_age_days: null, capability_roles: null, also_elsewhere: 0 },
    { station: 'q.platform-admin.checklist', kind: 'constraint', depth: 0, wip_limit: null, over_limit: false, oldest_age_days: null, capability_roles: ['platform-admin'], also_elsewhere: 0 },
  ],
  total: 5,
  distinct_packets: 304,
};

const TASK_BLIND = 'a job-level metadata_absent clause the flow cube cannot evaluate';
const AGENT_BLIND = 'the step predicate matches on agent_model';
const DOCK_BLIND = 'a Job-metadata clause the log cannot attribute';

/// The flow read for `hours`, echoing it; sign-off served `served`.
const flow = (hours: number, served: number, asOf: string) => ({
  window_hours: hours,
  as_of: asOf,
  data: [
    { station: 'q.platform-admin.task', basis: 'unavailable', arrived: null, served: null, net: null, unavailable_reason: TASK_BLIND },
    { station: 'a.platform-admin.opus-5-1m', basis: 'unavailable', arrived: null, served: null, net: null, unavailable_reason: AGENT_BLIND },
    { station: 'q.platform-admin.sign-off', basis: 'step-events', arrived: 6, served, net: 6 - served },
    { station: 'loading-dock', basis: 'unavailable', arrived: null, served: null, net: null, unavailable_reason: DOCK_BLIND },
    { station: 'q.platform-admin.checklist', basis: 'step-events', arrived: 0, served: 2, net: -2 },
  ],
});

const AS_OF_24 = '2026-09-25T05:00:00Z';

const WAIT_ACTIVE = '11111111-aaaa-4aaa-8aaa-000000000001';
const WAIT_READY = '11111111-aaaa-4aaa-8aaa-000000000002';
const WAIT_FLOOR = '11111111-aaaa-4aaa-8aaa-000000000003';

/// Three real obligations — one CLAIMED (status active, gap 7), one on
/// nobody, one whose stamp is a fallback — and one simulated.
const WAITS = {
  data: [
    { job_id: WAIT_ACTIVE, job_kind: 'backlog-item', job_title: 'Teach the dock to breathe', step_title: 'Build the change', status: 'active', assignee_id: 'agent-claude', partition: 'real', simulated: false, exact: true, waiting_days: 5.5 },
    { job_id: WAIT_READY, job_kind: 'page-audit', job_title: 'Page audit: /it/estate', step_title: 'Sign off the page', status: 'ready', assignee_id: null, partition: 'real', simulated: false, exact: true, waiting_days: 0.25 },
    { job_id: WAIT_FLOOR, job_kind: 'design-doc', job_title: 'Recent performance', step_title: 'Review the design', status: 'ready', assignee_id: 'emp-david', partition: 'real', simulated: false, exact: false, waiting_days: 2 },
    { job_id: 'sim-1', job_kind: 'brew', job_title: 'a simulated batch', step_title: 'Mash', status: 'ready', assignee_id: null, partition: 'simulated', simulated: true, exact: true, waiting_days: 400 },
  ],
  total: 4,
  now: '2026-09-25T05:00:00Z',
};

type Counts = { load: number; waits: number; flow: Record<number, number> };

/// The page's world, well-formed. Each read can be overridden; the
/// counts say how many times the page asked.
async function install(
  page: Page,
  over: Partial<Record<'regions' | 'borders' | 'load' | 'flow' | 'waits', (r: Route) => Promise<void>>> = {},
): Promise<Counts> {
  await installSmokeMocks(page);
  const counts: Counts = { load: 0, waits: 0, flow: {} };
  await page.route(YARD_REGIONS, over.regions ?? ((r) => json(r, REGIONS)));
  await page.route(YARD_BORDERS, over.borders ?? ((r) => json(r, BORDERS)));
  await page.route(LOAD_READ, (r) => {
    counts.load += 1;
    return over.load ? over.load(r) : json(r, LOAD);
  });
  await page.route(FLOW_READ, (r) => {
    const hours = Number(new URL(r.request().url()).searchParams.get('window_hours'));
    counts.flow[hours] = (counts.flow[hours] ?? 0) + 1;
    return over.flow ? over.flow(r) : json(r, flow(hours, hours === 24 ? 5 : 30, `${AS_OF_24} (${hours}h)`));
  });
  await page.route(WAITS_READ, (r) => {
    counts.waits += 1;
    return over.waits ? over.waits(r) : json(r, WAITS);
  });
  return counts;
}

const board = (page: Page) => page.locator('.my-root');
const regionMap = (page: Page) => page.locator('section[aria-label="the marshalling region map"]');
const WORLD_SVG = 'section[aria-label="the IT world map"] svg';
const sidingRows = (page: Page) => board(page).locator('table.my-table').first().locator('tbody tr');
const waitsTable = (page: Page) => board(page).locator('table.my-table').nth(1);
const failures = (page: Page) => page.locator(FAILURE_MARKER);
/// The SVG's failure sentence, its wrapped lines joined back up.
const svgWhy = (page: Page) =>
  regionMap(page).locator('svg text.why').evaluate((t) =>
    Array.from(t.querySelectorAll('tspan')).map((s) => s.textContent ?? '').join(' '));

// ---------------------------------------------------------------------
// The route, the chrome and the words
// ---------------------------------------------------------------------

test.describe('/it/operate/marshalling — the alias, and the words it says', () => {
  test('the alias IS the Marshalling region floor, headed by the region under its way back', async ({ page }) => {
    expect(parseRoute(PATH)).toEqual(parseRoute(FLOOR));
    expect(parseRoute(PATH)).toEqual({ kind: 'systemYardFloor', region: 'marshalling' });
    await install(page);
    await mountPage(page, PATH, TITLE);

    await expect(page).toHaveURL(new RegExp(`${PATH}$`));
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('IT · Marshalling');
    await expect(page.getByRole('heading', { name: 'Department Map' })).toHaveCount(0);
    // The board's own page header is dropped: the region heads the page.
    await expect(page.getByRole('heading', { name: 'Marshalling Yard' })).toHaveCount(0);
    const crumbs = page.locator('nav.crumbs[data-region="marshalling"]');
    await expect(crumbs).toHaveText('Department Map › Marshalling');
    await expect(crumbs.locator('[aria-current="page"]')).toHaveText('› Marshalling');

    // The region's head, off the regions read.
    const map = regionMap(page);
    await expect(map.locator('button.leave')).toHaveText('← the world');
    await expect(map.locator('button.leave')).toHaveAttribute('aria-label', 'back to the IT world');
    await expect(map.locator('.region-name')).toHaveText('marshalling');
    await expect(map.locator('.region-count')).toHaveText('303 packets at stations');
    await expect(map.locator('.region-state')).toHaveText('attention');
    await expect(map.locator('.region-why')).toHaveText('303 packets standing at 3 stations');
    // Five platforms stand 517 against a head of 303, and the map says why.
    await expect(page.locator('[data-drawn="marshalling"]')).toHaveText(
      'the platforms stand 517 — the head counts 303 packets at stations: a packet stands at every station it matches, including one still in receiving or held by another region, and the head counts each once, only while it is marshalling’s',
    );
    // The world line under the map.
    await expect(page.locator('.yard-flow:not(.region-rails)')).toContainText('window 24h against the 24h before · read ');
  });

  test("the region's own rails, in then out — gap 5 (c4c77ddd) is FIXED on main", async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    const rails = page.locator('.region-rails[data-rails="marshalling"] .rail-line');
    await expect(rails).toHaveText([
      'in from receiving · 5/day · 12 waiting · nothing · worked by actors',
      'out to shop-floor · 215/day · 303 waiting · dispatch-agent-runs · fired 7m ago · attention — 303 waiting against 68 a day',
    ]);
  });

  test('the HUD frame stands above the region with stuck and waiting side by side — gap 6 (4142d821) is FIXED', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    const hud = page.locator('section[data-hud][aria-label="The whole system"]');
    await expect(hud.locator('.hud-title')).toHaveText('The whole system');
    const row = hud.locator('.hud-row[data-third="queue-management"]');
    await expect(row.locator('.hud-label')).toHaveText('Queue management');
    await expect(row.locator('[data-cell="stuck"]')).toHaveText(/stuck\s*2\s*oldest 3d\s*·\s*waiting\s*4/);
  });

  test('the board reads its sections, headers, rows and footnote verbatim', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    const b = board(page);

    await expect(b.locator('.my-controls-label')).toHaveText('window');
    await expect(b.locator('.my-controls button')).toHaveText(['24 h', '3 d', '7 d']);
    await expect(b.locator('.my-asof')).toHaveText(`read ${AS_OF_24} (24h)`);
    await expect(b.locator('.my-section')).toHaveText([
      '00 — THE CONSTRAINT',
      '01 — SIDINGS HOLDING WORK',
      '02 — LONGEST-WAITING OBLIGATIONS',
      '03 — RATE NOT COUNTABLE',
      '04 — CLEAR',
    ]);

    await expect(b.locator('table.my-table').first().locator('thead th')).toHaveText([
      'Station', 'Depth', 'Oldest', 'Arrived', 'Served', 'Net', 'Clears in', 'Why it is not moving',
    ]);
    // Pressure order: the deepest blind queues first, the dock's last.
    const rows = sidingRows(page);
    await expect(rows).toHaveCount(4);
    await expect(rows.nth(0).locator('td')).toHaveText([
      'q.platform-admin.task', '297 / 24', '6 d', '—', '—', '—', '—',
      `297 waiting; rate not counted — ${TASK_BLIND}.`,
    ]);
    await expect(rows.nth(1).locator('td')).toHaveText([
      'a.platform-admin.opus-5-1m', '213', '5 d', '—', '—', '—', '—',
      `213 waiting; rate not counted — ${AGENT_BLIND}.`,
    ]);
    await expect(rows.nth(2).locator('td')).toHaveText([
      'q.platform-admin.sign-off', '6', '2 d', '6', '5', '+1', '29 h',
      '6 arrived, 5 served: growing by 1 a 24h window — 29h to clear at that rate.',
    ]);
    await expect(rows.nth(2)).toHaveClass(/constraint/);

    await expect(b.locator('.my-overlap')).toHaveText(
      'The depths sum to 517 but hold 304 distinct packets: a packet stands at every station whose predicate it matches, so 213 of those standings count a packet already counted at another station. Also standing elsewhere: q.platform-admin.task 213 of 297, a.platform-admin.opus-5-1m 213 of 213.',
    );

    await expect(waitsTable(page).locator('thead th')).toHaveText(['Waiting', 'Obligation', 'Packet', 'On whom']);
    const waits = waitsTable(page).locator('tbody tr');
    await expect(waits).toHaveCount(3);
    await expect(waits.nth(0).locator('td')).toHaveText(['5 d', 'Build the change', 'Teach the dock to breathe backlog-item', 'agent-claude']);
    await expect(waits.nth(1).locator('td')).toHaveText(['at least 2 d', 'Review the design', 'Recent performance design-doc', 'emp-david']);
    await expect(waits.nth(2).locator('td')).toHaveText(['6 h', 'Sign off the page', 'Page audit: /it/estate page-audit', 'nobody']);
    await expect(waits.nth(2).locator('td').nth(3)).toHaveClass(/nobody/);
    await expect(b.locator('.my-waits-count')).toHaveText(
      '3 of 3 outstanding obligations, longest first · 1 on simulated or shadow packets not ranked',
    );

    await expect(b.locator('.my-blind li')).toHaveText([
      `q.platform-admin.task — ${TASK_BLIND}`,
      `a.platform-admin.opus-5-1m — ${AGENT_BLIND}`,
      `loading-dock — ${DOCK_BLIND}`,
    ]);
    await expect(b.locator('.my-clear')).toHaveText('q.platform-admin.checklist');
    await expect(b.locator('.my-footnote')).toHaveText(
      'Depth and the oldest packet\'s age come from /api/stations/load; arrivals and departures from /api/stations/flow, counted over the window from the log\'s own step.ready / step.done transitions on their WALL-CLOCK write instant — never event time, which is sim-authoritative here. "Oldest" is the oldest member packet\'s age since it opened, which over-reports time in this queue; the per-obligation wait below it is the step-level figure. Nothing on this page is estimated: a queue whose rate could not be counted says so.',
    );
    await expect(failures(page)).toHaveCount(0);
  });

  test('CURRENT, gap 1 (ba1fcc6d): the constraint is named from the countable queues only — 6 standing — while 510 stand where no rate is counted', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    const box = board(page).locator('.my-constraint');
    await expect(box).toHaveClass(/named/);
    await expect(box.locator('.my-constraint-name')).toHaveText('q.platform-admin.sign-off');
    await expect(box.locator('.my-constraint-why')).toHaveText(
      '6 waiting against 5 served in the last 24h: 29h to clear at the rate it is actually being worked, longer than any other counted queue.',
    );
    // Nothing in the box names the blind queues holding the work.
    await expect(box).not.toContainText('q.platform-admin.task');
  });

  test('CURRENT, gap 4 (f49f21b7): the dock\'s own station stands on the marshalling floor, on the board and as a platform', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    await expect(sidingRows(page).nth(3).locator('td').first()).toHaveText('loading-dock');
    const interior = regionMap(page).locator('svg .interior[data-interior="marshalling"]');
    await expect(interior.locator('.platform')).toHaveCount(5);
    await expect(interior.locator('.platform[data-platform="loading-dock"]')).toHaveCount(1);
  });

  test('CURRENT, gap 7 (f9b75688): a claimed obligation reads exactly like a ready one — no WORKING split', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    // The first row's step is ACTIVE (claimed by agent-claude) and the
    // third's READY; the table has no column and no word for either.
    await expect(waitsTable(page).locator('thead th')).toHaveCount(4);
    await expect(board(page)).not.toContainText(/\bactive\b|\bclaimed\b|\bworking\b/i);
  });

  test('CURRENT, gap 9 (fdc0ea0b): no station opens its queue — the only links on the board are the packets', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    const links = board(page).locator('a');
    await expect(links).toHaveCount(3);
    const hrefs = await links.evaluateAll((as) => as.map((a) => a.getAttribute('href')));
    expect(hrefs).toEqual([`/jobs/${WAIT_ACTIVE}`, `/jobs/${WAIT_FLOOR}`, `/jobs/${WAIT_READY}`]);
    await expect(board(page).locator('.my-constraint a, .my-blind a, .my-clear a')).toHaveCount(0);
    await expect(regionMap(page).locator('svg a')).toHaveCount(0);
  });
});

// ---------------------------------------------------------------------
// The controls — each does what its label says, and back returns
// ---------------------------------------------------------------------

test.describe('/it/operate/marshalling — every control', () => {
  test('the breadcrumb lands on the catalogued Department Map, and back returns to the alias', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    const crumb = page.locator('nav.crumbs').getByRole('link', { name: 'Department Map' });
    await expect(crumb).toHaveAttribute('href', ROUTE_CATALOG['system-yard'].path);
    expect(parseRoute(ROUTE_CATALOG['system-yard'].path)).toEqual({ kind: 'systemYard' });

    await crumb.click();
    await expect(page).toHaveURL(/\/it$/);
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('Department Map');
    await page.goBack();
    await expect(page).toHaveURL(new RegExp(`${PATH}$`));
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('IT · Marshalling');
  });

  test('"← the world" goes back to the world, and back returns to the alias', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    await regionMap(page).locator('button.leave').click();
    await expect(page).toHaveURL(/\/it$/);
    await expect(page.locator(WORLD_SVG)).toHaveCount(1);
    await expect(regionMap(page)).toHaveCount(0);
    await page.goBack();
    await expect(page).toHaveURL(new RegExp(`${PATH}$`));
    await expect(regionMap(page)).toHaveCount(1);
  });

  test('Escape leaves for the world too', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH, TITLE);
    await expect(regionMap(page)).toHaveCount(1);
    await page.keyboard.press('Escape');
    await expect(page).toHaveURL(/\/it$/);
    await expect(page.locator(WORLD_SVG)).toHaveCount(1);
  });

  test('a packet link lands on the catalogued job detail surface, and back returns to the alias', async ({ page }) => {
    await install(page);
    // The packet the link opens, so the detail page has something to
    // mount rather than throwing on the catch-all's `[]`.
    await page.route(new RegExp(`/api/jobs/${WAIT_ACTIVE}$`), (r) =>
      json(r, {
        id: WAIT_ACTIVE, kind: 'backlog-item', workflow_version: 1,
        subject: { subject_kind: 'custom', id: 'bosspipeline' },
        title: 'Teach the dock to breathe', owner_id: 'emp-david', status: 'open', priority: 'standard',
        opened_on: '2026-09-20', due_on: null, closed_on: null, metadata: {}, steps: [],
      }));
    await mountPage(page, PATH, TITLE);
    const link = waitsTable(page).getByRole('link', { name: 'Teach the dock to breathe' });
    // The link spells the bare /jobs/{id}; the router resolves it to the
    // same surface as the catalog's /ux/jobs/{id}.
    await expect(link).toHaveAttribute('href', `/jobs/${WAIT_ACTIVE}`);
    expect(parseRoute(`/jobs/${WAIT_ACTIVE}`)).toEqual({ kind: 'jobDetail', jobId: WAIT_ACTIVE });
    expect(parseRoute(`${ROUTE_CATALOG.jobs.path}/${WAIT_ACTIVE}`)).toEqual({ kind: 'jobDetail', jobId: WAIT_ACTIVE });

    await link.click();
    await expect(page).toHaveURL(new RegExp(`/jobs/${WAIT_ACTIVE}$`));
    await expect(regionMap(page)).toHaveCount(0);
    await expect(page.locator('h1').first()).toContainText('Teach the dock to breathe');
    await page.goBack();
    await expect(page).toHaveURL(new RegExp(`${PATH}$`));
    await expect(waitsTable(page).getByRole('link', { name: 'Teach the dock to breathe' })).toBeVisible();
  });

  test("the HUD's machine link selects the machine's region on the Department Map", async ({ page }) => {
    // A selection since car N1 of design e765b3fc: the map comes back on
    // top with marshalling's panel under it, where the link used to open
    // this floor again.
    await install(page);
    await mountPage(page, PATH, TITLE);
    const link = page.locator('section[data-hud] .hud-listed a');
    await expect(link).toHaveCount(1);
    await expect(link).toHaveText('failed · marshalling · sign-off station');
    await expect(link).toHaveAttribute('href', regionHref('marshalling'));
    await expect(link).toHaveAttribute('title', 'over its bound');
    await link.click();
    await expect(page).toHaveURL(/\/it\?at=marshalling$/);
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('Department Map');
    await expect(page.locator('section[data-map-panel]')).toHaveAttribute('data-selection', 'marshalling');
    await page.goBack();
    await expect(page).toHaveURL(new RegExp(`${PATH}$`));
  });

  test('each window button re-reads the board over its window, says it is pressed, and judges in its hours', async ({ page }) => {
    const counts = await install(page);
    await mountPage(page, PATH, TITLE);
    const buttons = board(page).locator('.my-controls button');
    const pressed = board(page).locator('.my-controls button[aria-pressed="true"]');
    await expect(buttons.nth(0)).toHaveAttribute('aria-pressed', 'true');
    await expect(buttons.nth(1)).toHaveAttribute('aria-pressed', 'false');
    await expect(buttons.nth(2)).toHaveAttribute('aria-pressed', 'false');
    await expect.poll(() => counts.flow[24]).toBe(1);

    for (const [i, hours, label] of [[1, 72, '3 d'], [2, 168, '7 d'], [0, 24, '24 h']] as const) {
      const before = { load: counts.load, waits: counts.waits, flow: counts.flow[hours] ?? 0 };
      await buttons.nth(i).click();
      await expect(buttons.nth(i)).toHaveText(label);
      await expect(buttons.nth(i)).toHaveAttribute('aria-pressed', 'true');
      await expect(pressed).toHaveText([label]);
      // The flow read over the window — and, CURRENT (measure's note),
      // the load and the waits again although neither takes a window.
      await expect.poll(() => counts.flow[hours] ?? 0).toBe(before.flow + 1);
      await expect.poll(() => counts.load).toBe(before.load + 1);
      await expect.poll(() => counts.waits).toBe(before.waits + 1);
      const served = hours === 24 ? 5 : 30;
      await expect(board(page).locator('.my-asof')).toHaveText(`read ${AS_OF_24} (${hours}h)`);
      await expect(board(page).locator('.my-constraint-why')).toContainText(
        `6 waiting against ${served} served in the last ${hours}h`,
      );
    }
  });

  test('CURRENT, gap 12 (371aa184): the envelope\'s own window_hours is parsed and ignored — a clamped answer is judged in the pressed hours', async ({ page }) => {
    await install(page, {
      flow: (r) => {
        const hours = Number(new URL(r.request().url()).searchParams.get('window_hours'));
        // The server clamps 168 to 72 and says so on its envelope.
        return json(r, flow(hours === 168 ? 72 : hours, 30, 'clamped'));
      },
    });
    await mountPage(page, PATH, TITLE);
    await board(page).locator('.my-controls button', { hasText: '7 d' }).click();
    await expect(board(page).locator('.my-asof')).toHaveText('read clamped');
    await expect(board(page).locator('.my-constraint-why')).toHaveText(
      '6 waiting against 30 served in the last 168h: 34h to clear at the rate it is actually being worked, longer than any other counted queue.',
    );
  });

  test('CURRENT, gap 12 (371aa184): window reads are not sequenced — a slow 24 h answer lands under a pressed 7 d', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => (release = resolve));
    await install(page, {
      flow: async (r) => {
        const hours = Number(new URL(r.request().url()).searchParams.get('window_hours'));
        if (hours === 24) await held;
        return json(r, flow(hours, hours === 24 ? 5 : 30, `${AS_OF_24} (${hours}h)`));
      },
    });
    await mountPage(page, PATH, TITLE);
    await expect(board(page).locator('.my-quiet')).toHaveText('Reading the network…');
    await board(page).locator('.my-controls button', { hasText: '7 d' }).click();
    await expect(board(page).locator('.my-asof')).toHaveText(`read ${AS_OF_24} (168h)`);
    release();
    // The 24 h answer, which the 7 d click superseded, lands last and wins.
    await expect(board(page).locator('.my-asof')).toHaveText(`read ${AS_OF_24} (24h)`);
    await expect(board(page).locator('.my-controls button', { hasText: '7 d' })).toHaveAttribute('aria-pressed', 'true');
    await expect(board(page).locator('.my-constraint-why')).toHaveText(
      '6 waiting against 5 served in the last 168h: 202h to clear at the rate it is actually being worked, longer than any other counted queue.',
    );
  });

  test('the page writes nothing: every control used, no non-GET call leaves it', async ({ page }) => {
    const writes: string[] = [];
    page.on('request', (req) => {
      if (req.method() !== 'GET' && req.url().includes('/api/') && !req.url().includes('/api/surface-opens')) {
        writes.push(`${req.method()} ${req.url()}`);
      }
    });
    const counts = await install(page);
    await mountPage(page, PATH, TITLE);
    for (const label of ['3 d', '7 d', '24 h']) {
      await board(page).locator('.my-controls button', { hasText: label }).click();
    }
    await expect.poll(() => counts.load).toBe(4);
    await regionMap(page).locator('button.leave').click();
    await expect(page).toHaveURL(/\/it$/);
    expect(writes).toEqual([]);
  });
});

// ---------------------------------------------------------------------
// Empty, failed and malformed — never the same paint
// ---------------------------------------------------------------------

/// The well-formed empty network: stations answered, nothing stands.
const EMPTY = {
  load: (r: Route) => json(r, { data: [], total: 0, distinct_packets: 0 }),
  flow: (r: Route) => json(r, { window_hours: 24, as_of: AS_OF_24, data: [] }),
  waits: (r: Route) => json(r, { data: [], total: 0, now: AS_OF_24 }),
};

async function expectClearPaint(page: Page): Promise<void> {
  const b = board(page);
  await expect(b.locator('.my-constraint-name')).toHaveText('No queue can be named');
  await expect(b.locator('.my-constraint')).not.toHaveClass(/named/);
  await expect(b.locator('.my-constraint-why')).toHaveText('Nothing is waiting at any watched station.');
  await expect(b.locator('.my-quiet')).toHaveText(['Every watched station is clear.', 'Nothing is outstanding.']);
  await expect(b.locator('table.my-table')).toHaveCount(0);
  await expect(regionMap(page).locator('svg')).toContainText('no queue is declared here');
  await expect(failures(page)).toHaveCount(0);
}

test.describe('/it/operate/marshalling — the empty leg and every failure line', () => {
  test('an EMPTY network paints as clear, with no failure marker', async ({ page }) => {
    await install(page, EMPTY);
    await mountPage(page, PATH, TITLE);
    await expectClearPaint(page);
  });

  test('CURRENT, gap 11 (8dcd28ce): a denied scope answers the empty envelope, so it paints exactly as clear', async ({ page }) => {
    // stations_load's answer to a caller whose scope predicate is None.
    await install(page, { ...EMPTY, load: (r) => json(r, { data: [], total: 0 }) });
    await mountPage(page, PATH, TITLE);
    await expectClearPaint(page);
  });

  // Gap 10, FIXED by 67825067: each board read's parse goes through the
  // shared envelope reader (src/data/shape.ts), so a 200 that is not
  // the envelope is a failed read — the same failure line as a 500, with
  // the read named and what came back said.
  const NOT_THE_SHAPE = (path: string) =>
    `${path}: HTTP 200, but the body is an object with no data list, not a {data: [...]} envelope`;

  test('gap 10 (67825067): a malformed 200 from all three board reads is a failure line, never a clear yard', async ({ page }) => {
    await install(page, {
      load: (r) => json(r, { error: 'not the shape' }),
      flow: (r) => json(r, { error: 'not the shape' }),
      waits: (r) => json(r, { error: 'not the shape' }),
    });
    await mountPage(page, PATH, TITLE);
    await expect(board(page).locator('.my-fail.load-failed')).toHaveText(
      `The station load did not answer: ${NOT_THE_SHAPE('/api/stations/load')}. An unreachable read is not an empty yard, so this page shows nothing rather than a clear one.`,
    );
    await expect(board(page)).not.toContainText('Every watched station is clear.');
    await expect(board(page).locator('.my-quiet')).toHaveCount(0);
    await expect(board(page).locator('.my-section')).toHaveCount(0);
    expect(await svgWhy(page)).toContain('the queues cannot be read — /api/stations/load: HTTP 200, but the body is');
  });

  test('gap 10 (67825067): a malformed 200 from the station flow alone is its failure line, and no constraint is named', async ({ page }) => {
    await install(page, { flow: (r) => json(r, []) });
    await mountPage(page, PATH, TITLE);
    await expect(board(page).locator('.my-fail.load-failed')).toHaveText(
      'The station flow did not answer: /api/stations/flow: HTTP 200, but the body is a list, not a {data: [...]} envelope. Depth without a rate cannot say whether anything is forming, so the constraint is not named.',
    );
    await expect(board(page).locator('.my-constraint')).toHaveCount(0);
    await expect(board(page).locator('.my-quiet')).toHaveCount(0);
  });

  test('gap 10 (67825067): a malformed 200 from the queue-age lens alone is its failure line, never "Nothing is outstanding."', async ({ page }) => {
    await install(page, { waits: (r) => json(r, { error: 'not the shape' }) });
    await mountPage(page, PATH, TITLE);
    await expect(board(page).locator('.my-fail.load-failed')).toHaveText(
      `The queue-age lens did not answer: ${NOT_THE_SHAPE('/api/jobs/queue-age')}.`,
    );
    await expect(board(page)).not.toContainText('Nothing is outstanding.');
    await expect(sidingRows(page)).toHaveCount(4);
  });

  test('a failed regions read is said, and the HUD says when; the board still reads its own', async ({ page }) => {
    await install(page, { regions: (r) => json(r, 'down', 500) });
    await mountPage(page, PATH, { titleMatch: /IT · Marshalling/ });
    await expect(page.locator('.yard-empty.load-failed')).toHaveText('The regions cannot be read — /api/yard/regions: HTTP 500');
    await expect(page.locator('section[data-hud] .hud-age')).toHaveText(/^read failed \d\d:\d\dZ · no good read yet$/);
    await expect(regionMap(page)).toHaveCount(0);
    await expect(board(page).locator('.my-constraint-name')).toHaveText('q.platform-admin.sign-off');
  });

  test('a failed borders read is said, in place of the rails', async ({ page }) => {
    await install(page, { borders: (r) => json(r, 'down', 500) });
    await mountPage(page, PATH, TITLE);
    await expect(page.locator('.yard-empty.load-failed')).toHaveText('The borders cannot be read — /api/yard/borders: HTTP 500');
    await expect(page.locator('.region-rails')).toHaveCount(0);
  });

  test('a failed station load is said on the board AND on the map, never as a clear yard', async ({ page }) => {
    await install(page, { load: (r) => json(r, 'down', 500) });
    await mountPage(page, PATH, TITLE);
    await expect(board(page).locator('.my-fail.load-failed')).toHaveText(
      'The station load did not answer: /api/stations/load: HTTP 500. An unreachable read is not an empty yard, so this page shows nothing rather than a clear one.',
    );
    expect(await svgWhy(page)).toBe('the queues cannot be read — /api/stations/load: HTTP 500');
    // Neither empty state, and no section at all under a failed load.
    await expect(board(page).locator('.my-quiet')).toHaveCount(0);
    await expect(board(page).locator('.my-section')).toHaveCount(0);
  });

  test('a failed station flow is said on the board AND on the map, and no constraint is named', async ({ page }) => {
    await install(page, { flow: (r) => json(r, 'down', 500) });
    await mountPage(page, PATH, TITLE);
    await expect(board(page).locator('.my-fail.load-failed')).toHaveText(
      'The station flow did not answer: /api/stations/flow?window_hours=24: HTTP 500. Depth without a rate cannot say whether anything is forming, so the constraint is not named.',
    );
    expect(await svgWhy(page)).toBe('the queues cannot be read — /api/stations/flow?window_hours=24: HTTP 500');
    await expect(board(page).locator('.my-constraint')).toHaveCount(0);
    await expect(board(page).locator('.my-quiet')).toHaveCount(0);
  });

  test('a failed queue-age read is said under the sidings, never as "Nothing is outstanding."', async ({ page }) => {
    await install(page, { waits: (r) => json(r, 'down', 500) });
    await mountPage(page, PATH, TITLE);
    await expect(board(page).locator('.my-fail.load-failed')).toHaveText(
      'The queue-age lens did not answer: /api/jobs/queue-age: HTTP 500.',
    );
    await expect(board(page)).not.toContainText('Nothing is outstanding.');
    await expect(sidingRows(page)).toHaveCount(4);
  });
});
