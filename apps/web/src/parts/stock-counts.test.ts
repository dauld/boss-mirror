// /ux/parts counted an unread stock list as an empty one (backlog
// f867d71c; page audit 63d810aa gap 5, 2026-09-23). The header and every
// filter button derived their numbers from the `[]` the inventory starts
// as, so while the four reads were in flight — and above "Couldn't load
// parts" when a row source failed — the page stated "0 parts",
// "0 need attention · 0 out · 0 critical", All (0) and Needs attention
// (0). The body was honest; the counts were the false-empty class. These
// cases hold the rule: a count is printed only for a list that was read.
// The page wiring is pinned by tests/mocked/parts-page.mocked.spec.ts.
// The read state and the filter labels are data/readState.ts's
// (readStateOfLoad, countLabel), tested there (backlog a97d4cf2).

import { describe, expect, it } from 'bun:test';
import { failedRead, loadingRead, okRead } from '../data/readState';
import { partsHeader } from './stock-counts';

describe('partsHeader', () => {
  const counts = { total: 6, attention: 3, out: 1, critical: 1 };
  const zero = { total: 0, attention: 0, out: 0, critical: 0 };

  it('counts a read list, zero included — an inventory that was read and is empty IS empty', () => {
    expect(partsHeader(okRead, 'parts', counts)).toEqual({
      title: '6 parts',
      subtitle: '3 need attention · 1 out · 1 critical',
    });
    expect(partsHeader(okRead, 'parts', zero)).toEqual({
      title: '0 parts',
      subtitle: '0 need attention · 0 out · 0 critical',
    });
  });

  it('names the page by its tenant label, counted or not', () => {
    const label = 'ingredients & packaging items';
    expect(partsHeader(okRead, label, counts).title).toBe('6 ingredients & packaging items');
    expect(partsHeader(loadingRead, label, zero).title).toBe('Ingredients & packaging items');
  });

  it('states no count while the list is loading', () => {
    expect(partsHeader(loadingRead, 'parts', zero)).toEqual({
      title: 'Parts',
      subtitle: 'Loading stock…',
    });
  });

  it('says the counts are unknown when a row source failed', () => {
    expect(partsHeader(failedRead('HTTP 500'), 'parts', zero)).toEqual({
      title: 'Parts',
      subtitle: 'Counts unknown: parts did not load',
    });
  });

  it('never prints a number for an unread list, whatever the arrays hold', () => {
    // A failed MODELS read leaves the inventory array populated: the
    // counts below are real rows, and still not the page's to state.
    for (const read of [loadingRead, failedRead('HTTP 500')]) {
      const h = partsHeader(read, 'parts', counts);
      expect(`${h.title} ${h.subtitle}`).not.toMatch(/\d/);
    }
  });
});
