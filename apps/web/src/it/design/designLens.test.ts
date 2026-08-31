import { describe, expect, test } from 'bun:test';
import {
  FALLBACK_HEADER,
  KNOWN_PANELS,
  pageHeader,
  panelsFor,
  reviewHref,
  reviewsByDocPath,
  type QueuePacket,
  type StationLens,
} from './designLens';

function packet(over: Partial<QueuePacket>): QueuePacket {
  return {
    id: 'j1',
    title: 'Review: something',
    status: 'open',
    opened_on: '2026-08-15',
    subject: { id: 'docs/design/a.md' },
    ...over,
  };
}

describe('pageHeader', () => {
  test('renders the header the station row declares', () => {
    const lens: StationLens = {
      eyebrow: 'System Model · Design review',
      title: 'Design review',
      subtitle: 'Open questions, pending decisions, ADRs',
    };
    expect(pageHeader(lens)).toEqual({
      eyebrow: 'System Model · Design review',
      title: 'Design review',
      subtitle: 'Open questions, pending decisions, ADRs',
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
    expect(panelsFor({ title: 't', panels: ['corpus', 'rejections'] })).toEqual([
      'corpus',
      'rejections',
    ]);
  });

  test('skips a key this build does not know rather than blanking the page', () => {
    // The registry runs ahead of the bundle during a rollout. A page
    // that throws on an unpublished panel key fails exactly when
    // someone is publishing one.
    expect(panelsFor({ title: 't', panels: ['rejections', 'flow-strip'] })).toEqual([
      'rejections',
    ]);
  });

  test('no lens keeps the whole page', () => {
    expect(panelsFor(undefined)).toEqual(KNOWN_PANELS);
    expect(panelsFor({ title: 't' })).toEqual(KNOWN_PANELS);
    expect(panelsFor({ title: 't', panels: [] })).toEqual(KNOWN_PANELS);
  });

  test('a row declaring only unknown panels renders none of them', () => {
    // Distinct from declaring nothing: the row DID choose, this build
    // just cannot honour the choice.
    expect(panelsFor({ title: 't', panels: ['flow-strip'] })).toEqual([]);
  });
});

describe('reviewsByDocPath', () => {
  test('keys packets by the doc path in their subject', () => {
    const by = reviewsByDocPath([
      packet({ id: 'a', subject: { id: 'docs/design/a.md' } }),
      packet({ id: 'b', subject: { id: 'docs/design/b.md' } }),
    ]);
    expect(by['docs/design/a.md']?.id).toBe('a');
    expect(by['docs/design/b.md']?.id).toBe('b');
  });

  test('drops a packet with no subject id — it is a review of nothing', () => {
    const by = reviewsByDocPath([packet({ id: 'a', subject: null }), packet({ id: 'b' })]);
    expect(Object.keys(by)).toEqual(['docs/design/a.md']);
    expect(by['docs/design/a.md']?.id).toBe('b');
  });

  test('two packets for one doc resolve to the one the station hands out first', () => {
    // The envelope arrives in the station's discipline order, so first
    // wins is the station's answer, not the loop's.
    const by = reviewsByDocPath([packet({ id: 'first' }), packet({ id: 'second' })]);
    expect(by['docs/design/a.md']?.id).toBe('first');
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
