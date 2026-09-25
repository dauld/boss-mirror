// /it/registry/dispatcher — every control the page audit counted, pinned
// (page-audit 2ee7cdfc, step `test`).
//
// The audit's inventory (controls_md, measured 2026-09-23 against
// 642c0171): 7 links (6 registry tabs and "Edit rules →"), 5 fixed
// buttons plus one "×" per selected trigger, 1 select, one clickable
// node per graph node, 0 forms, 1 read (`GET /api/dispatcher/rules`),
// 0 writes. Since then the registry tabs grew a Rules tab (0a98d93f), so
// there are 8 in-app links, and this spec counts three controls the
// inventory did not: the library's own "Svelte Flow" attribution link
// (external, a new tab), keyboard selection of a node, and Backspace on
// it, which measured deletes nothing.
//
// THIS SPEC PINS THE PAGE AS IT IS ON origin/main, INCLUDING OPEN GAPS,
// and names the item at each one, so the car that fixes a gap flips the
// assertion named for it:
//   gap 1 (cf13bd12, closed) jobs.job.closed and step.assigned.* had no
//                    inbound edge — FIXED server-side: the handler crate's
//                    system_edges carry both, and the loop through packet
//                    closure is drawn and lit red here;
//   gap 2 (ec40e269) 21 handlers no rule invokes were drawn and counted —
//                    FIXED on the page: only what a live rule invokes is
//                    drawn and counted (the item stays open for its
//                    server half);
//   gap 3 (4d9b09e0, closed) an empty registry painted a full picture —
//                    FIXED as a consequence of gap 2: an empty rule set
//                    with the server's real roster paints the empty line;
//   gap 4 (d3734028) the only filter cannot reach a scheduled rule, and a
//                    filtered view drops it;
//   gap 5 (68162348) the rule panel drops why/source/authored/version and
//                    links nowhere;
//   gap 6 (f0216f5e) the legend keys 3 of the 5 edge kinds;
//   covered A (cae1a377 / d7732e88) the failure line is marked — FIXED;
//                    the 503 and 200-with-error branches are pinned in
//                    dispatcher-failed-reads.mocked.spec.ts, and the two
//                    throw branches here.
// Two findings no item owns, marked UNFILED where they are asserted:
//   nodes paint 150px wide, not the 240×54 the layout reserves, so
//   neighbouring nodes overlap;
//   the stats say "1 rules" and "1 handlers" in the singular.
//
// THE FIXTURE. The payload is the server's shape: rules, the handler
// roster as the dispatcher build declares it (boss-dispatcher-handlers
// cascade.rs — the four handlers here spelled as it spells them, plus two
// no rule invokes), and its eight system edges verbatim. Three rules are
// read from infra/dispatcher/rules/ (names, triggers, guards and handlers
// as the files spell them; args trimmed); `notify-on-task-done` is
// invented, so one rule listens on a concrete step.done topic and the
// wildcard `match` edge is drawn. What the page builds from it, by hand
// from cascadeToGraph.ts:
//   14 nodes: 7 events, 4 rules, 3 handlers;
//   15 edges: 3 trigger, 4 do, 2 emit, 5 system, 1 match (the job.updated
//             and two invoice system edges start from undrawn events);
//   1 cycle:  jobs.job.closed → resolve-subjob → jobs.subjob_resolve →
//             jobs.step.completed → (jobs-api) jobs.job.closed — 4 nodes,
//             4 edges;
//   8 ranks wide, which is why the viewport below is wide: at the
//             suite's 1 280 px, fitView stops at minZoom and the outer
//             ranks sit outside the flow, where no click can reach.

import { expect, test, type Locator, type Page, type Request, type Route } from '@playwright/test';
import { mountPage, settledReads } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { parseRoute } from '../../src/router';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { sectionForRoute } from '../../src/shell/sections';

test.use({ viewport: { width: 2400, height: 1000 } });

const PAGE = '/it/registry/dispatcher';
const TITLE = 'Dispatcher rules — reactive cascade';
const RULES_PAGE = '/it/registry/rules';
const SUBTITLE =
  'The side-effect wiring the boss-dispatcher runs: a step completes or an event fires → a rule matches → handlers run → they emit events that re-trigger more rules. Red = a feedback cycle.';

/// The page's one read — anchored, so an editor's
/// `/api/dispatcher/rules/<name>/versions` is not it.
const RULES = /\/api\/dispatcher\/rules$/;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const WHY = 'a why the panel does not render';

const RESOLVE = {
  name: 'resolve-subjob-on-child-job-closed',
  on_event: 'jobs.job.closed',
  when: 'parent_step_id != null',
  do: [{ handler: 'jobs.subjob_resolve', args: {} }],
  version: 1,
  status: 'active',
  why: `A CROSS-PROTOCOL REACTOR — ${WHY}.`,
  authored: true,
  source: 'product',
};
const ASSIGNED = {
  name: 'notify-assignee-on-step-assigned',
  on_event: 'step.assigned.*',
  when: null,
  do: [{ handler: 'messages.notify', args: {} }],
  version: 1,
  status: 'active',
  why: `The other order of the same cross-domain fact — ${WHY}.`,
  authored: true,
  source: 'product',
};
const TASK_DONE = {
  name: 'notify-on-task-done',
  on_event: 'step.done.task',
  when: 'notify_on_done = true',
  do: [{ handler: 'messages.notify', args: { id_prefix: '"done"' } }],
  version: 4,
  status: 'active',
  why: null,
  authored: false,
  source: 'tenant:algedonic',
};
const PUBLISH = {
  name: 'publish-to-github-daily',
  schedule: { cadence: 'daily', anchor_date: '2026-08-15' },
  when: 'NOT open_publish_exists("github-mirror")',
  do: [{ handler: 'jobs.spawn', args: { kind: '"publish-to-github"', subject: '"github-mirror"' } }],
  version: 2,
  status: 'active',
  why: `A TIMER, the standing exemption — ${WHY}.`,
  authored: true,
  source: 'product',
};
const ROWS = [RESOLVE, ASSIGNED, TASK_DONE, PUBLISH];

/// The dispatcher build's roster: every handler it registers, whether or
/// not a rule invokes it. The last two are invoked by none of ROWS.
const HANDLER_EMITS = {
  'jobs.subjob_resolve': ['jobs.step.completed'],
  'messages.notify': [],
  'jobs.spawn': ['jobs.job.created'],
  'people.hire': ['people.employee.created'],
  'commerce.invoice.issue': ['commerce.invoice.created'],
};

/// boss-dispatcher-handlers cascade.rs `system_edges()`, verbatim.
const SYSTEM_EDGES = [
  { from: 'jobs.job.created', to: 'step.ready.*', kind: 'jobs-api', label: "a new Job's entry steps become ready" },
  { from: 'jobs.step.completed', to: 'step.done.*', kind: 'jobs-api', label: 'a completed step emits its done topic' },
  { from: 'jobs.step.completed', to: 'step.ready.*', kind: 'jobs-api', label: 'completing a step readies its dependents' },
  { from: 'jobs.job.updated', to: 'step.ready.*', kind: 'jobs-api', label: 'a metadata write wakes a metadata-gated step' },
  { from: 'jobs.step.completed', to: 'jobs.job.closed', kind: 'jobs-api', label: 'completing a terminal step closes its packet' },
  { from: 'step.ready.*', to: 'step.assigned.*', kind: 'jobs-api', label: 'the assignment loop places a ready step with an executor' },
  { from: 'commerce.invoice.created', to: 'commerce.invoice.paid', kind: 'external', label: 'the counterparty settles the invoice' },
  { from: 'commerce.invoice.created', to: 'commerce.invoice.past_due', kind: 'external', label: 'the invoice goes unpaid past terms' },
];

const payload = (rules: unknown[]) => ({
  rules,
  handler_emits: HANDLER_EMITS,
  system_edges: SYSTEM_EDGES,
  authored_registry: { dir: '/opt/boss/infra/dispatcher/rules', rules: 3, error: null },
});

const NODES = 14;
const EDGES = 15;

async function install(page: Page, rules: unknown[] = ROWS): Promise<void> {
  await installSmokeMocks(page);
  await page.route(RULES, (r) => json(r, payload(rules)));
}

/// The shell's own write: App.svelte posts one surface-open per
/// navigation. It is chrome, not a control of this page.
const SHELL_WRITE = /\/api\/surface-opens$/;

function watchWrites(page: Page): Request[] {
  const writes: Request[] = [];
  page.on('request', (r) => {
    const url = r.url();
    if (url.includes('/api/') && r.method() !== 'GET' && !SHELL_WRITE.test(new URL(url).pathname)) {
      writes.push(r);
    }
  });
  return writes;
}

const catalogPaths = (): Set<string> =>
  new Set(Object.values(ROUTE_CATALOG).map((r) => (r as { path: string }).path));

/// The section a path lights, when that section is a ROUTE_CATALOG key.
function catalogued(path: string): string | null {
  const section = sectionForRoute(parseRoute(path));
  return section in ROUTE_CATALOG ? section : null;
}

const node = (page: Page, id: string): Locator => page.locator(`.svelte-flow__node[data-id="${id}"]`);
const edges = (page: Page, prefix = ''): Locator => page.locator(`.svelte-flow__edge[data-id^="${prefix}"]`);
const panel = (page: Page): Locator => page.locator('aside.dx-panel');

async function mountGraph(page: Page): Promise<void> {
  await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });
  await expect(page.locator('.svelte-flow__node')).toHaveCount(NODES);
  await expect(page.locator('.svelte-flow__edge')).toHaveCount(EDGES);
}

/// The flow's zoom, read off its viewport transform.
async function zoom(page: Page): Promise<number> {
  const t = await page.locator('.svelte-flow__viewport').evaluate((el) => (el as HTMLElement).style.transform);
  const m = /scale\(([\d.]+)\)/.exec(t);
  return m ? Number(m[1]) : Number.NaN;
}

/// Click a node where a user can: the first point down its vertical
/// centre line that no other node covers. Nodes overlap on this page (the
/// UNFILED finding pinned in "the picture"), so a node's centre may be
/// under its neighbour, and a click there selects the neighbour.
async function clickNode(page: Page, id: string): Promise<void> {
  const target = node(page, id);
  await expect(target).toBeVisible();
  const point = await target.evaluate((el) => {
    const r = el.getBoundingClientRect();
    const x = r.left + r.width / 2;
    for (let y = r.top + 2; y < r.bottom - 1; y += 2) {
      if (document.elementFromPoint(x, y)?.closest('.svelte-flow__node') === el) return { x, y };
    }
    return null;
  });
  expect(point, `some point of ${id} is not covered by another node`).not.toBeNull();
  await page.mouse.click(point!.x, point!.y);
}

/// An empty corner of the flow — where `onpaneclick` fires.
async function clickPane(page: Page): Promise<void> {
  await page.locator('.svelte-flow__pane').click({ position: { x: 8, y: 8 } });
}

async function backHere(page: Page): Promise<void> {
  await page.goBack();
  await expect(page).toHaveURL((u) => u.pathname === PAGE);
  await expect(page.locator('h1').first()).toHaveText(TITLE);
  await expect(page.locator('.svelte-flow__node')).toHaveCount(NODES);
}

test.describe('/it/registry/dispatcher — the picture', () => {
  test('header, stats, legend, and the graph the payload builds', async ({ page }) => {
    await install(page);
    await mountGraph(page);

    await expect(page.locator('.dx-head h1')).toHaveText(TITLE);
    await expect(page.locator('.dx-head p.dx-sub')).toHaveText(SUBTITLE);
    // GAP 2 (backlog ec40e269), fixed on the page: the handler stat
    // counts the 3 handlers a rule invokes, not the roster's 5.
    await expect(page.locator('.dx-stats span')).toHaveText(['4 rules', '3 handlers', '4 in cycles']);

    await expect(page.locator('.dx-legend .dx-key')).toHaveText(['event', 'rule', 'handler']);
    // GAP 6 (backlog f0216f5e), pinned as it stands: three edge keys,
    // while the graph also draws trigger, do and match edges — 8 of the
    // 15 here have no key. The fixing car adds the three.
    await expect(page.locator('.dx-legend .dx-edgekey')).toHaveText([
      'emits',
      'system (jobs-api / external)',
      'feedback cycle',
    ]);
    await expect(edges(page, 't:')).toHaveCount(3);
    await expect(edges(page, 'd:')).toHaveCount(4);
    await expect(edges(page, 'm:')).toHaveCount(1);
    await expect(edges(page, 'e:')).toHaveCount(2);
    await expect(edges(page, 's:')).toHaveCount(5);

    await expect(page.locator('.svelte-flow__node.dx-event')).toHaveCount(7);
    await expect(page.locator('.svelte-flow__node.dx-rule')).toHaveCount(4);
    await expect(page.locator('.svelte-flow__node.dx-handler')).toHaveCount(3);

    // Each node's label and sublabel: the trigger in words and the guard
    // mark for a rule, the emit count or "sink" for a handler.
    await expect(node(page, `rule:${RESOLVE.name}`)).toHaveText(`${RESOLVE.name} on jobs.job.closed · when ⚲`);
    await expect(node(page, `rule:${ASSIGNED.name}`)).toHaveText(`${ASSIGNED.name} on step.assigned.*`);
    await expect(node(page, `rule:${PUBLISH.name}`)).toHaveText(`${PUBLISH.name} every day · from 2026-08-15 · when ⚲`);
    await expect(node(page, 'hdl:jobs.spawn')).toHaveText('jobs.spawn emits 1');
    await expect(node(page, 'hdl:messages.notify')).toHaveText('messages.notify sink');
    await expect(node(page, 'evt:step.done.*')).toHaveText('step.done.*');

    // GAP 2 (ec40e269), fixed: a roster handler no rule invokes is not
    // drawn, and neither is the event only it would emit.
    await expect(node(page, 'hdl:people.hire')).toHaveCount(0);
    await expect(node(page, 'hdl:commerce.invoice.issue')).toHaveCount(0);
    await expect(node(page, 'evt:people.employee.created')).toHaveCount(0);

    // GAP 1 (backlog cf13bd12), fixed server-side: packet closure is an
    // edge, so the loop through it is drawn — and it is the only red.
    await expect(page.getByText('completing a terminal step closes its packet')).toHaveCount(1);
    await expect(page.getByText('the assignment loop places a ready step with an executor')).toHaveCount(1);
    // A system edge whose source nothing draws is not drawn.
    await expect(page.getByText('a metadata write wakes a metadata-gated step')).toHaveCount(0);
    await expect(page.getByText('the counterparty settles the invoice')).toHaveCount(0);
    const cycle = page.locator('.svelte-flow__node.dx-cycle');
    await expect(cycle).toHaveCount(4);
    const lit = await cycle.evaluateAll((ns) => ns.map((n) => n.getAttribute('data-id')).sort());
    expect(lit).toEqual([
      'evt:jobs.job.closed',
      'evt:jobs.step.completed',
      'hdl:jobs.subjob_resolve',
      `rule:${RESOLVE.name}`,
    ]);
    await expect(page.locator('.svelte-flow__edge.animated')).toHaveCount(4);
  });

  test('UNFILED: nodes paint at the library’s 150px, not the 240×54 the layout reserves, so neighbours overlap', async ({ page }) => {
    // Not in the audit's inventory, and no item owns it. buildFlow lays
    // every node out as 240×54 (NODE_W, NODE_H) and the page's own CSS
    // asks for 240px, but @xyflow/svelte's `.svelte-flow__node-default`
    // rule (width 150px, padding 10px) wins in the bundle this suite
    // serves. A hyphenated rule name then wraps across several lines, the
    // node grows far past 54px, and dagre's 22px gaps are overrun: a
    // node's centre can sit under its neighbour, and a click there
    // selects the neighbour. The fixing car flips these to 240 and to no
    // overlapping pair.
    await install(page);
    await mountGraph(page);
    await expect.poll(() => zoom(page)).toBeLessThan(1);
    const z = await zoom(page);
    const rects = await page
      .locator('.svelte-flow__node')
      .evaluateAll((ns) => ns.map((n) => n.getBoundingClientRect().toJSON() as { x: number; y: number; width: number; height: number }));
    expect(rects).toHaveLength(NODES);
    expect(rects.map((r) => Math.round(r.width / z))).toEqual(Array(NODES).fill(150));
    expect(rects.every((r) => r.height / z > 54), 'every node is taller than the 54px dagre reserved').toBe(true);
    const overlapping = rects.flatMap((a, i) =>
      rects.slice(i + 1).filter((b) => a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height),
    );
    expect(overlapping.length, 'pairs of nodes that overlap').toBeGreaterThan(0);
  });
});

test.describe('/it/registry/dispatcher — the trigger filter', () => {
  test('options are the event topics; a chip narrows, × and show all widen', async ({ page }) => {
    const writes = watchWrites(page);
    await install(page);
    await mountGraph(page);

    const select = page.locator('select.dx-filter-select');
    const options = select.locator('option');
    await expect(page.locator('.dx-filter-label')).toContainText('Trigger');
    // GAP 4 (backlog d3734028), pinned as it stands: the options are the
    // three on_event topics; the scheduled publish-to-github-daily has
    // no option and no other way to be chosen.
    await expect(options).toHaveText([
      'filter cascade by trigger event…',
      'jobs.job.closed',
      'step.assigned.*',
      'step.done.task',
    ]);
    await expect(page.locator('.dx-chip')).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'show all' })).toHaveCount(0);
    await expect(page.locator('.dx-filter-note')).toHaveCount(0);

    await select.selectOption('step.assigned.*');
    await expect(page.locator('.dx-chip')).toHaveText(['step.assigned.* ×']);
    await expect(select).toHaveValue('');
    await expect(options).toHaveText(['filter cascade by trigger event…', 'jobs.job.closed', 'step.done.task']);
    await expect(page.locator('.dx-filter-note')).toHaveText(`cascade from 1 trigger · 3/${NODES} nodes`);
    await expect(page.locator('.svelte-flow__node')).toHaveCount(3);

    await select.selectOption('jobs.job.closed');
    await expect(page.locator('.dx-chip')).toHaveText(['step.assigned.* ×', 'jobs.job.closed ×']);
    await expect(page.locator('.dx-filter-note')).toHaveText(`cascade from 2 triggers · 11/${NODES} nodes`);
    await expect(page.locator('.svelte-flow__node')).toHaveCount(11);
    // GAP 4 again: the scheduled rule, and the chain only it starts,
    // leave every filtered view.
    await expect(node(page, `rule:${PUBLISH.name}`)).toHaveCount(0);
    await expect(node(page, 'hdl:jobs.spawn')).toHaveCount(0);
    // The stats describe the whole registry, not the filtered view.
    await expect(page.locator('.dx-stats span')).toHaveText(['4 rules', '3 handlers', '4 in cycles']);

    // "×" (title "remove") drops its own trigger only.
    const x = page.locator('.dx-chip').filter({ hasText: 'step.assigned.*' }).getByRole('button', { name: '×' });
    await expect(x).toHaveAttribute('title', 'remove');
    await x.click();
    await expect(page.locator('.dx-chip')).toHaveText(['jobs.job.closed ×']);
    await expect(page.locator('.dx-filter-note')).toHaveText(`cascade from 1 trigger · 11/${NODES} nodes`);

    await page.getByRole('button', { name: 'show all' }).click();
    await expect(page.locator('.dx-chip')).toHaveCount(0);
    await expect(page.locator('.dx-filter-note')).toHaveCount(0);
    await expect(page.locator('.svelte-flow__node')).toHaveCount(NODES);
    await expect(options).toHaveCount(4);
    expect(writes.map((w) => `${w.method()} ${w.url()}`)).toEqual([]);
  });
});

test.describe('/it/registry/dispatcher — the side panel', () => {
  test('a rule node opens its trigger, guard and do list; the pane closes it', async ({ page }) => {
    await install(page);
    await mountGraph(page);
    await expect(panel(page)).toHaveCount(0);

    await clickNode(page, `rule:${RESOLVE.name}`);
    await expect(panel(page).locator('h2')).toHaveText(`rule · ${RESOLVE.name}`);
    await expect(panel(page).locator('dt')).toHaveText(['on event', 'when', 'do']);
    await expect(panel(page).locator('dd').nth(0)).toHaveText('jobs.job.closed');
    await expect(panel(page).locator('dd').nth(1)).toHaveText('parent_step_id != null');
    await expect(panel(page).locator('dd ol > li')).toHaveText(['jobs.subjob_resolve']);
    await expect(node(page, `rule:${RESOLVE.name}`)).toHaveClass(/dx-selected/);

    // GAP 5 (backlog 68162348), pinned as it stands: the payload's why,
    // source, authored flag and version are not shown, and the panel has
    // no link to the rule's own page. The fixing car flips these.
    await expect(panel(page).getByText(WHY)).toHaveCount(0);
    await expect(panel(page).getByText('product')).toHaveCount(0);
    await expect(panel(page).getByText(/version|v1\b/)).toHaveCount(0);
    await expect(panel(page).locator('a')).toHaveCount(0);

    await clickPane(page);
    await expect(panel(page)).toHaveCount(0);

    // A scheduled rule says "on schedule" and its cadence; its args are
    // listed under the handler.
    await clickNode(page, `rule:${PUBLISH.name}`);
    await expect(panel(page).locator('h2')).toHaveText(`rule · ${PUBLISH.name}`);
    await expect(panel(page).locator('dt')).toHaveText(['on schedule', 'when', 'do']);
    await expect(panel(page).locator('dd').nth(0)).toHaveText('every day · from 2026-08-15');
    await expect(panel(page).locator('dd').nth(1)).toHaveText('NOT open_publish_exists("github-mirror")');
    await expect(panel(page).locator('ul.dx-args > li')).toHaveText([
      'kind = "publish-to-github"',
      'subject = "github-mirror"',
    ]);
    await expect(panel(page).locator('a')).toHaveCount(0);
    await clickPane(page);

    // A tenant rule paints like a product rule: the source is not shown.
    await clickNode(page, `rule:${TASK_DONE.name}`);
    await expect(panel(page).locator('h2')).toHaveText(`rule · ${TASK_DONE.name}`);
    await expect(panel(page).getByText('tenant:algedonic')).toHaveCount(0);
    await clickPane(page);
    await expect(panel(page)).toHaveCount(0);
  });

  test('a handler node lists what it emits, or says it is a sink', async ({ page }) => {
    await install(page);
    await mountGraph(page);

    await clickNode(page, 'hdl:jobs.spawn');
    await expect(panel(page).locator('h2')).toHaveText('handler · jobs.spawn');
    await expect(panel(page).locator('dt')).toHaveText(['emits']);
    await expect(panel(page).locator('ul > li')).toHaveText(['jobs.job.created']);
    await clickPane(page);

    await clickNode(page, 'hdl:messages.notify');
    await expect(panel(page).locator('h2')).toHaveText('handler · messages.notify');
    await expect(panel(page).locator('p.dx-sink')).toHaveText('— sink (emits no event)');
    await clickPane(page);
    await expect(panel(page)).toHaveCount(0);
  });

  test('an event node names the rules it triggers and the handlers that emit it', async ({ page }) => {
    await install(page);
    await mountGraph(page);

    await clickNode(page, 'evt:jobs.job.closed');
    await expect(panel(page).locator('h2')).toHaveText('event · jobs.job.closed');
    await expect(panel(page).locator('dt')).toHaveText(['triggers rules', 'emitted by']);
    await expect(panel(page).locator('ul > li')).toHaveText([RESOLVE.name]);
    // Packet closure is a jobs-api consequence (the system edge), not a
    // handler's emit, and the panel says which of the two it is.
    await expect(panel(page).locator('p.dx-sink')).toHaveText('— (external / jobs-api origin)');
    await clickPane(page);

    await clickNode(page, 'evt:jobs.step.completed');
    await expect(panel(page).locator('h2')).toHaveText('event · jobs.step.completed');
    await expect(panel(page).locator('p.dx-sink')).toHaveText('— (no rule listens for this exact topic)');
    await expect(panel(page).locator('ul > li')).toHaveText(['jobs.subjob_resolve']);
    await clickPane(page);
    await expect(panel(page)).toHaveCount(0);
  });
});

test.describe('/it/registry/dispatcher — the graph controls', () => {
  test('zoom in, zoom out and fit view do what they say; there is no lock; the minimap is there', async ({ page }) => {
    await install(page);
    await mountGraph(page);

    const controls = page.locator('.svelte-flow__controls button');
    await expect(controls).toHaveCount(3);
    await expect(page.locator('.svelte-flow__controls-interactive')).toHaveCount(0);
    await expect(page.locator('.svelte-flow__minimap')).toHaveCount(1);

    // fitView framed the graph on mount (zoom leaves 1).
    await expect.poll(() => zoom(page)).toBeLessThan(1);
    const fitted = await zoom(page);

    await page.getByRole('button', { name: 'Zoom In' }).click();
    await expect.poll(() => zoom(page)).toBeCloseTo(fitted * 1.2, 3);
    await page.getByRole('button', { name: 'Zoom Out' }).click();
    await expect.poll(() => zoom(page)).toBeCloseTo(fitted, 3);
    await page.getByRole('button', { name: 'Zoom In' }).click();
    await page.getByRole('button', { name: 'Zoom In' }).click();
    await expect.poll(() => zoom(page)).toBeCloseTo(fitted * 1.44, 3);
    await page.getByRole('button', { name: 'Fit View' }).click();
    await expect.poll(() => zoom(page)).toBeCloseTo(fitted, 3);
  });

  test('a node can be dragged, and a drag writes nothing', async ({ page }) => {
    const writes = watchWrites(page);
    await install(page);
    await mountGraph(page);

    const target = node(page, `rule:${PUBLISH.name}`);
    const before = await target.evaluate((el) => (el as HTMLElement).style.transform);
    const box = await target.boundingBox();
    expect(box).not.toBeNull();
    const x = box!.x + box!.width / 2;
    const y = box!.y + box!.height / 2;
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + 40, y + 60, { steps: 5 });
    await page.mouse.up();
    await expect
      .poll(() => target.evaluate((el) => (el as HTMLElement).style.transform))
      .not.toBe(before);
    expect(writes.map((w) => `${w.method()} ${w.url()}`)).toEqual([]);
  });

  test('the keyboard selects a node, and Backspace does not take it out of the picture', async ({ page }) => {
    // Not in the audit's inventory. The flow is elementsSelectable, and
    // the library's default deleteKey is Backspace, so a selected node
    // looked deletable from the drawing — a picture of the registry that
    // could omit a live rule. Measured here: a node selected from the
    // keyboard (focus, Enter — the library's a11y path) stays, with its
    // edges, and nothing is written. Pinned so a library upgrade that
    // starts deleting shows up as this test, not as a missing rule.
    const writes = watchWrites(page);
    await install(page);
    await mountGraph(page);

    const target = node(page, `rule:${ASSIGNED.name}`);
    await target.focus();
    await page.keyboard.press('Enter');
    await expect(target).toHaveClass(/(^|\s)selected(\s|$)/);
    await page.keyboard.press('Backspace');
    await expect(page.locator('.svelte-flow__node')).toHaveCount(NODES);
    await expect(page.locator('.svelte-flow__edge')).toHaveCount(EDGES);
    await expect(target).toHaveCount(1);
    expect(writes.map((w) => `${w.method()} ${w.url()}`)).toEqual([]);
  });
});

test.describe('/it/registry/dispatcher — links, reads and writes', () => {
  test('one unfiltered read, no forms, no writes, and the page’s links', async ({ page }) => {
    const writes = watchWrites(page);
    const reads: string[] = [];
    page.on('request', (r) => {
      if (RULES.test(new URL(r.url()).pathname)) reads.push(r.url());
    });
    await install(page);
    await mountGraph(page);

    expect(await settledReads(page, () => reads.length, 1)).toBe(1);
    expect(new URL(reads[0]!).search, 'the page reads every rule, unfiltered').toBe('');
    await expect(page.locator('.dx form')).toHaveCount(0);
    // The page's own buttons outside the flow: none until a trigger is
    // chosen ("×" and "show all" appear with the first chip).
    await expect(page.locator('.dx-head button, .dx-legend button, .dx-filter button')).toHaveCount(0);
    // "Edit rules →", and the library's attribution — external, a new
    // tab, not a route of this app.
    await expect(page.locator('.dx a')).toHaveCount(2);
    const attribution = page.getByRole('link', { name: 'Svelte Flow attribution' });
    await expect(attribution).toHaveAttribute('href', /^https:\/\/svelteflow\.dev/);
    await expect(attribution).toHaveAttribute('target', '_blank');
    expect(writes.map((w) => `${w.method()} ${w.url()}`)).toEqual([]);
  });

  test('the registry tabs land on catalogued routes, Dispatcher lit, and back returns here', async ({ page }) => {
    const writes = watchWrites(page);
    await install(page);
    await mountGraph(page);

    const tabs = page.locator('nav.it-tabs[aria-label="IT registry"] a');
    await expect(tabs).toHaveText(['Workflows', 'Dispatcher', 'Rules', 'Step plugins', 'Policy', 'Subjects', 'Drift']);
    await expect(tabs.nth(1)).toHaveAttribute('aria-current', 'page');
    await expect(tabs.nth(1)).toHaveAttribute('href', PAGE);
    expect(catalogued(PAGE)).toBe('system-dispatcher');
    const hrefs = await tabs.evaluateAll((as) => as.map((a) => a.getAttribute('href') ?? ''));
    for (const h of hrefs) {
      expect(catalogPaths().has(h), `${h} is a ROUTE_CATALOG path`).toBe(true);
      expect(catalogued(h), `${h} lights a catalogued section`).not.toBeNull();
    }
    for (const [i, h] of hrefs.entries()) {
      if (h === PAGE) continue;
      await tabs.nth(i).click();
      await expect(page).toHaveURL((u) => u.pathname === h);
      await backHere(page);
    }
    expect(writes.map((w) => `${w.method()} ${w.url()}`)).toEqual([]);
  });

  test('"Edit rules →" lands on the catalogued rule list, and back returns here', async ({ page }) => {
    await install(page);
    await mountGraph(page);

    const edit = page.getByRole('link', { name: 'Edit rules →' });
    await expect(edit).toHaveAttribute('href', RULES_PAGE);
    expect(catalogPaths().has(RULES_PAGE), `${RULES_PAGE} is a ROUTE_CATALOG path`).toBe(true);
    // The list lights the cascade's section, as the editor does.
    expect(catalogued(RULES_PAGE)).toBe('system-dispatcher');

    await edit.click();
    await expect(page).toHaveURL((u) => u.pathname === RULES_PAGE);
    await expect(page.locator('h1').first()).toHaveText('Dispatcher rules');
    await backHere(page);
  });
});

test.describe('/it/registry/dispatcher — loading, empty and failed are three paints', () => {
  test('while the read is in flight it says Loading… and nothing else claims a state', async ({ page }) => {
    await installSmokeMocks(page);
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route(RULES, async (r) => {
      await held;
      await json(r, payload(ROWS));
    });
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    await expect(page.locator('.dx-msg')).toHaveText('Loading dispatcher rules…');
    await expect(page.locator('.dx-stats')).toHaveCount(0);
    await expect(page.locator('.dx-filter')).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.getByText('No dispatcher rules are loaded.')).toHaveCount(0);

    release();
    await expect(page.locator('.svelte-flow__node')).toHaveCount(NODES);
    await expect(page.locator('.dx-msg')).toHaveCount(0);
  });

  test('an empty rule set with the server’s real roster is the empty line, unmarked', async ({ page }) => {
    // GAP 3 (backlog 4d9b09e0), closed: the server always sends its
    // handler roster and system edges, and an empty registry used to
    // paint them as a full picture. Only invoked handlers are drawn now
    // (ec40e269), so nothing is.
    await install(page, []);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    await expect(page.locator('.dx-msg')).toHaveText('No dispatcher rules are loaded.');
    await expect(page.locator('.dx-stats span')).toHaveText(['0 rules', '0 handlers', '0 in cycles']);
    await expect(page.locator('.svelte-flow')).toHaveCount(0);
    await expect(page.locator('.dx-filter')).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.getByRole('link', { name: 'Edit rules →' })).toHaveCount(1);
  });

  test('UNFILED: one rule and one handler are counted in the plural', async ({ page }) => {
    // The words finding no item owns: the stats have no singular, where
    // the rule list's header ("1 active rule") and this page's own
    // filter note ("1 trigger") do.
    await install(page, [ASSIGNED]);
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });
    await expect(page.locator('.svelte-flow__node')).toHaveCount(3);
    await expect(page.locator('.dx-stats span')).toHaveText(['1 rules', '1 handlers', '0 in cycles']);
  });

  test('a read that never answers is a marked failure, not the empty line', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(RULES, (r) => r.abort('failed'));
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const failure = page.locator(`.dx ${FAILURE_MARKER}[role=alert]`);
    await expect(failure).toHaveText('Couldn’t load rules: Failed to fetch');
    await expect(page.getByText('No dispatcher rules are loaded.')).toHaveCount(0);
    await expect(page.locator('.dx-stats')).toHaveCount(0);
    await expect(page.locator('.dx-filter')).toHaveCount(0);
    await expect(page.getByRole('link', { name: 'Edit rules →' })).toHaveCount(1);
  });

  test('a 200 whose body is not JSON is a marked failure, not the empty line', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(RULES, (r) => r.fulfill({ status: 200, contentType: 'application/json', body: 'not json' }));
    await mountPage(page, PAGE, { titleMatch: new RegExp(TITLE) });

    const failure = page.locator(`.dx ${FAILURE_MARKER}[role=alert]`);
    await expect(failure).toHaveCount(1);
    await expect(failure).toHaveText(/^Couldn’t load rules: .*JSON/);
    await expect(page.getByText('No dispatcher rules are loaded.')).toHaveCount(0);
    await expect(page.locator('.dx-stats')).toHaveCount(0);
  });
});
