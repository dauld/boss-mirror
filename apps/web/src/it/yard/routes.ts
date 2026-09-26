// THE ROUTES THE SERVER SERVES — `GET /api/yard/routes` (design e765b3fc
// §2b, car R2; drawn by car R3 on feedback 84cba7e2).
//
// WHY THE MAP READS THIS AND NOTHING ELSE FOR ITS EDGES. Every section on
// the transit map used to come from a hand-written list in transit.ts
// (PATHS), itself one of three copies of the same ten pairs (world.ts
// BORDERS, boss_jobs::borders::BORDERS), pinned equal to each other and
// to nothing else. The design measured it against the record: of ten
// drawn edges five matched a source, three were partly wrong, two carried
// no packet — and ten real routes were drawn nowhere, among them the one
// David named: "like how the dock routes a train over to the gates before
// it departs onto the tracks." The server now derives every route from
// the protocols (each workflow's ready_when walked through the one
// placement function), the declared hand-offs, and the moves record, and
// serves each with its sources. The page draws exactly that set: a route
// the server stops serving leaves the map, and one it starts serving
// arrives, with no web change. The lint
// `infra/lint/a-map-edge-is-served-not-drawn.sh` refuses a station-pair
// literal under apps/web/src/it/yard/ so a hand-drawn edge cannot return.
//
// AN END IS A REGION OR OFF THE MAP (`null`). A route from nowhere is an
// ENTRY — a packet admitted onto the map — and a route to nowhere is an
// EXIT, an off-ramp: the terminal the packet closed on (David,
// added_2026_09_25_david_offramps: "every packet that leaves the map must
// leave by a drawn route"). `declared: false` is a route only the moves
// record supports — observed, undeclared — drawn dashed red.
//
// Pure functions of the payload, so `bun test` pins every rule without a
// DOM.
import { fetchRemote, type Remote } from '../../data/remote';

/** What supports one route (boss_jobs::routes::Source). */
export type RouteSource =
  | Readonly<{ source: 'workflow'; workflow: string; version: number; step: string; via: string }>
  | Readonly<{ source: 'hand-off'; by: string; from: string | null; to: string | null; why: string }>
  | Readonly<{ source: 'observed'; moves: number; last_at: string }>;

export type Route = Readonly<{
  from: string | null;
  to: string | null;
  /** A protocol or a hand-off supports it; false — only moves did. */
  declared: boolean;
  sources: ReadonlyArray<RouteSource>;
}>;

export type Routes = Readonly<{
  window_hours: number;
  /** Whether the moves record was read: false serves the declared
   *  routes with no counts beside them — never zeroes. */
  observed: boolean;
  routes: ReadonlyArray<Route>;
  /** Hand-offs the server could not resolve, each naming why. */
  refused: ReadonlyArray<string>;
}>;

function asObject(raw: unknown, where: string): Record<string, unknown> {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
    throw new Error(`${where}: expected an object`);
  }
  return raw as Record<string, unknown>;
}

const endOf = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);

function parseSource(raw: unknown): RouteSource {
  const o = asObject(raw, 'route source');
  switch (o.source) {
    case 'workflow':
      return {
        source: 'workflow',
        workflow: String(o.workflow ?? ''),
        version: typeof o.version === 'number' ? o.version : 0,
        step: String(o.step ?? ''),
        via: String(o.via ?? ''),
      };
    case 'hand-off':
      return { source: 'hand-off', by: String(o.by ?? ''), from: endOf(o.from), to: endOf(o.to), why: String(o.why ?? '') };
    case 'observed':
      return { source: 'observed', moves: typeof o.moves === 'number' ? o.moves : 0, last_at: String(o.last_at ?? '') };
    default:
      // A source this page cannot name is a wrong server, not a quiet one.
      throw new Error(`route source: unknown kind ${JSON.stringify(o.source)}`);
  }
}

function parseRoute(raw: unknown): Route {
  const o = asObject(raw, 'route');
  const from = endOf(o.from);
  const to = endOf(o.to);
  if (from === null && to === null) throw new Error('route: neither end is on the map');
  if (!Array.isArray(o.sources) || o.sources.length === 0) {
    throw new Error(`route ${from ?? '(entry)'} → ${to ?? '(exit)'}: served with no source`);
  }
  return { from, to, declared: o.declared === true, sources: o.sources.map(parseSource) };
}

/** The whole set, or a throw — a payload without `routes` is a wrong
 *  server, not a map with no edges on it. */
export function parseRoutes(raw: unknown): Routes {
  const o = asObject(raw, 'yard routes');
  if (!Array.isArray(o.routes)) throw new Error('yard routes: expected a routes list');
  return {
    window_hours: typeof o.window_hours === 'number' ? o.window_hours : 0,
    observed: o.observed === true,
    routes: o.routes.map(parseRoute),
    refused: Array.isArray(o.refused) ? o.refused.map(String) : [],
  };
}

export async function fetchRoutes(): Promise<Remote<Routes>> {
  return fetchRemote('/api/yard/routes', parseRoutes);
}

// ---------------------------------------------------------------------
// Words — what a route's title and its panel say.
// ---------------------------------------------------------------------

/** The terminals an exit names — the steps its packets close on, each
 *  once, in the order served — and the hand-offs that take a packet off
 *  the map without one. */
export function exitNames(r: Route): ReadonlyArray<string> {
  const names = r.sources.flatMap((s) =>
    s.source === 'workflow' ? [s.step] : s.source === 'hand-off' ? [s.by.replace(/^[a-z]+:/, '')] : [],
  );
  return [...new Set(names)];
}

/** The moves the record counted on this route in the window, or null
 *  where it counted none or was not read. */
export function movesOn(r: Route): number | null {
  const counts = r.sources.flatMap((s) => (s.source === 'observed' ? [s.moves] : []));
  return counts.length === 0 ? null : counts.reduce((a, b) => a + b, 0);
}

/** One line per source: the protocol step that makes the move, the
 *  declared hand-off and its why, the moves the record counted. */
export function sourceLines(r: Route, windowHours: number): ReadonlyArray<string> {
  const lines = r.sources.map((s) =>
    s.source === 'workflow'
      ? `${s.workflow} v${s.version}, step ${s.step} (${s.via})`
      : s.source === 'hand-off'
        ? `${s.by}: ${s.why}`
        : `${s.moves} moves observed in ${windowHours}h`,
  );
  return r.declared ? lines : ['observed, undeclared — no protocol or hand-off declares this route', ...lines];
}
