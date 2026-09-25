import { describe, expect, it } from 'bun:test';
import * as borders from './borders';
import { hudOf, labelOf, machineHref, type Figure, type HudRow } from './hud';
import { parseRegions, type Regions } from './regions';

// THE HUD FRAME (design 00774ca8). What these pin, each named by the
// design's car plan: the row order and membership come from the payload;
// the delivery row reads `arrivals.trend` and never recomputes it; each
// of the four pictures — value, zero, floor, unread — renders as itself;
// a failed read turns every cell unread and keeps no stale value; and
// the client sum it replaces is gone.

const trend = (current: number | null, previous: number | null) => ({
  metric: 'arrivals', unit: 'per day', current, previous, samples: 5, previous_samples: 4,
});

const region = (name: string, t = trend(1, 1)) => ({ name, count: 0, state: 'clear', why: '', trend: t });

const balance = (inn: number | null, out: number | null, net: number | null, why?: string) => ({
  unit: 'packets', in_means: 'a packet opened', out_means: 'a packet closed',
  in: inn, out, net, in_count: inn, out_count: out, ...(why ? { why } : {}),
});

const stuck = (n: number, waiting: number, unknown: string[] = [], oldest: number | null = null) => ({
  third: 'x', stuck: n, waiting, unknown, oldest_hours: oldest, regions: [],
});

/** A payload whose thirds arrive in an order and with names this client
 *  has never seen — the HUD must follow them, not a list of its own. */
const PAYLOAD = {
  window_hours: 24,
  now: '2026-09-24T12:00:00Z',
  regions: [region('receiving'), region('arrivals', trend(17, 12)), region('gates')],
  thirds: [
    { third: 'delivery', regions: ['dock', 'arrivals'], balance: balance(10, 12, -2), stuck: stuck(2, 3, [], 170) },
    { third: 'queue-management', regions: ['receiving'], balance: balance(40, 28, 12), stuck: stuck(0, 0, ['station q: the flow cube is blind']) },
    {
      third: 'actors-building', regions: ['gates'],
      balance: balance(null, null, null, 'the agent-run packets could not be read'),
      stuck: stuck(0, 0),
    },
  ],
  machines: {
    running: 12, idle: 11, failed: 0, unknown: 4, total: 27,
    failed_or_unknown: [{ region: 'marshalling', id: 'station:x', name: 'x', state: 'unknown', why: 'blind' }],
  },
};

const NOW = Date.parse('2026-09-24T12:00:04Z');
const READ_AT = Date.parse('2026-09-24T12:00:00Z');
const data = (): Regions => parseRegions(PAYLOAD);
const ready = () => hudOf({ kind: 'ready', data: data() }, READ_AT, { at: READ_AT, data: data() }, NOW);
const row = (rows: ReadonlyArray<HudRow>, third: string): HudRow => {
  const r = rows.find((x) => x.third === third);
  if (!r) throw new Error(`no row ${third}`);
  return r;
};

describe('the rows are the payload’s', () => {
  it('in the order the server sent, with the membership it sent', () => {
    const hud = ready();
    expect(hud.rows.map((r) => r.third)).toEqual(['delivery', 'queue-management', 'actors-building']);
    expect(row(hud.rows, 'delivery').regions).toEqual(['dock', 'arrivals']);
    expect(hud.rows.map((r) => r.label)).toEqual(['Delivery', 'Queue management', 'Actors building']);
    expect(labelOf('a-new-third')).toBe('A new third');
  });

  it('prints the server’s net and the two rates it is the difference of — adding nothing up', () => {
    const q = row(ready().rows, 'queue-management');
    expect(q.net).toEqual({ kind: 'value', text: '+12/day', troubled: false });
    expect(q.into).toEqual({ kind: 'value', text: '40', troubled: false });
    expect(q.out).toEqual({ kind: 'value', text: '28', troubled: false });
    expect(q.unit).toBe('packets');
    expect(row(ready().rows, 'delivery').net).toEqual({ kind: 'value', text: '−2/day', troubled: false });
    // The net is the server's field: a payload whose net disagrees with
    // its halves is printed as sent, which is how a test can tell the
    // HUD never subtracts.
    const skew = { ...PAYLOAD, thirds: [{ ...PAYLOAD.thirds[1]!, balance: balance(40, 28, 99) }] };
    expect(hudOf({ kind: 'ready', data: parseRegions(skew) }, READ_AT, null, NOW).rows[0]!.net).toEqual({
      kind: 'value', text: '+99/day', troubled: false,
    });
  });
});

describe('the delivery row reads arrivals.trend — decision 4’s one exception', () => {
  it('shows the arrivals territory’s own field, on the row that owns arrivals only', () => {
    const d = data();
    const hud = hudOf({ kind: 'ready', data: d }, READ_AT, null, NOW);
    const arrivals = d.regions.find((r) => r.name === 'arrivals')!.trend;
    expect(row(hud.rows, 'delivery').arrivals).toEqual({
      current: { kind: 'value', text: String(arrivals.current), troubled: false },
      previous: { kind: 'value', text: String(arrivals.previous), troubled: false },
    });
    expect(row(hud.rows, 'queue-management').arrivals).toBeNull();
    // A trend nobody measured is unread, not zero.
    const blind = { ...PAYLOAD, regions: [region('arrivals', trend(null, null))] };
    const b = row(hudOf({ kind: 'ready', data: parseRegions(blind) }, READ_AT, null, NOW).rows, 'delivery');
    expect(b.arrivals?.current.kind).toBe('unread');
  });
});

describe('the four pictures', () => {
  const kinds = (f: Figure) => f.kind;

  it('a value is plain, and a stuck count above zero is troubled', () => {
    const d = row(ready().rows, 'delivery');
    expect(d.stuck).toEqual({ kind: 'value', text: '2', troubled: true });
    expect(d.waiting).toEqual({ kind: 'value', text: '3', troubled: false });
    expect(d.oldest).toBe('oldest 7d');
  });

  it('a true zero is zero — no mark', () => {
    const a = row(ready().rows, 'actors-building');
    expect(a.stuck).toEqual({ kind: 'zero' });
    expect(a.waiting).toEqual({ kind: 'zero' });
    expect(ready().machines.failed).toEqual({ kind: 'zero' });
  });

  it('a floor is ≥ n, with what could not be counted', () => {
    const q = row(ready().rows, 'queue-management');
    expect(q.stuck).toEqual({ kind: 'floor', text: '≥ 0', troubled: false, why: 'station q: the flow cube is blind' });
  });

  it('an unread input is unread, with its reason — never 0, never blank', () => {
    const a = row(ready().rows, 'actors-building');
    expect([a.net, a.into, a.out].map(kinds)).toEqual(['unread', 'unread', 'unread']);
    expect(a.net).toEqual({ kind: 'unread', why: 'the agent-run packets could not be read' });
  });

  it('the machine cell is the server’s count, with the unjudged listed by region', () => {
    const m = ready().machines;
    expect(m.unjudged).toEqual({ kind: 'value', text: '4', troubled: false });
    expect(m.total).toEqual({ kind: 'value', text: '27', troubled: false });
    expect(m.listed.map((x) => `${x.region}/${x.id}`)).toEqual(['marshalling/station:x']);
    const failing = { ...PAYLOAD, machines: { ...PAYLOAD.machines, failed: 1 } };
    expect(hudOf({ kind: 'ready', data: parseRegions(failing) }, READ_AT, null, NOW).machines.failed).toEqual({
      kind: 'value', text: '1', troubled: true,
    });
  });
});

describe('the header and a failed read', () => {
  it('carries the read’s age and the window', () => {
    expect(ready().header).toBe('read 4s ago, window 24h');
    expect(ready().failed).toBe(false);
  });

  it('turns every cell unread, keeps the rows’ names and says when the last good read was', () => {
    const failedAt = Date.parse('2026-09-24T12:05:00Z');
    const hud = hudOf(
      { kind: 'failed', error: 'HTTP 502' },
      failedAt,
      { at: READ_AT, data: data() },
      failedAt,
    );
    expect(hud.failed).toBe(true);
    expect(hud.header).toBe('read failed 12:05Z · last good 12:00Z');
    expect(hud.rows.map((r) => r.third)).toEqual(['delivery', 'queue-management', 'actors-building']);
    for (const r of hud.rows) {
      for (const f of [r.net, r.into, r.out, r.stuck, r.waiting]) expect(f.kind).toBe('unread');
      expect(r.oldest).toBeNull();
    }
    // No stale value survives anywhere in the frame.
    expect(JSON.stringify(hud)).not.toContain('"value"');
    expect(hud.machines.failed.kind).toBe('unread');
  });

  it('before the first read lands it is reading — unknown, and not drawn as a failure', () => {
    const hud = hudOf({ kind: 'loading' }, null, null, NOW);
    expect(hud.read).toBe('loading');
    expect(hud.header).toBe('reading…');
    expect(hud.rows.every((r) => r.net.kind === 'unread')).toBe(true);
  });

  it('with no good read yet, the frame still stands at three rows of unknowns', () => {
    const hud = hudOf({ kind: 'failed', error: 'down' }, READ_AT, null, NOW);
    expect(hud.read).toBe('failed');
    expect(hud.header).toBe('read failed 12:00Z · no good read yet');
    expect(hud.rows).toHaveLength(3);
    expect(hud.rows.every((r) => r.stuck.kind === 'unread')).toBe(true);
  });

  it('an older server without the block is unanswered, never balanced', () => {
    const older = parseRegions({ window_hours: 24, regions: [] });
    const hud = hudOf({ kind: 'ready', data: older }, READ_AT, null, NOW);
    expect(hud.rows).toHaveLength(3);
    expect(hud.rows[0]!.net).toEqual({ kind: 'unread', why: 'the server sends no thirds block' });
    expect(hud.machines.total).toEqual({ kind: 'unread', why: 'the server sends no machine count' });
  });
});

describe('what the frame retires (decision 10)', () => {
  it('the client-side border sum is gone', () => {
    expect('summaryLine' in borders).toBe(false);
  });
});

// The host runners left receiving for the PLANT (62de32ae decision 11),
// and the server counts them in the machine cell under `plant`. The
// plant strip stands under the world map, so a listed plant machine
// opens the world — there is no /it/yard/plant to open.
describe('a listed machine opens where it is drawn', () => {
  it('a region machine opens its region, a plant machine the world', () => {
    const at = (region: string) => ({ region, id: 'x', name: 'x', state: 'failed' as const, why: '' });
    // A selection on the Department Map (design e765b3fc, car N1).
    expect(machineHref(at('marshalling'))).toBe('/it?at=marshalling');
    expect(machineHref(at('plant'))).toBe('/it');
  });
});
