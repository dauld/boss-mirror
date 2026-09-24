// /it/operate/audit — the Audit Log (catalogued as "Monitoring"), pinned
// control by control. Page audit 65a273d5, step `test`.
//
// WHY THIS FILE EXISTS. The route sat in DEFERRED (_routes.ts) for want
// of an object-shaped /api/events/stats fixture, so route-smoke,
// interaction-crawl and outage-crawl all skipped it and none of its five
// reads was pinned in a browser (gap 5, backlog 0398c4d0). This spec
// carries the fixture and the page's own meaning: every control from the
// audit's `controls_md` — 9 tab links, 3 buttons, 1 clickable row, 7
// inputs, 5 reads, 0 writes — and what each one does against the mock.
//
// IT PINS THE PAGE AS IT IS, GAPS INCLUDED. Where the audit filed a gap,
// the assertion below states today's behaviour and names the backlog id,
// so the car that closes the gap turns that assertion red and must
// rewrite it — a gap cannot close without this file saying so:
//   91b41817  the page has two names; the row toggle is mouse-only
//
// THE STREAM IS MOCKED HONESTLY, AND HERE IS WHAT THAT CAN AND CANNOT
// REACH. Playwright fulfils an EventSource request with a whole body and
// ends the connection, so a mocked stream can deliver N frames and then
// either END (the browser schedules a reconnect after `retry`) or FAIL
// (a non-200 status: readyState CLOSED, no reconnect). Both are pinned
// below. Gap 260879f5 was a third shape a mocked browser could NOT make:
// tail_http.rs answered a failed read with `Err(_) => continue`, holding
// the connection open and silent. Since 2026-09-24 the server sends one
// `event: failed` frame naming the error and ENDS the stream — a whole
// body, which a mock can fulfil — so that shape is pinned here too, and
// its server half in crates/core/boss-events/tests/tail_http.rs. Each
// shape now paints its own line: connecting, reconnecting, and down
// (with the server's words when it gave any), then the 5s poll.

import { expect, test, type Page, type Request, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { FAILURE_MARKER } from './_routes';
import { AUDIT_STATS, installSmokeMocks } from './_smokeMocks';
import { parseRoute } from '../../src/router';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = '/it/operate/audit';

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const TAIL = /\/api\/events\/tail(\?|$)/;
const STATS = /\/api\/events\/stats$/;
const STREAM = /\/api\/events\/stream(\?|$)/;
const EXPORT = /\/api\/events\/export(\?|$)/;
const FILES = /\/api\/files\?/;

/// Three rows in the shape /api/events/tail returns: every payload
/// carries `_actor`, `_partition`, `_simulated` (3 of 3 live rows, the
/// audit's needs_md), and a jobs.step.* payload carries its packet as
/// `job_id` beside the step's own `id` — the shape boss-jobs writes
/// (events.rs, step_state_payload).
const ROWS = [
  {
    event_id: 'ev-3',
    timestamp: '2026-09-23T21:44:03.250Z',
    source: 'jobs',
    kind: 'jobs.step.updated',
    payload: { id: 'step-1', step_id: 'step-1', job_id: 'job-1', _actor: 'agent-claude', _partition: 'jobs', _simulated: false },
  },
  {
    event_id: 'ev-2',
    timestamp: '2026-09-23T21:43:02.125Z',
    source: 'dispatcher',
    kind: 'dispatcher.rule.fired',
    payload: { rule: 'r1', _actor: 'automation:dispatcher', _partition: 'dispatcher', _simulated: false },
  },
  {
    event_id: 'ev-1',
    timestamp: '2026-09-23T21:42:01.000Z',
    source: 'credential',
    kind: 'credential.rotate.verified',
    payload: { credential: 'forge', _actor: 'automation:broker', _partition: 'credential', _simulated: false },
  },
];

/// The page's reads, counted and kept, so a test can say exactly what
/// the page asked for.
type Seen = { tail: string[]; stream: string[]; stats: number; files: string[]; writes: string[] };

function watch(page: Page): Seen {
  const seen: Seen = { tail: [], stream: [], stats: 0, files: [], writes: [] };
  page.on('request', (r: Request) => {
    const url = r.url();
    if (TAIL.test(url)) seen.tail.push(url);
    else if (STREAM.test(url)) seen.stream.push(url);
    else if (STATS.test(url)) seen.stats += 1;
    else if (FILES.test(url)) seen.files.push(url);
    // The chrome records every route open with a fire-and-forget POST;
    // that is the shell's write, not this page's.
    if (r.method() !== 'GET' && url.includes('/api/') && !url.includes('/api/surface-opens')) {
      seen.writes.push(`${r.method()} ${url}`);
    }
  });
  return seen;
}

/// The page's reads, healthy. The stream answers installSmokeMocks' own
/// rule unless a test installs its own: a well-formed empty stream that
/// ends, so the page says it is reconnecting. It was a 204, which fails
/// the EventSource and paints "Live stream down" — a failure line, on
/// the shared marker since sweep c3e4edcc, which a healthy render must
/// not show.
async function installAuditReads(page: Page, rows: ReadonlyArray<unknown> = ROWS): Promise<void> {
  await installSmokeMocks(page);
  await page.route(STATS, (r) => json(r, AUDIT_STATS));
  await page.route(TAIL, (r) => json(r, rows));
  await page.route(FILES, (r) => json(r, []));
}

const param = (url: string, key: string): string | null => new URL(url).searchParams.get(key);
const last = (xs: ReadonlyArray<string>): string => xs[xs.length - 1] ?? '';

/// parseRoute reads `window.location.search` for two routes; this is
/// Node, so give it the one field it reads (interaction-crawl does the
/// same).
function route(path: string): ReturnType<typeof parseRoute> {
  (globalThis as { window?: unknown }).window = { location: { search: '', pathname: path } };
  return parseRoute(path);
}

const CATALOGUED = new Set(Object.values(ROUTE_CATALOG).map((i) => i.path));

/// The operate tab strip, in order, as ItTabs.svelte declares it — and
/// whether the nav catalog (the ONE list, never a second one) carries
/// each path. Four do not: that is today's catalog, pinned so a change
/// to it is a decision this file hears about.
const TABS: ReadonlyArray<{ label: string; path: string; catalogued: boolean }> = [
  { label: 'Incidents', path: '/it/operate', catalogued: true },
  { label: 'Yard status', path: '/it/operate/yard-status', catalogued: false },
  { label: 'Conductor', path: '/it/operate/conductor', catalogued: false },
  { label: 'Audit Log', path: '/it/operate/audit', catalogued: true },
  { label: 'Performance', path: '/it/operate/perf', catalogued: false },
  { label: 'Atlas', path: '/it/operate/atlas', catalogued: false },
  { label: 'Bottlenecks', path: '/it/operate/bottlenecks', catalogued: true },
  { label: 'Receiving Yard', path: '/it/yard/receiving', catalogued: true },
  { label: 'Marshalling Yard', path: '/it/yard/marshalling', catalogued: true },
];

const stream = (page: Page) => page.locator('.events-table tbody tr.events-row');
/// The one line that says what the live stream is doing (260879f5).
const liveLine = (page: Page) => page.locator('.events-live');

test.describe('/it/operate/audit — the page and its names', () => {
  test('the header, the three sections, and the two names the page goes by', async ({ page }) => {
    await installAuditReads(page);
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await expect(page.getByText('IT · Event stream')).toBeVisible();
    await expect(page.getByText('Live tail of every domain event. Operator tier only.')).toBeVisible();
    for (const s of ['Size and growth', 'Filters', 'Stream']) {
      await expect(page.getByRole('heading', { name: s, exact: true })).toBeVisible();
    }
    // Gap 91b41817: the catalog calls this route "Monitoring", the tab
    // and the header call it "Audit Log".
    expect(ROUTE_CATALOG['system-monitoring'].path).toBe(PATH);
    expect(ROUTE_CATALOG['system-monitoring'].label).toBe('Monitoring');
    expect(route(PATH)).toEqual({ kind: 'systemMonitoringEvents' });
  });
});

test.describe('/it/operate/audit — the tab links', () => {
  test('nine tabs, each served by the router, five of them catalogued', async ({ page }) => {
    await installAuditReads(page);
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    const tabs = page.locator('nav.it-tabs a');
    await expect(tabs).toHaveText(TABS.map((t) => t.label));
    for (const t of TABS) {
      await expect(page.locator('nav.it-tabs').getByRole('link', { name: t.label, exact: true })).toHaveAttribute('href', t.path);
      const r = route(t.path);
      expect(r.kind, `${t.path} must not fall to the router's catch-all`).not.toBe('home');
      expect(CATALOGUED.has(t.path), `${t.label} catalogued`).toBe(t.catalogued);
    }
    await expect(page.locator('nav.it-tabs a[aria-current="page"]')).toHaveText('Audit Log');
  });

  for (const t of TABS.filter((x) => x.path !== PATH)) {
    test(`"${t.label}" lands on ${t.path} and back returns to the audit log`, async ({ page }) => {
      await installAuditReads(page);
      await mountPage(page, PATH, { titleMatch: /Audit Log/ });
      await page.locator('nav.it-tabs').getByRole('link', { name: t.label, exact: true }).click();
      await expect(page).toHaveURL(new RegExp(`${t.path}$`));
      await page.goBack();
      await expect(page).toHaveURL(new RegExp(`${PATH}$`));
      await expect(page.locator('h1').first()).toContainText('Audit Log');
    });
  }
});

test.describe('/it/operate/audit — size and growth (GET /api/events/stats)', () => {
  test('the five figures, the per-day bars and the busiest kinds', async ({ page }) => {
    await installAuditReads(page);
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    const stat = (label: string) =>
      page.locator('.events-stat', { has: page.locator('.events-stat-label', { hasText: label }) }).locator('.events-stat-value');
    await expect(stat('Rows')).toHaveText((384521).toLocaleString());
    await expect(stat('On disk')).toHaveText('492 MB');
    await expect(stat('Last 24h')).toHaveText((78516).toLocaleString());
    await expect(stat('Per day, 7d avg')).toHaveText(Math.round(420000 / 7).toLocaleString());
    await expect(stat('Since')).not.toHaveText('—');
    await expect(page.getByRole('heading', { name: 'Rows per day, last 30 days' })).toBeVisible();
    await expect(page.locator('.events-day')).toHaveCount(AUDIT_STATS.per_day.length);
    await expect(page.locator('.events-day-label').first()).toHaveText('09-22');
    await expect(page.getByRole('heading', { name: 'Busiest kinds, last 30 days' })).toBeVisible();
    await expect(page.locator('.events-top-kinds td.events-kind')).toHaveText(AUDIT_STATS.top_kinds.map((k) => k.kind));
  });

  test('a failed stats read says so, in the page\'s words, on the shared marker (c3e4edcc)', async ({ page }) => {
    await installAuditReads(page);
    await page.route(STATS, (r) => json(r, 'down', 500));
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    const line = page.getByText('Stats unavailable: HTTP 500');
    await expect(line).toBeVisible();
    // Gap 10, closed by sweep c3e4edcc: the line wears the marker the
    // outage crawl counts, as an alert.
    await expect(page.locator(`.events-stats-note${FAILURE_MARKER}[role="alert"]`)).toHaveCount(1);
  });

  test('while the stats read is outstanding the section says it is measuring', async ({ page }) => {
    await installAuditReads(page);
    let release: () => void = () => {};
    const held = new Promise<void>((r) => (release = r));
    await page.route(STATS, async (r) => {
      await held;
      await json(r, AUDIT_STATS).catch(() => {});
    });
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await expect(page.getByText('Measuring the log…')).toBeVisible();
    release();
    await expect(page.getByText('Measuring the log…')).toHaveCount(0);
  });
});

test.describe('/it/operate/audit — the stream (GET /api/events/tail)', () => {
  test('the default read is All, limit 100; the rows paint Time / Source / Kind / Actor', async ({ page }) => {
    await installAuditReads(page);
    const seen = watch(page);
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await expect(stream(page)).toHaveCount(3);
    await expect(page.locator('.events-table thead th')).toHaveText(['Time', 'Source', 'Kind', 'Actor']);
    await expect(stream(page).first().locator('td').nth(1)).toHaveText('jobs');
    await expect(stream(page).first().locator('td').nth(2)).toHaveText('jobs.step.updated');
    // Backlog 03f79eca: who acted is a column, read off the payload's
    // `_actor`, not only a line inside the expanded JSON.
    await expect(stream(page).locator('td:nth-child(4)')).toHaveText([
      'agent-claude',
      'automation:dispatcher',
      'automation:broker',
    ]);
    await expect(stream(page).first().locator('td').first()).toHaveAttribute('title', '2026-09-23T21:44:03.250Z');
    const first = seen.tail[0] ?? '';
    // All since 34ea2ae0 (decided under page audit 65a273d5): this
    // instance's log is real work, so "Real only" hid nearly nothing
    // while naming a filter. "All" sends no provenance parameter.
    expect(param(first, 'simulated')).toBeNull();
    expect(param(first, 'limit')).toBe('100');
    expect(param(first, 'source')).toBeNull();
    expect(param(first, 'kind')).toBeNull();
    expect(param(first, 'actor')).toBeNull();
    await expect(page.locator('.events-freshness')).toContainText(/Last: \d\d:\d\d:\d\d\.\d{3}/);
  });

  test('a row predating the _actor stamp paints a dash, not a blank that reads as nobody (03f79eca)', async ({ page }) => {
    await installAuditReads(page, [
      { event_id: 'ev-old', timestamp: '2026-09-01T00:00:00.000Z', source: 'jobs', kind: 'jobs.job.opened', payload: { id: 'job-0' } },
    ]);
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await expect(stream(page).first().locator('td').nth(3)).toHaveText('—');
    await stream(page).first().click();
    // The payload row still spans every column.
    await expect(page.locator('.events-payload-row > td')).toHaveAttribute('colspan', '4');
  });

  test('an EMPTY answer is "nothing here", a FAILED one is a failure line — never the same paint', async ({ page }) => {
    await installAuditReads(page, []);
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await expect(page.getByText('No events match these filters.')).toBeVisible();
    await expect(page.getByText(/^Failed to load:/)).toHaveCount(0);

    await page.unroute(TAIL);
    await page.route(TAIL, (r) => r.fulfill({ status: 403, contentType: 'text/plain', body: 'operator tier required' }));
    await page.reload();
    await expect(page.getByText('Failed to load: HTTP 403: operator tier required')).toBeVisible();
    await expect(page.getByText('No events match these filters.')).toHaveCount(0);
    // c3e4edcc: the failure line wears the shared marker, as an alert.
    await expect(page.locator(`${FAILURE_MARKER}[role="alert"]`)).toHaveText(
      'Failed to load: HTTP 403: operator tier required',
    );
  });

  test('each filter re-reads the tail with its own parameter', async ({ page }) => {
    await installAuditReads(page);
    const seen = watch(page);
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await expect(stream(page)).toHaveCount(3);

    const sourceBox = page.getByLabel('Source');
    await expect(sourceBox).toHaveAttribute('placeholder', 'e.g. jobs, assets');
    // The datalist is built from the sources in the current batch.
    await expect(page.locator('#events-sources option')).toHaveCount(3);
    await sourceBox.fill('jobs');
    await expect.poll(() => param(last(seen.tail), 'source')).toBe('jobs');

    const kindBox = page.getByLabel('Kind contains');
    await expect(kindBox).toHaveAttribute('placeholder', 'e.g. step, invoice');
    await kindBox.fill('step');
    await expect.poll(() => param(last(seen.tail), 'kind')).toBe('step');

    // Backlog 03f79eca: an exact actor, offered from the actors in the
    // current batch, sent to the snapshot AND the live stream — a lens
    // the stream ignored would paint every other actor into the view.
    const actorBox = page.getByLabel('Actor');
    await expect(actorBox).toHaveAttribute('placeholder', 'e.g. agent-claude');
    await expect(page.locator('#events-actors option')).toHaveCount(3);
    await actorBox.fill('agent-claude');
    await expect.poll(() => param(last(seen.tail), 'actor')).toBe('agent-claude');
    await expect.poll(() => param(last(seen.stream), 'actor')).toBe('agent-claude');

    const prov = page.getByLabel('Provenance');
    await expect(prov.locator('option')).toHaveText(['Real only', 'Simulated only', 'All']);
    await expect(prov).toHaveValue('all');
    // Backlog 34ea2ae0: the lens reaches the live stream as well as the
    // snapshot. It used to reach only the snapshot, so a synthetic row
    // streamed into a view the select called "Real only".
    await prov.selectOption('real');
    await expect.poll(() => param(last(seen.tail), 'simulated')).toBe('real');
    await expect.poll(() => param(last(seen.stream), 'simulated')).toBe('real');
    await prov.selectOption('sim');
    await expect.poll(() => param(last(seen.tail), 'simulated')).toBe('sim');
    await expect.poll(() => param(last(seen.stream), 'simulated')).toBe('sim');
    const before = seen.tail.length;
    const streamsBefore = seen.stream.length;
    await prov.selectOption('all');
    await expect.poll(() => seen.tail.length).toBeGreaterThan(before);
    await expect.poll(() => seen.stream.length).toBeGreaterThan(streamsBefore);
    // "All" sends no provenance parameter at all, to either read.
    expect(param(last(seen.tail), 'simulated')).toBeNull();
    expect(param(last(seen.stream), 'simulated')).toBeNull();

    const lim = page.getByLabel('Limit');
    await expect(lim.locator('option')).toHaveText(['50', '100', '200', '500']);
    await lim.selectOption('500');
    await expect.poll(() => param(last(seen.tail), 'limit')).toBe('500');
    // 500 rows is no longer the furthest the page reads: the Since and
    // Until window is pinned in "a Since / Until window ..." (62a0bbee).
  });

  test('a row opens its payload, event id and attachments on click, and closes on a second click', async ({ page }) => {
    await installAuditReads(page);
    const seen = watch(page);
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    const row = stream(page).first();
    await row.click();
    await expect(row).toHaveClass(/events-row-open/);
    const open = page.locator('.events-payload-row');
    await expect(open.locator('pre.events-payload')).toContainText('"_actor": "agent-claude"');
    await expect(open.locator('.events-event-id')).toHaveText('event_id: ev-3');
    await expect(open.getByText('No attachments yet.')).toBeVisible();
    expect(seen.files.map((u) => `${param(u, 'target_kind')}:${param(u, 'target_id')}`)).toEqual(['event:ev-3']);
    // canEdit=false: no upload surface, no Detach button.
    await expect(open.locator('.files-drop')).toHaveCount(0);
    await expect(open.locator('.files-delete')).toHaveCount(0);
    // Backlog 62a0bbee: a row that belongs to a packet links to it —
    // the step event's `job_id`, never the step's own `id`.
    const packet = open.locator('.events-packet a');
    await expect(packet).toHaveAttribute('href', '/jobs/job-1');
    await expect(packet).toHaveText('job-1');
    await expect(open.locator('a[href="/jobs/step-1"]')).toHaveCount(0);
    await row.click();
    await expect(page.locator('.events-payload-row')).toHaveCount(0);
    // Gap 91b41817: the toggle is a bare <tr onclick> — no tabindex and
    // no role, so it cannot be reached from the keyboard.
    expect(await row.getAttribute('tabindex')).toBeNull();
    expect(await row.getAttribute('role')).toBeNull();
  });

  test('a failed attachments read says so; a 503 renders the designed-off note', async ({ page }) => {
    await installAuditReads(page);
    await page.route(FILES, (r) => {
      const id = new URL(r.request().url()).searchParams.get('target_id');
      return id === 'ev-3' ? json(r, 'down', 500) : json(r, 'off', 503);
    });
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await stream(page).first().click();
    await expect(page.getByText("Couldn't load attachments — list files: HTTP 500")).toBeVisible();
    // The shared marker, as an alert (sweep c3e4edcc).
    await expect(page.locator(`.files-error${FAILURE_MARKER}[role="alert"]`)).toHaveCount(1);
    await stream(page).nth(1).click();
    await expect(page.getByText("File attachments aren't enabled in this deployment", { exact: false })).toBeVisible();
  });

  test('a row that belongs to no packet opens with no packet link (62a0bbee)', async ({ page }) => {
    await installAuditReads(page);
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    // The dispatcher row's payload carries `rule`, no `job_id`.
    await stream(page).nth(1).click();
    const open = page.locator('.events-payload-row');
    await expect(open.locator('.events-event-id')).toHaveText('event_id: ev-2');
    await expect(open.locator('.events-packet')).toHaveCount(0);
  });

  test('a Since / Until window reads any stretch of the log, and an Until stops the live stream (62a0bbee)', async ({ page }) => {
    // 500 rows was the widest view — about ten minutes of log — so a
    // retro or an incident could read no further back without the
    // export. The tail has always taken `since` (inclusive) and `until`
    // (exclusive); the page now sends them.
    await installAuditReads(page);
    const seen = watch(page);
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await expect(stream(page)).toHaveCount(3);
    await expect.poll(() => seen.stream.length).toBe(1);
    expect(param(last(seen.tail), 'since')).toBeNull();
    expect(param(last(seen.tail), 'until')).toBeNull();

    // The inputs hold the browser's zone, the zone the rows are painted
    // in; the read carries the UTC instant.
    const inUtc = (local: string) => page.evaluate((v) => new Date(v).toISOString(), local);
    const since = page.getByLabel('Since');
    await expect(since).toHaveAttribute('type', 'datetime-local');
    await since.fill('2026-09-20T08:00');
    const sinceUtc = await inUtc('2026-09-20T08:00');
    await expect.poll(() => param(last(seen.tail), 'since')).toBe(sinceUtc);
    expect(param(last(seen.tail), 'until')).toBeNull();
    // A Since alone leaves the window open at the top: new rows still
    // land inside it, so the stream reopens with the read.
    await expect.poll(() => seen.stream.length).toBeGreaterThan(1);

    const streams = seen.stream.length;
    await page.getByLabel('Until').fill('2026-09-20T09:30');
    const untilUtc = await inUtc('2026-09-20T09:30');
    await expect.poll(() => param(last(seen.tail), 'until')).toBe(untilUtc);
    expect(param(last(seen.tail), 'since')).toBe(sinceUtc);
    // An Until closes the window: no row landing now can fall inside
    // it, so no stream is opened and the line says why.
    await expect(liveLine(page)).toHaveText(
      'Live stream off: the Until bound closes the window, so no new row can land in it. Clear Until to follow the log.',
    );
    await page.waitForTimeout(500);
    expect(seen.stream.length).toBe(streams);
    await expect(page.getByLabel('Live (SSE)')).toBeChecked();

    // Clearing both bounds is the unbounded tail again, streaming.
    await page.getByLabel('Until').fill('');
    await since.fill('');
    await expect.poll(() => param(last(seen.tail), 'since')).toBeNull();
    expect(param(last(seen.tail), 'until')).toBeNull();
    expect(seen.stream.length).toBeGreaterThan(streams);
  });
});

test.describe('/it/operate/audit — the live stream (EventSource /api/events/stream)', () => {
  test('frames arrive at the top, deduped against the snapshot; under All the stream sends no provenance', async ({ page }) => {
    await installAuditReads(page);
    const seen = watch(page);
    // A long `retry` so the ended connection does not reconnect inside
    // the test: this leg is about what the frames do.
    const frames = [
      // A duplicate of a snapshot row: must not paint twice.
      ROWS[0],
      { event_id: 'ev-4', timestamp: '2026-09-23T21:45:00.000Z', source: 'jobs', kind: 'jobs.job.opened', payload: { id: 'job-2', _actor: 'agent-claude', _simulated: false } },
      // A SIMULATED row, arriving under the default "All" lens, which
      // keeps it.
      { event_id: 'ev-sim', timestamp: '2026-09-23T21:45:01.000Z', source: 'brewery', kind: 'brewery.batch.started', payload: { _actor: 'sim', _simulated: true } },
      'not json',
    ];
    const body = 'retry: 3600000\n' + frames.map((f) => `data: ${typeof f === 'string' ? f : JSON.stringify(f)}\n\n`).join('');
    // THE FRAMES ARE RELEASED ONLY AFTER THE SNAPSHOT THEY DEDUPE AGAINST
    // HAS PAINTED (backlog 797c5fcb). Each filter change re-runs the
    // page's effect, which fires the tail read and opens the stream in
    // the same tick, and a snapshot that answers AFTER the frames
    // REPLACES them. Mocked with the frames on every stream and the rows
    // on every tail, the order was the scheduler's: under
    // --repeat-each=10 --workers=4 this test went red 3 times in 10, each
    // time reading the snapshot's own rows where the frames belonged
    // (and two unrelated gates on 2026-09-23 went red the same way). So
    // the order is fixed here, not waited out: the reads before the last
    // filter answer nothing, which makes the filtered snapshot's three
    // rows the only three-row paint there can be; the streams before it
    // carry no frames; and the filtered stream is held until the test
    // has SEEN that paint. What the frames then do to it is the page's
    // doing alone. (The page's own race — a live frame landing before
    // its snapshot is lost — is a property of EventsPage.svelte, not of
    // this pin.)
    const filtered = (url: string): boolean => param(url, 'kind') === 'j';
    let releaseFrames: () => void = () => {};
    const snapshotPainted = new Promise<void>((r) => (releaseFrames = r));
    await page.route(TAIL, (r) => json(r, filtered(r.request().url()) ? ROWS : []));
    await page.route(STREAM, async (r) => {
      const live = filtered(r.request().url());
      if (live) await snapshotPainted;
      await r
        .fulfill({
          status: 200,
          headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' },
          body: live ? body : 'retry: 3600000\n\n',
        })
        // A held stream the page has already closed has nothing to fulfil.
        .catch(() => {});
    });
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await page.getByLabel('Source').fill('jobs');
    await page.getByLabel('Kind contains').fill('j');
    await expect.poll(() => seen.stream.length).toBeGreaterThan(0);
    await expect(stream(page)).toHaveCount(3);
    releaseFrames();
    await expect(stream(page)).toHaveCount(5);
    const ids = await stream(page).locator('td:nth-child(3)').allInnerTexts();
    expect(ids.slice(0, 2)).toEqual(['brewery.batch.started', 'jobs.job.opened']);
    const url = last(seen.stream);
    expect(param(url, 'source')).toBe('jobs');
    expect(param(url, 'kind')).toBe('j');
    // Under All the stream carries no provenance, so the simulated row
    // belongs in the view; the lens-set legs are pinned in "each filter
    // re-reads the tail with its own parameter" (34ea2ae0).
    expect(param(url, 'simulated')).toBeNull();
    await expect(page.getByLabel('Provenance')).toHaveValue('all');
    await expect(page.getByLabel('Live (SSE)')).toBeChecked();
  });

  test('a refused stream falls back to the 5s poll and the page says so (260879f5)', async ({ page }) => {
    await installAuditReads(page);
    const seen = watch(page);
    await page.route(STREAM, (r) => r.fulfill({ status: 500, contentType: 'text/plain', body: 'stream down' }));
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await expect(stream(page)).toHaveCount(3);
    await expect.poll(() => seen.stream.length).toBe(1);
    // The browser hands an EventSource no status and no body, so the
    // line says the stream was refused and that it cannot say why —
    // true, where a guessed cause would not be.
    await expect(liveLine(page)).toHaveText(
      'Live stream down: the server refused it (the browser does not say why). Re-reading the tail every 5 s.',
    );
    const tailsAtMount = seen.tail.length;
    // The fallback poll is SNAPSHOT_RELOAD_MS = 5000, so the suite's
    // stated expect budget covers it twice over. This wait capped itself
    // at 12 000 ms until backlog de205627 — under the budget, and the one
    // number in the file a loaded host could beat.
    await expect.poll(() => seen.tail.length).toBeGreaterThan(tailsAtMount);
    // The failed stream is not retried: a non-200 answer CLOSES an
    // EventSource.
    expect(seen.stream.length).toBe(1);
    // Sweep c3e4edcc: the down line is the page's one failure here, and
    // it wears the shared marker, as an alert.
    await expect(page.locator(`${FAILURE_MARKER}[role="alert"]`)).toHaveCount(1);
    await expect(liveLine(page)).toHaveClass(/\bload-failed\b/);
  });

  test('a stream whose server read failed says why, is closed, and the page polls (260879f5)', async ({ page }) => {
    // The server's side of the gap: a failed read of audit_log is one
    // `event: failed` frame naming the error, then the end of the body.
    // `retry: 100` makes the browser reconnect at once if the page left
    // the EventSource open, so a page that ignored the frame would show
    // up as more stream requests.
    await installAuditReads(page);
    const seen = watch(page);
    const error = 'reading rows past the stream\'s cursor: error returned from database: relation "audit_log" does not exist';
    await page.route(STREAM, (r) =>
      r.fulfill({
        status: 200,
        headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' },
        body: `retry: 100\nevent: failed\ndata: ${JSON.stringify({ error })}\n\n`,
      }),
    );
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await expect(liveLine(page)).toHaveText(`Live stream down: ${error}. Re-reading the tail every 5 s.`);
    const tailsAtFailure = seen.tail.length;
    await expect.poll(() => seen.tail.length).toBeGreaterThan(tailsAtFailure);
    // Closed on the frame, so the browser never reconnects.
    expect(seen.stream.length).toBe(1);
    // The frame is not a row.
    await expect(stream(page)).toHaveCount(3);
  });

  test('a stream still connecting says so (260879f5)', async ({ page }) => {
    await installAuditReads(page);
    // Held: the response never starts, so the EventSource never opens.
    // Released as an abort at the end, so the request is answered by
    // the mock rather than leaking to the dev-server.
    let release: () => void = () => {};
    const held = new Promise<void>((r) => (release = r));
    await page.route(STREAM, async (r) => {
      await held;
      await r.abort().catch(() => {});
    });
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await expect(stream(page)).toHaveCount(3);
    await expect(liveLine(page)).toHaveText('Live stream connecting…');
    release();
  });

  test('a stream that ended says it is reconnecting, and what that costs (260879f5)', async ({ page }) => {
    await installAuditReads(page);
    const seen = watch(page);
    // An open stream whose body ends: the browser schedules a reconnect
    // after `retry`, long enough here that the test sees the wait. The
    // server re-anchors a new connection at the log's newest row, so
    // rows that land in the gap never stream — the line says so.
    await page.route(STREAM, (r) =>
      r.fulfill({
        status: 200,
        headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' },
        body: 'retry: 3600000\n\n',
      }),
    );
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await expect(liveLine(page)).toHaveText(
      'Live stream lost; the browser is reconnecting. Rows that land before it is back will not stream — reload to read them.',
    );
    expect(seen.stream.length).toBe(1);
    // A reconnect is the browser's, still under way — not a failed read,
    // so it does not wear the marker (c3e4edcc marks only "down").
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('unticking Live (SSE) paints no stream line', async ({ page }) => {
    await installAuditReads(page);
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await expect(liveLine(page)).toHaveCount(1);
    await page.getByLabel('Live (SSE)').uncheck();
    await expect(liveLine(page)).toHaveCount(0);
  });

  test('unticking Live (SSE) reads one snapshot and opens no stream', async ({ page }) => {
    await installAuditReads(page);
    const seen = watch(page);
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await expect(page.getByLabel('Live (SSE)')).toBeChecked();
    await expect.poll(() => seen.stream.length).toBe(1);
    const tails = seen.tail.length;
    await page.getByLabel('Live (SSE)').uncheck();
    await expect.poll(() => seen.tail.length).toBe(tails + 1);
    await page.waitForTimeout(6_000);
    expect(seen.stream.length).toBe(1);
    expect(seen.tail.length).toBe(tails + 1);
  });

  test('a frame that beats the snapshot is still on screen after the snapshot paints (697f9f87)', async ({ page }) => {
    // The page fires the tail read and opens the stream in one effect
    // run, so either may answer first. The server anchors the stream at
    // MAX(id) when it connects, so a frame that lands before the
    // snapshot answers is a row the snapshot may never carry — and the
    // page used to REPLACE it with the snapshot. Here the snapshot is
    // held until the frames have painted, then released: the rows must
    // be exactly what the other order paints — the new frame on top,
    // the snapshot below it, the duplicate once.
    await installAuditReads(page);
    let releaseSnapshot: () => void = () => {};
    const framesPainted = new Promise<void>((r) => (releaseSnapshot = r));
    await page.route(TAIL, async (r) => {
      await framesPainted;
      await json(r, ROWS).catch(() => {});
    });
    const frames = [
      // A duplicate of a snapshot row: must still paint once.
      ROWS[0],
      { event_id: 'ev-4', timestamp: '2026-09-23T21:45:00.000Z', source: 'jobs', kind: 'jobs.job.opened', payload: { id: 'job-2', _actor: 'agent-claude', _simulated: false } },
    ];
    const body = 'retry: 3600000\n' + frames.map((f) => `data: ${JSON.stringify(f)}\n\n`).join('');
    await page.route(STREAM, (r) =>
      r.fulfill({ status: 200, headers: { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' }, body }),
    );
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    // The frames alone, before any snapshot has answered.
    await expect(stream(page).locator('td:nth-child(3)')).toHaveText(['jobs.job.opened', 'jobs.step.updated']);
    releaseSnapshot();
    await expect(stream(page).locator('td:nth-child(3)')).toHaveText([
      'jobs.job.opened',
      'jobs.step.updated',
      'dispatcher.rule.fired',
      'credential.rotate.verified',
    ]);
  });
});

test.describe('/it/operate/audit — Download ⤓ (GET /api/events/export)', () => {
  test('Download ⤓ opens the panel with a 7-day window; Cancel closes it without a request', async ({ page }) => {
    await installAuditReads(page);
    const exports: string[] = [];
    page.on('request', (r) => {
      if (EXPORT.test(r.url())) exports.push(r.url());
    });
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    const open = page.getByRole('button', { name: 'Download ⤓' });
    await expect(open).toHaveAttribute('title', 'Export matching events as a JSON Lines file');
    await open.click();
    const panel = page.locator('.events-download-panel');
    await expect(panel).toBeVisible();
    const from = await panel.getByLabel('From').inputValue();
    const to = await panel.getByLabel('To').inputValue();
    expect(from).toMatch(/^\d{4}-\d{2}-\d{2}$/);
    expect((Date.parse(to) - Date.parse(from)) / 86_400_000).toBe(7);
    await expect(panel.locator('.events-download-hint')).toHaveText(
      'Exports up to 50,000 events matching the current source, kind, actor and provenance filters in the window above as JSON Lines (one event per line — parseable by jq, log forwarders, and most analytics tools). Narrow the window for large ranges.',
    );
    await panel.getByRole('button', { name: 'Cancel' }).click();
    await expect(panel).toHaveCount(0);
    await open.click();
    await expect(panel).toBeVisible();
    await open.click();
    await expect(panel).toHaveCount(0);
    expect(exports).toEqual([]);
  });

  test('Save .jsonl downloads the window with the page\'s filters and the lens', async ({ page }) => {
    await installAuditReads(page);
    await page.route(EXPORT, (r) =>
      r.fulfill({
        status: 200,
        headers: { 'content-type': 'application/x-ndjson', 'content-disposition': 'attachment; filename="audit.jsonl"' },
        body: `${JSON.stringify(ROWS[0])}\n`,
      }),
    );
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await page.getByLabel('Source').fill('jobs');
    await page.getByLabel('Kind contains').fill('step');
    await page.getByLabel('Actor').fill('agent-claude');
    await page.getByLabel('Provenance').selectOption('real');
    await page.getByRole('button', { name: 'Download ⤓' }).click();
    const panel = page.locator('.events-download-panel');
    await panel.getByLabel('From').fill('2026-09-01');
    await panel.getByLabel('To').fill('2026-09-03');
    const [req, download] = await Promise.all([
      page.waitForRequest(EXPORT),
      page.waitForEvent('download'),
      panel.getByRole('button', { name: 'Save .jsonl' }).click(),
    ]);
    expect(download.suggestedFilename()).toBe('audit.jsonl');
    const u = req.url();
    expect(param(u, 'source')).toBe('jobs');
    expect(param(u, 'kind')).toBe('step');
    // 03f79eca: the server applies the actor to the export as it does to
    // the tail (tail_http.rs, export_honours_the_actor_filter).
    expect(param(u, 'actor')).toBe('agent-claude');
    expect(param(u, 'since')).toBe('2026-09-01T00:00:00Z');
    // `until` is exclusive, so the To day is bumped by one.
    expect(param(u, 'until')).toBe('2026-09-04T00:00:00.000Z');
    // The lens rides the export, and the server applies it since
    // 34ea2ae0 (tail_http.rs, export_honours_the_provenance_lens).
    expect(param(u, 'simulated')).toBe('real');
    await expect(panel).toHaveCount(0);
    await expect(page.locator('h1').first()).toContainText('Audit Log');
    // The file is saved only once its body has been read to the end
    // (4630ebc0), and the line says what was saved.
    await expect(page.locator('.events-download-status')).toHaveText('Saved audit.jsonl: 1 event.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  // Backlog 4630ebc0: Save .jsonl was a window.location navigation, so a
  // refusal replaced the SPA with the raw body at the export URL, and a
  // failure after the 200 was reported nowhere. The export is now read
  // with fetch; the break-off-mid-stream leg is pinned in
  // src/it/monitoring/auditExport.test.ts, because a mocked route can
  // only fulfil a whole body.
  test('a REFUSED export is named beside the panel, and the app stays (4630ebc0)', async ({ page }) => {
    await installAuditReads(page);
    await page.route(EXPORT, (r) => r.fulfill({ status: 403, contentType: 'text/plain', body: 'forbidden: operator tier required' }));
    let downloads = 0;
    page.on('download', () => {
      downloads += 1;
    });
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await page.getByRole('button', { name: 'Download ⤓' }).click();
    const panel = page.locator('.events-download-panel');
    await panel.getByRole('button', { name: 'Save .jsonl' }).click();
    await expect(page.locator('.events-download-status')).toHaveText(
      'The export was refused: HTTP 403: forbidden: operator tier required. Nothing was saved.',
    );
    // The failed export is a failure line like the page's others
    // (c3e4edcc); a saved one stays a status (pinned above).
    await expect(page.locator(`.events-download-status${FAILURE_MARKER}[role="alert"]`)).toHaveCount(1);
    await expect(page).toHaveURL(new RegExp(`${PATH}$`));
    await expect(page.locator('h1').first()).toContainText('Audit Log');
    // The panel keeps its window open for a retry.
    await expect(panel).toBeVisible();
    expect(downloads).toBe(0);
  });

  test('an export whose connection drops is named, and saves nothing (4630ebc0)', async ({ page }) => {
    await installAuditReads(page);
    await page.route(EXPORT, (r) => r.abort('connectionreset'));
    let downloads = 0;
    page.on('download', () => {
      downloads += 1;
    });
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await page.getByRole('button', { name: 'Download ⤓' }).click();
    await page.locator('.events-download-panel').getByRole('button', { name: 'Save .jsonl' }).click();
    const line = page.locator('.events-download-status');
    await expect(line).toContainText('The export could not be read (');
    await expect(line).toContainText('Nothing was saved.');
    await expect(page).toHaveURL(new RegExp(`${PATH}$`));
    expect(downloads).toBe(0);
  });
});

test.describe('/it/operate/audit — writes', () => {
  test('the page issues no write: every control above is a read (0 writes in controls_md)', async ({ page }) => {
    await installAuditReads(page);
    const seen = watch(page);
    await mountPage(page, PATH, { titleMatch: /Audit Log/ });
    await stream(page).first().click();
    await page.getByLabel('Provenance').selectOption('sim');
    await page.getByRole('button', { name: 'Download ⤓' }).click();
    await page.locator('.events-download-panel').getByRole('button', { name: 'Cancel' }).click();
    await page.getByLabel('Live (SSE)').uncheck();
    await page.waitForTimeout(500);
    expect(seen.writes).toEqual([]);
  });
});
