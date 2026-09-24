// The viewer's flights — the pure half (design c4c2a607, backlog
// 73c31776). The gateway resolves `GET /api/flights/mine` for the
// session and inlines the answer into index.html as
// `window.__BOSS_FLIGHTS__`, so the first paint already takes the right
// path; flights.svelte.ts owns the reactive copy and `flightOn`. Plain
// TypeScript, no runes, so `bun test` can exercise it.
//
// A CODE THE ANSWER DOES NOT LIST IS OFF — retired, unknown, not this
// viewer's, or no answer at all. The audience is decided on the server
// and never reaches the page: the answer is the codes that are on for
// THIS viewer and nothing else.

/// The codes on for this viewer, or `null` when there is no answer.
export type FlightsOn = ReadonlySet<string> | null;

/// The shape the gateway inlines and the read returns.
export type FlightsAnswer = { flights: string[] };

/// The set an answer names; `null` for anything that is not one
/// (absent, a string, a list with a non-string in it) — never a crash
/// at import time, and never a code read as on by accident.
export function flightsFromAnswer(raw: unknown): FlightsOn {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) return null;
  const flights = (raw as Record<string, unknown>).flights;
  if (!Array.isArray(flights) || !flights.every((c) => typeof c === 'string')) return null;
  return new Set(flights as string[]);
}

/// Is `code` on for this viewer? Only when the answer lists it.
export function flightIsOn(on: FlightsOn, code: string): boolean {
  return on !== null && on.has(code);
}
