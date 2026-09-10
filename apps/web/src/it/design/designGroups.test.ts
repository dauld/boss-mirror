import { describe, expect, test } from 'bun:test';
import { groupDocs, groupOf, openWeight, type DesignDoc } from './designGroups';

function doc(over: Partial<DesignDoc>): DesignDoc {
  return {
    path: 'docs/design/a.md',
    title: 'A',
    status: 'in-review',
    open_questions: 0,
    word_count: 100,
    last_modified: '2026-08-01T00:00:00Z',
    ...over,
  };
}

const none = () => false;

describe('groupOf', () => {
  test('a doc claiming in-review with nothing open is library, not your queue', () => {
    // The exact shape of the bug: eleven docs sat in "In review &
    // discussion" on the strength of a frontmatter line alone.
    expect(groupOf(doc({ status: 'in-review' }), false)).toBe('library');
    expect(groupOf(doc({ status: 'reopened' }), false)).toBe('library');
  });

  test('open questions put it on your plate whatever the status says', () => {
    expect(groupOf(doc({ status: 'approved', open_questions: 2 }), false)).toBe('needs-you');
    expect(groupOf(doc({ status: 'living', open_questions: 1 }), false)).toBe('needs-you');
  });

  test('an open review Job counts even with nothing parsed', () => {
    expect(groupOf(doc({ status: 'living' }), true)).toBe('needs-you');
  });

  test('a draft with nothing open yet is a draft, not settled', () => {
    // Filing a doc someone is mid-way through writing as a settled
    // reference is the failure mode of the other direction.
    expect(groupOf(doc({ status: 'draft' }), false)).toBe('draft');
  });

  test('a draft that has started asking is on your plate', () => {
    expect(groupOf(doc({ status: 'draft', open_questions: 3 }), false)).toBe('needs-you');
  });
});

describe('groupDocs', () => {
  test('splits the corpus and puts the heaviest doc first', () => {
    const g = groupDocs(
      [
        doc({ path: 'light.md', open_questions: 1 }),
        doc({ path: 'heavy.md', open_questions: 8 }),
        doc({ path: 'settled.md', status: 'in-review' }),
        doc({ path: 'wip.md', status: 'draft' }),
      ],
      none,
    );
    expect(g.needsYou.map((d) => d.path)).toEqual(['heavy.md', 'light.md']);
    expect(g.drafts.map((d) => d.path)).toEqual(['wip.md']);
    expect(g.library.map((d) => d.path)).toEqual(['settled.md']);
  });

  test('order is stable when weights tie', () => {
    const g = groupDocs(
      [doc({ path: 'b.md', open_questions: 2 }), doc({ path: 'a.md', open_questions: 2 })],
      none,
    );
    expect(g.needsYou.map((d) => d.path)).toEqual(['a.md', 'b.md']);
  });

  test('the review lookup is asked per path, not assumed', () => {
    const g = groupDocs(
      [doc({ path: 'reviewed.md', status: 'living' }), doc({ path: 'quiet.md', status: 'living' })],
      (p) => p === 'reviewed.md',
    );
    expect(g.needsYou.map((d) => d.path)).toEqual(['reviewed.md']);
    expect(g.library.map((d) => d.path)).toEqual(['quiet.md']);
  });
});

describe('openWeight', () => {
  test('counts what is actually waiting, not how many docs', () => {
    // "9 docs" and "48 questions" are different sizes of afternoon.
    expect(
      openWeight([
        doc({ open_questions: 8 }),
        doc({ open_questions: 5 }),
      ]),
    ).toEqual({ questions: 13 });
  });

  test('nothing waiting reads as zero, not as absent', () => {
    expect(openWeight([])).toEqual({ questions: 0 });
  });
});
