import { describe, expect, test } from 'bun:test';
import { stepCorrections } from './corrections';

// The exact shape the job GET hands a step as `step.corrections`
// (design 4105b020): the job's append-only list, filtered to this step,
// each entry carrying its `index` in that list. Pinned on the server by
// crates/core/boss-jobs/src/corrections.rs::for_step.
const DAMAGED_FIX = {
  index: 0,
  step: 's',
  field: 'evidence',
  reads: 'confirmed:  is',
  should_read: 'confirmed: `why` is',
  why: 'the shell ate the word (2376b89e)',
  by: 'agent-claude',
  at: '2026-09-19T19:10:00Z',
};
const WRONG_FIX = {
  index: 2,
  step: 's',
  field: 'evidence',
  reads: 'every rule',
  should_read: 'every rule file',
  why: '',
  by: 'agent-claude',
  at: '2026-09-20T00:00:00Z',
};
const WITHDRAWAL = {
  index: 3,
  step: 's',
  field: 'evidence',
  withdraws: 2,
  why: 'the original was right',
  by: 'emp-david',
  at: '2026-09-21T00:00:00Z',
};

describe("a step's corrections, as the job GET hands them", () => {
  test('each correction is read with the field it names and both texts, verbatim', () => {
    expect(stepCorrections({ corrections: [DAMAGED_FIX] })).toEqual([
      {
        kind: 'correction',
        index: 0,
        field: 'evidence',
        reads: 'confirmed:  is',
        shouldRead: 'confirmed: `why` is',
        why: 'the shell ate the word (2376b89e)',
        by: 'agent-claude',
        at: '2026-09-19T19:10:00Z',
        withdrawnBy: null,
      },
    ]);
  });

  test('a withdrawn correction says by which entry, and the withdrawal names its target', () => {
    const out = stepCorrections({ corrections: [DAMAGED_FIX, WRONG_FIX, WITHDRAWAL] });
    expect(out.map((c) => [c.kind, c.index])).toEqual([
      ['correction', 0],
      ['correction', 2],
      ['withdrawal', 3],
    ]);
    expect(out[0]).toMatchObject({ withdrawnBy: null });
    expect(out[1]).toMatchObject({ withdrawnBy: 3 });
    expect(out[2]).toEqual({
      kind: 'withdrawal',
      index: 3,
      field: 'evidence',
      withdraws: 2,
      why: 'the original was right',
      by: 'emp-david',
      at: '2026-09-21T00:00:00Z',
    });
  });

  test('an empty should_read is a said deletion and is kept, not dropped', () => {
    const [c] = stepCorrections({ corrections: [{ ...DAMAGED_FIX, should_read: '' }] });
    expect(c).toMatchObject({ kind: 'correction', shouldRead: '' });
  });

  test('a step with no corrections, or a malformed list, draws nothing', () => {
    expect(stepCorrections({})).toEqual([]);
    expect(stepCorrections({ corrections: 'not a list' })).toEqual([]);
    expect(stepCorrections({ corrections: [null, 7, { field: 'x' }] })).toEqual([]);
    expect(stepCorrections(null)).toEqual([]);
  });
});
