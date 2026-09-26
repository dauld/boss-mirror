// /ux/people counted an unread roster as an empty one (backlog 47eadca3;
// page audit 0c0265a3 GAP 3, 2026-09-23). The header and every filter
// button derived their numbers from the `[]` the roster starts as, so
// while the read was in flight — and above the "Couldn't load the
// roster" line when it failed — the page stated "0 active employees",
// "0 certifications expiring in 90 days", Active (0) and All (0). The
// body was honest; the counts were the false-empty class. These cases
// hold the rule: a count is printed only for a roster that was read.
// The page wiring is pinned by people-unread-roster-counts.mocked.spec.ts.
// The read state and the filter labels are data/readState.ts's
// (readStateOfLoad, countLabel), tested there (backlog a97d4cf2).

import { describe, expect, it } from 'bun:test';
import { failedRead, loadingRead, okRead } from '../data/readState';
import { rosterHeader } from './roster-counts';

describe('rosterHeader', () => {
  it('counts a read roster, zero included — an empty roster that was read is empty', () => {
    expect(rosterHeader(okRead, 12, 3)).toEqual({
      title: '12 active employees',
      subtitle: '3 certifications expiring in 90 days',
    });
    expect(rosterHeader(okRead, 0, 0)).toEqual({
      title: '0 active employees',
      subtitle: '0 certifications expiring in 90 days',
    });
  });

  it('states no count while the roster is loading', () => {
    const h = rosterHeader(loadingRead, 0, 0);
    expect(h.title).toBe('Active employees');
    expect(h.subtitle).toBe('Loading the roster…');
  });

  it('says the counts are unknown when the read failed', () => {
    const h = rosterHeader(failedRead('people HTTP 500'), 0, 0);
    expect(h.title).toBe('Active employees');
    expect(h.subtitle).toBe('Counts unknown: the roster did not load');
  });

  it('never prints a number for an unread roster, whatever the arrays hold', () => {
    for (const read of [loadingRead, failedRead('people HTTP 500')]) {
      const h = rosterHeader(read, 7, 2);
      expect(`${h.title} ${h.subtitle}`).not.toMatch(/\d/);
    }
  });
});
