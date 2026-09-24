import { describe, expect, test } from 'bun:test';
import {
  DECIDED_STATION,
  FALLBACK_HEADER,
  KNOWN_PANELS,
  decidedRows,
  foldLabel,
  pageHeader,
  panelsFor,
  progressLabel,
  queueRows,
  reviewHref,
  reviewProgress,
  type LensStep,
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
    expect(panelsFor({ title: 't', panels: ['queue', 'decided'] })).toEqual(['queue', 'decided']);
  });

  test('ships the decided panel — the page is IN, WORKING and OUT', () => {
    // design-review v2 declares ["queue", "decided"] (backlog 08372fdb).
    expect(KNOWN_PANELS).toContain('decided');
    expect(DECIDED_STATION).toBe('design-decided');
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

// The review step as the station envelope carries it, with the shape the
// design-doc Workflow gives it: `questions` stamped from the packet,
// `resolutions` written by the review surface on Save or Done.
function review(over: Partial<LensStep> & { questions?: unknown; resolutions?: unknown }): LensStep {
  const { questions, resolutions, ...rest } = over;
  return {
    id: 'rs1',
    kind: 'review-design',
    spec_slug: 'review',
    status: 'ready',
    metadata: { questions: questions ?? [], resolutions: resolutions ?? [] },
    ...rest,
  };
}

const Q3 = [
  { anchor: 'Q1', title: 'a', proposal: 'p' },
  { anchor: 'Q2', title: 'b', proposal: 'p' },
  { anchor: 'Q3', title: 'c', proposal: 'p' },
];

describe('reviewProgress — a saved review must not look untouched', () => {
  test('a review with nothing saved is not started', () => {
    expect(reviewProgress(review({ questions: Q3 }))).toEqual({ kind: 'untouched', asked: 3 });
  });

  test('saved answers on a READY step read as saved — Save moves only a pending step', () => {
    // Measured 2026-09-24: every open review sat at `ready`, and the
    // surface's Save flips a step only from `pending`. So status alone
    // cannot tell a half-made decision from an untouched one; the
    // answers on the step can.
    const step = review({
      questions: Q3,
      resolutions: [
        { anchor: 'Q1', decision: 'yes' },
        { anchor: 'Q3', decision: 'no, because' },
      ],
    });
    expect(reviewProgress(step)).toEqual({ kind: 'saved', answered: 2, asked: 3 });
    expect(progressLabel(reviewProgress(step))).toBe('saved · 2 of 3 answered');
  });

  test('a blank decision, or one for no question asked, is not an answer', () => {
    const step = review({
      questions: Q3,
      resolutions: [
        { anchor: 'Q1', decision: '   ' },
        { anchor: 'Q9', decision: 'stray' },
      ],
    });
    expect(reviewProgress(step)).toEqual({ kind: 'untouched', asked: 3 });
  });

  test('an ACTIVE review is work in hand even with no answer yet', () => {
    expect(reviewProgress(review({ questions: Q3, status: 'active' }))).toEqual({
      kind: 'saved',
      answered: 0,
      asked: 3,
    });
    expect(progressLabel(reviewProgress(review({ status: 'active' })))).toBe(
      'opened · nothing asked',
    );
  });

  test('no steps on the wire is said, not guessed', () => {
    // A registry still at design-review v1 (with_steps false) carries
    // none, and "not started" would be a claim the page cannot make.
    expect(reviewProgress(undefined)).toEqual({ kind: 'unread' });
    expect(progressLabel({ kind: 'unread' })).toBe('—');
  });

  test('labels', () => {
    expect(progressLabel({ kind: 'untouched', asked: 3 })).toBe('not started · 3 questions');
    expect(progressLabel({ kind: 'untouched', asked: 1 })).toBe('not started · 1 question');
    expect(progressLabel({ kind: 'untouched', asked: 0 })).toBe('not started · nothing asked');
  });
});

describe('queueRows with steps', () => {
  test('each row carries its review progress and its review step id', () => {
    const rows = queueRows([packet({ id: 'a' }), packet({ id: 'b' })], {
      a: [
        { id: 'd1', kind: 'trigger', spec_slug: 'drafted', status: 'completed' },
        review({ id: 'ra', questions: Q3, resolutions: [{ anchor: 'Q2', decision: 'ok' }] }),
      ],
    });
    expect(rows[0]!.progress).toEqual({ kind: 'saved', answered: 1, asked: 3 });
    expect(rows[0]!.reviewStepId).toBe('ra');
    // `b` has no steps on the wire: unread, and the Review button
    // resolves its step at click time as it always did.
    expect(rows[1]!.progress).toEqual({ kind: 'unread' });
    expect(rows[1]!.reviewStepId).toBeNull();
  });
});

describe('decidedRows — WORKING and OUT', () => {
  const fold = (status: string, folded_into?: string): LensStep => ({
    id: 'f',
    kind: 'task',
    spec_slug: 'fold',
    status,
    metadata: folded_into ? { folded_into } : {},
  });
  const decided = (on: string): LensStep => review({ status: 'completed', completed_on: on });

  test('open packets are folding, closed ones are settled, each in the station order', () => {
    const { working, settled } = decidedRows({
      station: DECIDED_STATION,
      discipline: ['recency'],
      terminal_window_days: 7,
      total: 3,
      data: [
        packet({ id: 'x', status: 'closed', closed_on: '2026-09-23', metadata: { outcome: 'published' } }),
        packet({ id: 'y', status: 'open' }),
        packet({ id: 'z', status: 'closed', closed_on: '2026-09-20', metadata: { outcome: 'published' } }),
      ],
      steps: {
        x: [decided('2026-09-22'), fold('completed', 'docs/architecture-decisions.md §Stations')],
        y: [decided('2026-09-24'), fold('active')],
      },
    });
    expect(working.map((r) => r.id)).toEqual(['y']);
    expect(working[0]!.decided_on).toBe('2026-09-24');
    expect(working[0]!.fold_status).toBe('active');
    expect(settled.map((r) => r.id)).toEqual(['x', 'z']);
    expect(settled[0]!.outcome).toBe('published');
    expect(settled[0]!.folded_into).toBe('docs/architecture-decisions.md §Stations');
    expect(settled[0]!.closed_on).toBe('2026-09-23');
    // z's steps were not on the wire: nothing invented for it.
    expect(settled[1]!.folded_into).toBeNull();
    expect(settled[1]!.decided_on).toBeNull();
  });

  test('a body that is not an envelope yields no rows rather than throwing', () => {
    // The mocked catch-all answers `[]`; an old gateway could answer anything.
    expect(decidedRows([] as unknown)).toEqual({ working: [], settled: [] });
    expect(decidedRows(null)).toEqual({ working: [], settled: [] });
  });

  test('fold labels say where the fold has got to', () => {
    expect(foldLabel('ready')).toBe('waiting for a builder');
    expect(foldLabel('active')).toBe('being folded');
    expect(foldLabel(null)).toBe('—');
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
