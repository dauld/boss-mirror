import { describe, expect, test } from 'bun:test';
import {
  FALLBACK_HEADER,
  KNOWN_PANELS,
  pageHeader,
  panelsFor,
  queueRows,
  reviewHref,
  type QueuePacket,
  type StationLens,
} from './designLens';

function packet(over: Partial<QueuePacket>): QueuePacket {
  return {
    id: 'j1',
    title: 'Review: something',
    status: 'open',
    opened_on: '2026-08-15',
    ...over,
  };
}

describe('pageHeader', () => {
  test('renders the header the station row declares', () => {
    const lens: StationLens = {
      eyebrow: 'System Model · Design review',
      title: 'Design review',
      subtitle: 'Open questions and ADRs',
    };
    expect(pageHeader(lens)).toEqual({
      eyebrow: 'System Model · Design review',
      title: 'Design review',
      subtitle: 'Open questions and ADRs',
    });
  });

  test('a cluster whose registry predates the column still names the page', () => {
    // The ordinary state mid-rollout: binary ahead of schema.
    expect(pageHeader(undefined)).toEqual(FALLBACK_HEADER);
    expect(pageHeader(null)).toEqual(FALLBACK_HEADER);
  });

  test('a blank title is treated as undeclared, not as an empty heading', () => {
    expect(pageHeader({ title: '   ' })).toEqual(FALLBACK_HEADER);
  });

  test('a lens may decline a subtitle without losing its eyebrow', () => {
    const h = pageHeader({ title: 'Night review' });
    expect(h.title).toBe('Night review');
    expect(h.subtitle).toBe('');
    expect(h.eyebrow).toBe(FALLBACK_HEADER.eyebrow);
  });
});

describe('panelsFor', () => {
  test('renders the panels the row declares, in its order', () => {
    expect(panelsFor({ title: 't', panels: ['queue'] })).toEqual(['queue']);
  });

  test('skips a key this build does not know rather than blanking the page', () => {
    // The registry runs ahead of the bundle during a rollout. A page
    // that throws on an unpublished panel key fails exactly when
    // someone is publishing one. `rejections` and `corpus` are the
    // REAL cases: both panels were deleted on 2026-09-10, and a row
    // that still declares them alongside a key this build ships must
    // render that key.
    expect(panelsFor({ title: 't', panels: ['rejections', 'queue', 'corpus'] })).toEqual([
      'queue',
    ]);
  });

  test('no lens keeps the whole page', () => {
    expect(panelsFor(undefined)).toEqual(KNOWN_PANELS);
    expect(panelsFor({ title: 't' })).toEqual(KNOWN_PANELS);
    expect(panelsFor({ title: 't', panels: [] })).toEqual(KNOWN_PANELS);
  });

  test('a row declaring ONLY unknown panels falls back rather than blanking', () => {
    // The live state on 2026-09-10: the row said `["corpus"]` and the
    // tree's seed said `["rejections", "corpus"]`, and this build
    // ships neither. Filtering to nothing would render a header over
    // blank space for every reader until the migration ran. Declaring
    // nothing and declaring only unknowns are the same state from the
    // renderer's side — no honourable instruction — so they fall back
    // the same way.
    expect(panelsFor({ title: 't', panels: ['corpus'] })).toEqual(KNOWN_PANELS);
    expect(panelsFor({ title: 't', panels: ['rejections', 'corpus'] })).toEqual(KNOWN_PANELS);
  });
});

describe('queueRows', () => {
  test('renders every packet the station handed over, in its order', () => {
    const rows = queueRows([packet({ id: 'first' }), packet({ id: 'second' })]);
    expect(rows.map((r) => r.id)).toEqual(['first', 'second']);
  });

  test('every packet in the queue becomes a row', () => {
    // This is the regression the panel rename fixed. The old
    // `reviewsByDocPath` keyed rows by `subject.id` expecting a doc
    // path, and EVERY `design-doc` packet carries the literal
    // `boss-platform` its Workflow stamps — so the map collapsed the
    // whole queue to one entry, and a packet with no subject id was
    // dropped outright. There is no key to join on; a row is a row.
    const rows = queueRows([packet({ id: 'a' }), packet({ id: 'b' }), packet({ id: 'c' })]);
    expect(rows.map((r) => r.id)).toEqual(['a', 'b', 'c']);
  });

  test('an empty queue renders no rows rather than throwing', () => {
    expect(queueRows([])).toEqual([]);
  });
});

describe('reviewHref', () => {
  test('goes to the full-page step surface when the step is known', () => {
    const href = reviewHref('job-1', 'step-9');
    expect(href).toStartWith('/jobs/job-1/steps/step-9?');
    expect(href).toContain('from=%2Fit%2Fdesign');
    expect(href).toContain('from_label=Design%20Review');
  });

  test('falls back to the job page for a packet whose steps have not materialized', () => {
    expect(reviewHref('job-1')).toBe('/service/job-1');
    expect(reviewHref('job-1', null)).toBe('/service/job-1');
  });
});
