// The Tier filter's buttons, built from the (account, tier) Classes
// rather than written into the page (backlog 1be37454; page audit
// 08b0c4f8 GAP 11, 2026-09-23). WatchlistPage hard-coded Platinum /
// Gold / Silver — a copy of registry data (CLAUDE.md §9, §9a) — so a
// tier a tenant adds by inserting one Class row never became a filter,
// and an untiered account was reachable only under All.

import { humanizeClassCode } from '../people/types';

/// All, or one tier code — `null` being the accounts with no tier. A
/// union rather than a sentinel string, so no tier code can collide
/// with the All button (the same shape as the roster's CodeFilter).
export type TierFilter = Readonly<{ kind: 'all' } | { kind: 'code'; code: string | null }>;

/// One Tier button: a tier code (`null` = No tier), its label, and how
/// many scored rows it holds.
export type TierBucket = Readonly<{ code: string | null; label: string; count: number }>;

/// The Tier buttons for `tiers` — the tier of each scored row whose
/// account the directory returned. Every tier Class gets a button, in
/// the registry's order and counted even at zero; a code on a row that
/// no Class names (a retired Class, or the registry not answered yet)
/// follows under a humanised label, so its rows stay reachable; and a
/// No tier bucket closes the list when any account has none. Every
/// entry of `tiers` is in exactly one bucket.
export function tierBuckets(
  tiers: ReadonlyArray<string | null>,
  tierClasses: ReadonlyArray<Readonly<{ code: string; display_name: string }>>,
): TierBucket[] {
  const countOf = (code: string | null): number => tiers.filter((t) => t === code).length;
  const classCodes = new Set(tierClasses.map((c) => c.code));
  const unclassed = Array.from(
    new Set(tiers.filter((t): t is string => t !== null && !classCodes.has(t))),
  );
  const nulls = countOf(null);
  return [
    ...tierClasses.map((c) => ({ code: c.code, label: c.display_name, count: countOf(c.code) })),
    ...unclassed.map((t) => ({ code: t, label: humanizeClassCode(t), count: countOf(t) })),
    ...(nulls > 0 ? [{ code: null, label: 'No tier', count: nulls }] : []),
  ];
}

/// Whether `filter` admits a row whose account has `tier`. `undefined`
/// is an account the directory did not return (a failed or capped
/// read): its tier is unknown, not absent, so only All admits it —
/// No tier is a claim about the account, and there is none to make.
export function tierAdmits(filter: TierFilter, tier: string | null | undefined): boolean {
  if (filter.kind === 'all') return true;
  return tier !== undefined && tier === filter.code;
}
