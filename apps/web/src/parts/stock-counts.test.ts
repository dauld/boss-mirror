// /ux/parts counted an unread stock list as an empty one (backlog
// f867d71c; page audit 63d810aa gap 5, 2026-09-23). The header and every
// filter button derived their numbers from the `[]` the inventory starts
// as, so while the four reads were in flight — and above "Couldn't load
// parts" when a row source failed — the page stated "0 parts",
// "0 need attention · 0 out · 0 critical", All (0) and Needs attention
// (0). The body was honest; the counts were the false-empty class. These
// cases hold the rule: a count is printed only for a list that was read.
// The page wiring is pinned by tests/mocked/parts-page.mocked.spec.ts.

import { describe, expect, it } from 'bun:test';
import { countedLabel, partsHeader, stockRead } from './stock-counts';

describe('stockRead', () => {
  it('is loading while the reads are in flight, even with no failure yet', () => {
    expect(stockRead(true, null)).toBe('loading');
  });

  it('is failed when a row source failed', () => {
    expect(stockRead(false, 'HTTP 500')).toBe('failed');
  });

  it('is read only when the reads finished and none failed', () => {
    expect(stockRead(false, null)).toBe('read');
  });
});

describe('partsHeader', () => {
  const counts = { total: 6, attention: 3, out: 1, critical: 1 };
  const zero = { total: 0, attention: 0, out: 0, critical: 0 };

  it('counts a read list, zero included — an inventory that was read and is empty IS empty', () => {
    expect(partsHeader('read', 'parts', counts)).toEqual({
      title: '6 parts',
      subtitle: '3 need attention · 1 out · 1 critical',
    });
    expect(partsHeader('read', 'parts', zero)).toEqual({
      title: '0 parts',
      subtitle: '0 need attention · 0 out · 0 critical',
    });
  });

  it('names the page by its tenant label, counted or not', () => {
    const label = 'ingredients & packaging items';
    expect(partsHeader('read', label, counts).title).toBe('6 ingredients & packaging items');
    expect(partsHeader('loading', label, zero).title).toBe('Ingredients & packaging items');
  });

  it('states no count while the list is loading', () => {
    expect(partsHeader('loading', 'parts', zero)).toEqual({
      title: 'Parts',
      subtitle: 'Loading stock…',
    });
  });

  it('says the counts are unknown when a row source failed', () => {
    expect(partsHeader('failed', 'parts', zero)).toEqual({
      title: 'Parts',
      subtitle: 'Counts unknown: parts did not load',
    });
  });

  it('never prints a number for an unread list, whatever the arrays hold', () => {
    // A failed MODELS read leaves the inventory array populated: the
    // counts below are real rows, and still not the page's to state.
    for (const read of ['loading', 'failed'] as const) {
      const h = partsHeader(read, 'parts', counts);
      expect(`${h.title} ${h.subtitle}`).not.toMatch(/\d/);
    }
  });
});

describe('countedLabel', () => {
  it('appends the count for a read list, zero included', () => {
    expect(countedLabel('Needs attention', 3, 'read')).toBe('Needs attention (3)');
    expect(countedLabel('Out of stock', 0, 'read')).toBe('Out of stock (0)');
  });

  it('omits the count while loading and after a failed read', () => {
    expect(countedLabel('All', 0, 'loading')).toBe('All');
    expect(countedLabel('Needs attention', 0, 'failed')).toBe('Needs attention');
  });
});
