// /watchlist's Tier buttons are built from the (account, tier) Classes,
// not from a hand-written Platinum / Gold / Silver trio (backlog
// 1be37454; page audit 08b0c4f8 GAP 11, 2026-09-23). The trio was a
// copy of registry data in a page: a tier a tenant adds never appeared
// as a filter, and an untiered account (acct-anonymous-sponsor, 1 of 1
// live) was reachable only under All. These cases hold the rule the fix
// states: every tier Class has a button, and every scored row whose
// account the directory holds sits in exactly one bucket.

import { describe, expect, it } from 'bun:test';
import { tierAdmits, tierBuckets } from './tiers';

/// The three seeded tier Classes (01-registries.sql) plus a fourth a
/// tenant added — the case the hand-written trio could never show.
const TIER_CLASSES = [
  { code: 'platinum', display_name: 'Platinum' },
  { code: 'gold', display_name: 'Gold' },
  { code: 'silver', display_name: 'Silver' },
  { code: 'bronze', display_name: 'Bronze' },
];

describe('tierBuckets', () => {
  it('gives every tier Class a button, in registry order, counted', () => {
    expect(tierBuckets(['gold', 'bronze', 'gold'], TIER_CLASSES)).toEqual([
      { code: 'platinum', label: 'Platinum', count: 0 },
      { code: 'gold', label: 'Gold', count: 2 },
      { code: 'silver', label: 'Silver', count: 0 },
      { code: 'bronze', label: 'Bronze', count: 1 },
    ]);
  });

  it('adds a No tier bucket when an account has none, and only then', () => {
    expect(tierBuckets(['gold', null], TIER_CLASSES).at(-1)).toEqual({
      code: null, label: 'No tier', count: 1,
    });
    expect(tierBuckets(['gold'], TIER_CLASSES).some((b) => b.code === null)).toBe(false);
  });

  it('keeps a tier no Class names reachable rather than dropping its rows', () => {
    // A retired Class, or the registry read not answered yet: the rows
    // still carry the code, so the code still gets a button.
    expect(tierBuckets(['gold', 'legacy'], [])).toEqual([
      { code: 'gold', label: 'Gold', count: 1 },
      { code: 'legacy', label: 'Legacy', count: 1 },
    ]);
  });

  it('puts every row in exactly one bucket', () => {
    const tiers = ['platinum', 'bronze', null, 'legacy', 'gold', null];
    const total = tierBuckets(tiers, TIER_CLASSES).reduce((n, b) => n + b.count, 0);
    expect(total).toBe(tiers.length);
  });
});

describe('tierAdmits', () => {
  it('All admits every row, an unknown account included', () => {
    expect(tierAdmits({ kind: 'all' }, 'gold')).toBe(true);
    expect(tierAdmits({ kind: 'all' }, undefined)).toBe(true);
  });

  it('a code admits only its rows; No tier admits only the untiered', () => {
    expect(tierAdmits({ kind: 'code', code: 'bronze' }, 'bronze')).toBe(true);
    expect(tierAdmits({ kind: 'code', code: 'bronze' }, 'gold')).toBe(false);
    expect(tierAdmits({ kind: 'code', code: null }, null)).toBe(true);
    expect(tierAdmits({ kind: 'code', code: null }, 'gold')).toBe(false);
  });

  it('No tier does not claim a row whose account the directory did not return', () => {
    // undefined = the directory read failed or was capped: the tier is
    // unknown, not absent, so it is not an untiered account.
    expect(tierAdmits({ kind: 'code', code: null }, undefined)).toBe(false);
  });
});
