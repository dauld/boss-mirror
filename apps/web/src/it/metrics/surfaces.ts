// Surface usage — which surfaces each operator opened, read off the
// `surface_opens` roll-up (backlog 628f182b).
//
// David, 2026-09-16: "let's measure which surfaces I open for a week."
// The recording half is apps/web/src/shell/surface-opens.ts (one POST
// per navigation, the pattern and the time, the actor signed by the
// gateway); the daily chore (infra/surface-usage.sh) files a 24-hour
// roll-up onto a `maintenance-surface-usage` packet. This is the
// reading half: the last seven days per actor, opens per route
// sorted, distinct routes, and the catalogued surfaces NOBODY opened —
// the deletion candidates.
//
// ONE READ, SHARED WITH THE CHORE. `GET /api/surface-opens/rollup`
// does the GROUP BY server-side, so the page and the packet cannot
// disagree by summing differently; everything here is a pure function
// of the rows it returns. The never-opened list compares those rows
// against the nav catalog's paths — the catalog is the roster of
// surfaces, so a path it lists that no row names is a surface with no
// reader in the window.
//
// EVERY NUMBER IS A FUNCTION OF THE ROWS. A failed read is a failure
// (rendered as such, never as "nobody opened anything"); an empty
// roll-up is an empty week, which after seven days of recording is a
// real finding and before that is stated as "nothing recorded yet".

import { fetchRemote, type Remote } from '../../data/remote';
import { ROUTE_CATALOG } from '../../shell/nav-catalog';

export type RouteCount = Readonly<{ actor_id: string; route: string; opens: number; last_at: string }>;

export type Rollup = Readonly<{
  since: string | null;
  until: string | null;
  rows: ReadonlyArray<RouteCount>;
}>;

const rec = (v: unknown): Record<string, unknown> | null =>
  v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null);

function parseRow(v: unknown): RouteCount | null {
  const r = rec(v);
  const actor_id = r ? str(r.actor_id) : null;
  const route = r ? str(r.route) : null;
  const opens = r ? num(r.opens) : null;
  if (!r || !actor_id || !route || opens === null) return null;
  return { actor_id, route, opens, last_at: str(r.last_at) ?? '' };
}

/// The roll-up envelope `{since, until, rows}`. A bare array (a mock's
/// catch-all) is an empty roll-up with no window stated.
export function parseRollup(raw: unknown): Rollup {
  const env = rec(raw);
  const list: unknown[] = env && Array.isArray(env.rows) ? env.rows : [];
  return {
    since: env ? str(env.since) : null,
    until: env ? str(env.until) : null,
    rows: list.flatMap((r) => parseRow(r) ?? []),
  };
}

/// The window's two edges for the last `days` days ending at `now`.
export function windowFor(days: number, now: Date): Readonly<{ since: string; until: string }> {
  return { since: new Date(now.getTime() - days * 86_400_000).toISOString(), until: now.toISOString() };
}

export function loadSurfaceRollup(
  days: number,
  now: Date = new Date(),
): Promise<Exclude<Remote<Rollup>, { kind: 'loading' }>> {
  const w = windowFor(days, now);
  return fetchRemote(
    `/api/surface-opens/rollup?since=${encodeURIComponent(w.since)}&until=${encodeURIComponent(w.until)}`,
    parseRollup,
  );
}

export type ActorUsage = Readonly<{
  actor_id: string;
  opens: number;
  distinct: number;
  /** Most-opened first, then by route so equal counts are stable. */
  routes: ReadonlyArray<Readonly<{ route: string; opens: number; last_at: string }>>;
}>;

/// Rows folded per actor: total opens, distinct routes, and the routes
/// sorted most-opened first. Actors are ordered by opens, busiest first.
export function perActor(rows: ReadonlyArray<RouteCount>): ReadonlyArray<ActorUsage> {
  const byActor = rows.reduce<Map<string, RouteCount[]>>((m, r) => {
    m.set(r.actor_id, [...(m.get(r.actor_id) ?? []), r]);
    return m;
  }, new Map());
  return [...byActor.entries()]
    .map(([actor_id, rs]) => ({
      actor_id,
      opens: rs.reduce((s, r) => s + r.opens, 0),
      distinct: new Set(rs.map((r) => r.route)).size,
      routes: [...rs]
        .sort((a, b) => b.opens - a.opens || (a.route < b.route ? -1 : a.route > b.route ? 1 : 0))
        .map((r) => ({ route: r.route, opens: r.opens, last_at: r.last_at })),
    }))
    .sort((a, b) => b.opens - a.opens || (a.actor_id < b.actor_id ? -1 : a.actor_id > b.actor_id ? 1 : 0));
}

/// Every path the nav catalog registers, once each, in catalog order
/// (two catalog entries share `/it/registry/rules`).
export function catalogPaths(): ReadonlyArray<string> {
  return [...new Set(Object.values(ROUTE_CATALOG).map((e) => e.path))];
}

/// The catalogued surfaces no row names — nobody opened them in the
/// window. `paths` is injectable so the fold is testable against a
/// small roster; the page passes `catalogPaths()`.
export function neverOpened(rows: ReadonlyArray<RouteCount>, paths: ReadonlyArray<string>): ReadonlyArray<string> {
  const opened = new Set(rows.map((r) => r.route));
  return paths.filter((p) => !opened.has(p));
}
