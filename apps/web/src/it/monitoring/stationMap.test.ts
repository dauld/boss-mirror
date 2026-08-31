import { describe, expect, test } from 'bun:test';
import { protocolHue } from '@boss/web-kit/ui/packet-card';
import {
  disciplineLabel,
  toStationNode,
  withDepth,
  stationsStateFromResponse,
  toQueueCard,
  queueViewFromBody,
  type StationRow,
} from './stationMap';

function row(over: Partial<StationRow> = {}): StationRow {
  return {
    name: 'loading-dock',
    version: 1,
    title: 'Loading dock',
    kind: 'batch',
    discipline: ['priority', 'age'],
    wip_limit: null,
    ...over,
  };
}

describe('disciplineLabel', () => {
  test('renders the ratified phrasing: priority, then age', () => {
    expect(disciplineLabel(['priority', 'age'])).toBe('priority, then age');
    expect(disciplineLabel(['due'])).toBe('due');
    expect(disciplineLabel(['priority', 'age', 'due'])).toBe('priority, then age, then due');
  });
  test('an empty or missing discipline falls back to the registry default', () => {
    expect(disciplineLabel([])).toBe('priority, then age');
    expect(disciplineLabel(undefined)).toBe('priority, then age');
  });
});

describe('toStationNode', () => {
  test('maps a registry row to a node view model, depth unknown until polled', () => {
    const n = toStationNode(row());
    expect(n.name).toBe('loading-dock');
    expect(n.title).toBe('Loading dock');
    expect(n.kind).toBe('batch');
    expect(n.discipline).toBe('priority, then age');
    expect(n.depth).toBeNull();
    expect(n.overLimit).toBe(false);
    expect(n.wipLimit).toBeNull();
  });
  test('the kind chip is colored by protocolHue of the kind string — a new kind needs zero code', () => {
    expect(toStationNode(row({ kind: 'batch' })).hue).toBe(protocolHue('batch'));
    expect(toStationNode(row({ kind: 'some-future-kind' })).hue).toBe(
      protocolHue('some-future-kind'),
    );
  });
  test('a title-less row still renders — the name stands in', () => {
    const n = toStationNode(row({ title: '' }));
    expect(n.title).toBe('loading-dock');
  });
  test('wip_limit rides through', () => {
    expect(toStationNode(row({ wip_limit: 8 })).wipLimit).toBe(8);
  });
});

describe('withDepth', () => {
  test('folds a polled queue envelope into the node', () => {
    const n = withDepth(toStationNode(row()), { total: 12, over_limit: false });
    expect(n.depth).toBe(12);
    expect(n.overLimit).toBe(false);
  });
  test('over_limit is advisory state on the node, straight from the envelope', () => {
    const n = withDepth(toStationNode(row({ wip_limit: 3 })), { total: 5, over_limit: true });
    expect(n.depth).toBe(5);
    expect(n.overLimit).toBe(true);
  });
});

describe('stationsStateFromResponse', () => {
  test('404 means the registry has not reached this deployment — not an error', () => {
    expect(stationsStateFromResponse(404, null).kind).toBe('unavailable');
  });
  test('503 (registry not configured) reads the same way', () => {
    expect(stationsStateFromResponse(503, null).kind).toBe('unavailable');
  });
  test('other failures are errors', () => {
    expect(stationsStateFromResponse(500, null).kind).toBe('error');
    expect(stationsStateFromResponse(401, null).kind).toBe('error');
  });
  test('a 200 envelope yields ready nodes in registry order', () => {
    const s = stationsStateFromResponse(200, {
      data: [row(), row({ name: 'design-review', title: 'Design review', kind: 'group' })],
      total: 2,
    });
    expect(s.kind).toBe('ready');
    if (s.kind !== 'ready') throw new Error('unreachable');
    expect(s.nodes.map(n => n.name)).toEqual(['loading-dock', 'design-review']);
  });
  test('a 200 with no rows (or a bare-array body) is ready-and-empty, not a crash', () => {
    const empty = stationsStateFromResponse(200, { data: [], total: 0 });
    expect(empty.kind).toBe('ready');
    if (empty.kind !== 'ready') throw new Error('unreachable');
    expect(empty.nodes).toEqual([]);
    // The mocked-crawl catch-all answers `[]` for unknown endpoints.
    const bare = stationsStateFromResponse(200, []);
    expect(bare.kind).toBe('ready');
  });
});

describe('toQueueCard — the yard grammar for a station queue packet', () => {
  const job = {
    id: 'j1',
    kind: 'ship-a-change',
    title: 'A change',
    status: 'open',
    opened_on: '2026-08-13',
    tags: ['hotfix'],
    metadata: { branch: 'feat/x' },
  };
  test('protocol, title, tags and the mono provenance line carry through', () => {
    const c = toQueueCard(job);
    expect(c.id).toBe('j1');
    expect(c.kind).toBe('ship-a-change');
    expect(c.title).toBe('A change');
    expect(c.tags).toEqual(['hotfix']);
    expect(c.branch).toBe('feat/x');
    expect(c.sim).toBe(false);
  });
  test('a branchless packet falls back to its opened date as provenance', () => {
    expect(toQueueCard({ ...job, metadata: {} }).branch).toBe('opened 2026-08-13');
  });
  test('sim is a fact on the packet — the admission-fixed field or the tag fallback', () => {
    expect(toQueueCard({ ...job, simulated: true }).sim).toBe(true);
    expect(toQueueCard({ ...job, tags: ['sim'] }).sim).toBe(true);
  });
});

describe('queueViewFromBody', () => {
  const envelope = {
    station: 'loading-dock',
    kind: 'batch',
    discipline: ['priority', 'age'],
    wip_limit: 3,
    over_limit: true,
    total: 2,
    data: [
      { id: 'a', kind: 'ship-a-change', title: 'A', status: 'open', opened_on: '2026-08-12' },
      { id: 'b', kind: 'ship-a-change', title: 'B', status: 'open', opened_on: '2026-08-13' },
    ],
  };
  test('maps the envelope: server order preserved, discipline named, advisory verdict kept', () => {
    const v = queueViewFromBody(envelope);
    expect(v).not.toBeNull();
    expect(v!.station).toBe('loading-dock');
    expect(v!.discipline).toBe('priority, then age');
    expect(v!.wipLimit).toBe(3);
    expect(v!.overLimit).toBe(true);
    expect(v!.total).toBe(2);
    expect(v!.cards.map(c => c.id)).toEqual(['a', 'b']);
  });
  test('an envelope-shaped nothing (the mock catch-all, an error body) is null, not a crash', () => {
    expect(queueViewFromBody([])).toBeNull();
    expect(queueViewFromBody(null)).toBeNull();
    expect(queueViewFromBody({ nope: true })).toBeNull();
  });

  // The walk upstream reaches every station lens, not just the yard's
  // dock: the map's queue panel is where an operator lands when a node
  // reads shallower than expected.
  test('a declared upstream rides the envelope into the queue view', () => {
    const v = queueViewFromBody({
      ...envelope,
      upstream: { label: 'FEEDBACK', href: '/it/design/feedback' },
    });
    expect(v!.upstream).toEqual({
      label: '↑ UPSTREAM: FEEDBACK',
      href: '/it/design/feedback',
      title: 'Walk upstream to FEEDBACK — the queue that feeds this station',
    });
  });
  test('a station with no declared upstream gets no button', () => {
    expect(queueViewFromBody(envelope)!.upstream).toBeNull();
  });
});
