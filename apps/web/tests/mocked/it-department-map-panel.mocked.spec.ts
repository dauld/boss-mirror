// THE SELECTION'S PANEL — design e765b3fc, car N2 (decided by David
// 2026-09-25, on feedback 84cba7e2).
//
// "I want to put more of that data behind a map selection for display at
// the bottom. So I can click a line segment and see info on how it is
// performing, but we don't need to keep it all on the main map"
// (added_2026_09_25_david_2). The car's plan names its test: a mocked
// spec per panel field, and a DOM assertion that the map carries no
// hold, machine or headway text. This pins, on the transit map (the map
// the design builds on, behind it-map-transit):
//
//  - a station's panel carries every field — rate, waiting by class,
//    stuck, trend, the verdict and its why, machines, crossings — and the
//    floor's cards, which used to be the region's own page;
//  - a SECTION is a selection (`?at=dock->track`), reached by a click on
//    the track, with the same fields for that one border;
//  - what is selected is MARKED on the map, and only that;
//  - the map itself writes none of it.

import { expect, test, type Page, type Route } from '@playwright/test';
import { STATIONS } from '../../src/it/yard/transit';
import { BORDERS, TERRITORIES } from '../../src/it/yard/world';
import { YARD_BORDERS, YARD_REGIONS, installSmokeMocks } from './_smokeMocks';

const FLIGHTS = /\/api\/flights\/mine(\?|$)/;
const NOW = '2026-09-25T17:21:00Z';

const json = (r: Route, b: unknown): Promise<void> =>
  r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(b) });

/** Words that belong to the panel and must never be written on the map. */
const MACHINE = 'auto-park-on-gate-green';
const HOLD_WHAT = 'feat/a-car-in-line';
const HOLD_WHY = 'in line for one of 3 bays';
const FLOWING_WHY = 'last crossed 19m ago, inside 4 mean gaps';
const GATES_WHY = '3 of 3 bays in use, 3 runs waiting for a slot';

/** Every region clear with one thing in it, but the gates at attention
 *  with a band, a KPI and a machine. */
const regions = () => ({
  window_hours: 24,
  now: NOW,
  regions: TERRITORIES.map(({ name }) =>
    name === 'gates'
      ? {
          name, count: 3, bound: 3, bound_kind: 'capacity', unit: 'bays in use', state: 'attention', why: GATES_WHY,
          band: { id: 'gates-at-bound', reads: 'held 31m > the 30m bound', hold_minutes: 30, since: '2026-09-25T16:50:00Z', held_minutes: 31, held: '31m' },
          trend: { metric: 'gate duration', unit: 'minutes', current: 11, previous: 9, samples: 5, previous_samples: 4 },
          kpi: [{ name: 'bays', value: 3, unit: 'bays', text: '3 of 3 bays in use' }],
          machines: [{ id: 'gate-bay-1', name: 'bay 1', state: 'running', why: 'a gate-run holds it' }],
        }
      : {
          name, count: 1, unit: 'things', state: 'clear', why: 'fine',
          trend: { metric: 'crossings', unit: 'per day', current: 4, previous: 4, samples: 4, previous_samples: 4 },
        },
  ),
});

/** Every section flowing at 202 a day with one hold named of three
 *  waiting, and gates→dock with a stuck car. */
const borders = () => ({
  window_hours: 24,
  now: NOW,
  borders: BORDERS.map(({ from, to }) => ({
    from, to, crossing: `a packet crossed from ${from} to ${to}`, state: 'clear', why: 'flowing',
    rate: { metric: 'crossings', unit: 'per day', current: 202, previous: 180, samples: 202, previous_samples: 180 },
    last_crossed: '2026-09-25T17:02:00Z', waiting: 3,
    holds: [{ what: HOLD_WHAT, why: HOLD_WHY }],
    holds_by_class: from === 'gates' && to === 'dock'
      ? { machine: 1, person: 1, unknown: 0, stuck: 1 }
      : { machine: 3, person: 0, unknown: 0, stuck: 0 },
    flowing: true, held_since: null, flowing_why: FLOWING_WHY,
    machine: { name: MACHINE, kind: 'dispatcher-rule', last_fired: '2026-09-25T17:14:00Z', silent_for_minutes: 7,
      expected_every_minutes: null, silent: false, why: 'fired on the last green' },
  })),
});

async function mocks(page: Page, over: Readonly<{ borders?: unknown }> = {}): Promise<void> {
  await installSmokeMocks(page);
  await page.route(FLIGHTS, (r) => json(r, { flights: ['it-map-transit'] }));
  await page.route(YARD_REGIONS, (r) => json(r, regions()));
  await page.route(YARD_BORDERS, (r) => ('borders' in over ? r.fulfill(over.borders as never) : json(r, borders())));
}

const MAP = 'section.transit[data-transit]';
const SVG = `${MAP} .board > svg`;
const PANEL = 'section[data-map-panel]';
const field = (page: Page, f: string) => page.locator(`${PANEL} [data-field="${f}"] li`);

test('a station\'s panel carries every field, and its floor', async ({ page }) => {
  await mocks(page);
  await page.goto('/it?at=gates');
  const panel = page.locator(PANEL);
  await expect(panel).toHaveAttribute('data-kind', 'station');
  await expect(panel).toHaveAttribute('data-state', 'attention');
  await expect(panel.locator('[data-field]')).toHaveCount(7);

  // Rate: each section in and out, a day's crossings and the headway.
  await expect(field(page, 'rate')).toHaveText(['in from shop floor: 202/d · 7m gap', 'out to dock: 202/d · 7m gap', 'out to garage: 202/d · 7m gap']);
  // Waiting, by whom it waits on, with the holds the server named and
  // the count it did not list.
  await expect(field(page, 'waiting').first()).toHaveText('in from shop floor: 3 waiting — 3 on a machine');
  await expect(field(page, 'waiting').nth(1)).toHaveText(`${HOLD_WHAT} — ${HOLD_WHY}`);
  await expect(field(page, 'waiting').nth(2)).toHaveText('+2 more waiting, not listed');
  // Stuck: the stuck block's own members, where they stand.
  await expect(field(page, 'stuck')).toHaveText(['out to dock: 1 stuck']);
  // Trend: the region's metric against the window before, and its KPI.
  await expect(field(page, 'trend')).toHaveText(['gate duration: 11 vs 9 minutes · n=5 / 4', '3 of 3 bays in use']);
  // The verdict: the state with how long, the band that decided it, the why.
  await expect(field(page, 'verdict')).toHaveText(['attention for 31m', 'held 31m > the 30m bound', GATES_WHY]);
  await expect(field(page, 'machines')).toHaveText(['bay 1 · running — a gate-run holds it']);
  // Recent crossings: the last of each section — and the per-packet list
  // is said to be not kept yet, never drawn as an empty one.
  await expect(field(page, 'crossings').first()).toHaveText('in from shop floor: 2026-09-25 17:02 UTC · 19m ago');
  await expect(field(page, 'crossings').last()).toContainText('does not yet');

  // The floor's cards are in the panel: the region's page is no longer
  // the only place they are drawn.
  await expect(panel.locator('[data-contents="gates"]')).toHaveCount(1);
  await expect(panel.locator('[data-contents="gates"] [data-alerts="gates"]')).toHaveCount(1);
});

test('a section is a selection: a click on the track opens its panel, and the map stays on top', async ({ page }) => {
  // Nothing moves, so the click lands on the track and not on a train.
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await mocks(page);
  await page.goto('/it');
  const node = await page.locator(SVG).elementHandle();

  // A level section's geometry is a line with no height, so the browser
  // reports its box as zero-high and Playwright calls it invisible; the
  // wide unpainted stroke still takes a real pointer. Click where a
  // person would: the middle of the track.
  const box = (await page.locator(`${MAP} [data-section-link="gates→dock"]`).boundingBox())!;
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  await expect(page).toHaveURL(/\/it\?at=gates-(%3E|>)dock$/);
  const panel = page.locator(PANEL);
  await expect(panel).toHaveAttribute('data-kind', 'section');
  await expect(panel).toHaveAttribute('data-selection', 'gates->dock');
  await expect(panel.locator('.panel-title')).toHaveText('Gates → Dock');
  expect(await node!.evaluate((el) => el.isConnected)).toBe(true);

  await expect(field(page, 'crossing')).toHaveText(['a packet crossed from gates to dock']);
  await expect(field(page, 'rate')).toHaveText(['202/d · 7m gap']);
  await expect(field(page, 'waiting').first()).toHaveText('3 waiting — 1 on a machine · 1 on a person or the world · 1 stuck');
  await expect(field(page, 'stuck')).toHaveText(['1 stuck']);
  await expect(field(page, 'trend')).toHaveText(['202 vs 180 /day · n=202 / 180']);
  await expect(field(page, 'verdict')).toHaveText(['clear', 'flowing', FLOWING_WHY]);
  await expect(field(page, 'machines')).toHaveText([`${MACHINE} · fired 7m ago`, 'fired on the last green']);
  await expect(field(page, 'crossings').first()).toHaveText('2026-09-25 17:02 UTC · 19m ago');
  // A section has no floor of its own.
  await expect(panel.locator('[data-contents]')).toHaveCount(0);
});

test('the design\'s arrow selects the same section', async ({ page }) => {
  await mocks(page);
  await page.goto(`/it?at=${encodeURIComponent('dock→track')}`);
  await expect(page.locator(PANEL)).toHaveAttribute('data-selection', 'dock->track');
});

test('what is selected is marked on the map, and only that', async ({ page }) => {
  await mocks(page);
  await page.goto('/it?at=gates');
  await expect(page.locator(`${SVG} [data-selected-mark]`)).toHaveCount(1);
  await expect(page.locator(`${SVG} [data-selected-mark="gates"]`)).toHaveCount(1);
  await expect(page.locator(`${SVG} [data-station="gates"]`)).toHaveAttribute('aria-current', 'true');
  await expect(page.locator(`${SVG} [aria-current]`)).toHaveCount(1);
  // The mark is apart from the state: the gates' own ring still reads its state.
  await expect(page.locator(`${SVG} [data-station="gates"]`)).toHaveAttribute('data-state', 'attention');

  await page.goto('/it?at=dock-%3Etrack');
  await expect(page.locator(`${SVG} [data-selected-mark]`)).toHaveCount(1);
  await expect(page.locator(`${SVG} [data-selected-mark="dock→track"]`)).toHaveCount(1);
  await expect(page.locator(`${SVG} [data-section-link="dock→track"]`)).toHaveAttribute('aria-current', 'true');
  await expect(page.locator(`${SVG} [data-station] [data-selected-mark]`)).toHaveCount(0);

  // Nothing selected, nothing marked.
  await page.goto('/it');
  await expect(page.locator(`${SVG} [data-station]`)).toHaveCount(STATIONS.length);
  await expect(page.locator(`${SVG} [data-selected-mark]`)).toHaveCount(0);
  await expect(page.locator(`${SVG} [aria-current]`)).toHaveCount(0);
});

test('the map carries no hold, machine or headway text — that is the panel\'s', async ({ page }) => {
  await mocks(page);
  await page.goto('/it?at=gates');
  await expect(page.locator(`${PANEL} [data-field="rate"]`)).toBeVisible();
  await expect(page.locator(`${SVG} [data-station]`)).toHaveCount(STATIONS.length);
  await expect(page.locator(`${SVG} [data-headway]`)).toHaveCount(0);
  // Every word the SVG holds — visible text, hover titles and labels.
  const words = await page.locator(SVG).evaluate((svg) =>
    [svg.textContent ?? '', ...[...svg.querySelectorAll('[aria-label]')].map((e) => e.getAttribute('aria-label') ?? '')].join('\n'),
  );
  for (const panelOnly of [MACHINE, HOLD_WHAT, HOLD_WHY, FLOWING_WHY, GATES_WHY, '/d', 'gap', 'fired']) {
    expect(words, panelOnly).not.toContain(panelOnly);
  }
  // What the map DOES carry: the names and the one number.
  expect(words).toContain('gates');
  expect(words).toContain('3 / 3 bays in use');
});

test('a section selected while the rails cannot be read says no reading, never that it does not exist', async ({ page }) => {
  await mocks(page, { borders: { status: 503, contentType: 'text/plain', body: 'down' } });
  await page.goto('/it?at=dock-%3Etrack');
  const panel = page.locator(PANEL);
  await expect(panel).toHaveAttribute('data-kind', 'section');
  await expect(panel).not.toContainText('Nothing on this map is named');
  await expect(field(page, 'rate')).toHaveText(['no reading — the sections could not be read']);
});
