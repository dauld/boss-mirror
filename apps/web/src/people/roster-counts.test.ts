// /ux/people counted an unread roster as an empty one (backlog 47eadca3;
// page audit 0c0265a3 GAP 3, 2026-09-23). The header and every filter
// button derived their numbers from the `[]` the roster starts as, so
// while the read was in flight — and above the "Couldn't load the
// roster" line when it failed — the page stated "0 active employees",
// "0 certifications expiring in 90 days", Active (0) and All (0). The
// body was honest; the counts were the false-empty class. These cases
// hold the rule: a count is printed only for a roster that was read.
// The page wiring is pinned by people-unread-roster-counts.mocked.spec.ts.

import { describe, expect, it } from 'bun:test';
import { countedLabel, rosterHeader, rosterRead } from './roster-counts';

describe('rosterRead', () => {
  it('is loading while the read is in flight, even with no failure yet', () => {
    expect(rosterRead(true, null)).toBe('loading');
  });

  it('is failed when the read failed', () => {
    expect(rosterRead(false, 'people HTTP 500')).toBe('failed');
  });

  it('is read only when the read finished and did not fail', () => {
    expect(rosterRead(false, null)).toBe('read');
  });
});

describe('rosterHeader', () => {
  it('counts a read roster, zero included — an empty roster that was read is empty', () => {
    expect(rosterHeader('read', 12, 3)).toEqual({
      title: '12 active employees',
      subtitle: '3 certifications expiring in 90 days',
    });
    expect(rosterHeader('read', 0, 0)).toEqual({
      title: '0 active employees',
      subtitle: '0 certifications expiring in 90 days',
    });
  });

  it('states no count while the roster is loading', () => {
    const h = rosterHeader('loading', 0, 0);
    expect(h.title).toBe('Active employees');
    expect(h.subtitle).toBe('Loading the roster…');
  });

  it('says the counts are unknown when the read failed', () => {
    const h = rosterHeader('failed', 0, 0);
    expect(h.title).toBe('Active employees');
    expect(h.subtitle).toBe('Counts unknown: the roster did not load');
  });

  it('never prints a number for an unread roster, whatever the arrays hold', () => {
    for (const read of ['loading', 'failed'] as const) {
      const h = rosterHeader(read, 7, 2);
      expect(`${h.title} ${h.subtitle}`).not.toMatch(/\d/);
    }
  });
});

describe('countedLabel', () => {
  it('appends the count for a read roster, zero included', () => {
    expect(countedLabel('Active', 4, 'read')).toBe('Active (4)');
    expect(countedLabel('On Leave', 0, 'read')).toBe('On Leave (0)');
  });

  it('omits the count while loading and after a failed read', () => {
    expect(countedLabel('Active', 0, 'loading')).toBe('Active');
    expect(countedLabel('All', 0, 'failed')).toBe('All');
  });
});
