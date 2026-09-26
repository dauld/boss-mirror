// FIXTURES FOR THE YARD'S TWO EDGE READS — test data, not a layout.
//
// Car R3 of design e765b3fc deleted every edge list the web app drew
// from (world.ts BORDERS, transit.ts PATHS): the world map now draws a
// rail per border the server's `/api/yard/borders` answers, and the
// transit map the routes `/api/yard/routes` derives from the protocols.
// The unit tests and the mocked specs still need a server to answer, so
// the answers live here — under tests/, where the lint
// `a-map-edge-is-served-not-drawn` does not look, because a test's
// fixture is the server's answer standing in, not a drawing.

/** The borders the server's borders read answers today
 *  (boss_jobs::borders::BORDERS, whose rates car M3 moves onto the
 *  moves record): the line in flow order, the crossing to the mirror,
 *  the garage's two feeders. */
export const BORDERS: ReadonlyArray<Readonly<{ from: string; to: string }>> = [
  { from: 'receiving', to: 'marshalling' },
  { from: 'marshalling', to: 'shop-floor' },
  { from: 'shop-floor', to: 'gates' },
  { from: 'gates', to: 'dock' },
  { from: 'dock', to: 'track' },
  { from: 'track', to: 'arrivals' },
  { from: 'arrivals', to: 'shed' },
  { from: 'arrivals', to: 'publish' },
  { from: 'gates', to: 'garage' },
  { from: 'track', to: 'garage' },
];

type Source =
  | Readonly<{ source: 'workflow'; workflow: string; version: number; step: string; via: string }>
  | Readonly<{ source: 'hand-off'; by: string; from: string | null; to: string | null; why: string }>
  | Readonly<{ source: 'observed'; moves: number; last_at: string }>;

export type FixtureRoute = Readonly<{ from: string | null; to: string | null; declared: boolean; sources: ReadonlyArray<Source> }>;

const step = (workflow: string, s: string): Source => ({ source: 'workflow', workflow, version: 1, step: s, via: 'completed' });
const handOff = (by: string): Source => ({ source: 'hand-off', by, from: null, to: null, why: `${by} hands the packet on` });
const seen = (moves: number): Source => ({ source: 'observed', moves, last_at: '2026-09-26T12:00:00Z' });

/** A routes answer shaped like the live one of 2026-09-26 17:40Z: the
 *  inbound pair, the car's line, the TRAIN'S line (dock → gates → track
 *  → arrivals), the garage both ways, a cadence's hand-off, entries,
 *  exits with their terminals, and one route only the moves record
 *  supports — observed, undeclared. */
export const ROUTES: ReadonlyArray<FixtureRoute> = [
  { from: null, to: 'receiving', declared: true, sources: [step('backlog-item', 'filed')] },
  { from: null, to: 'shop-floor', declared: true, sources: [step('agent-run', 'claimed')] },
  { from: null, to: 'dock', declared: true, sources: [step('pr-train', 'scheduled')] },
  { from: 'receiving', to: 'marshalling', declared: true, sources: [step('backlog-item', 'triage'), seen(20)] },
  { from: 'receiving', to: null, declared: true, sources: [step('backlog-item', 'duplicate'), step('backlog-item', 'stale')] },
  { from: 'marshalling', to: 'shop-floor', declared: true, sources: [handOff('verb:dispatch'), seen(24)] },
  { from: 'marshalling', to: null, declared: true, sources: [step('backlog-item', 'closed'), step('backlog-item', 'declined')] },
  { from: 'shop-floor', to: 'gates', declared: true, sources: [handOff('verb:gate'), seen(33)] },
  { from: 'shop-floor', to: 'marshalling', declared: true, sources: [handOff('rule:an-abandoned-step-is-reclaimed-when-its-run-died')] },
  { from: 'shop-floor', to: null, declared: true, sources: [step('agent-run', 'landed'), step('agent-run', 'died')] },
  { from: 'gates', to: 'dock', declared: true, sources: [handOff('rule:auto-park-on-gate-green'), seen(28)] },
  { from: 'gates', to: 'garage', declared: true, sources: [step('gate-run', 'failed')] },
  { from: 'gates', to: 'track', declared: true, sources: [step('pr-train', 'merged'), seen(9)] },
  { from: 'dock', to: 'gates', declared: true, sources: [step('pr-train', 'pr'), seen(62)] },
  { from: 'dock', to: 'garage', declared: true, sources: [step('ship-a-change', 'review')] },
  { from: 'dock', to: null, declared: true, sources: [step('ship-a-change', 'settled'), step('pr-train', 'cancelled')] },
  { from: 'garage', to: 'dock', declared: true, sources: [step('ship-a-change', 'review')] },
  { from: 'garage', to: 'gates', declared: true, sources: [handOff('verb:gate')] },
  { from: 'track', to: 'arrivals', declared: true, sources: [step('pr-train', 'arrived')] },
  { from: 'track', to: 'shed', declared: true, sources: [handOff('cadence:train-reconcile')] },
  { from: 'shed', to: null, declared: true, sources: [step('ship-a-change', 'merged'), step('ship-a-change', 'disproved')] },
  { from: 'shed', to: 'arrivals', declared: false, sources: [seen(39)] },
];

/** The routes read's payload around a set of routes. */
export const routesPayload = (routes: ReadonlyArray<FixtureRoute> = ROUTES) => ({
  window_hours: 24,
  observed: true,
  routes,
  walked: [],
  refused: [],
});
