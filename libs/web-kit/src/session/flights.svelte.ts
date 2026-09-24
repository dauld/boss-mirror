// Flights — which in-flight changes are on for the person looking at
// this page (design c4c2a607, backlog 73c31776).
//
// A fork in source names its flight by code:
//
//   {#if flightOn('it-map-motion')} new path {:else} old path {/if}
//
// The string is the flight's identity, not its state. The state lives
// only in the flight's packet, and the server resolves it for this
// viewer: the gateway inlines the answer into index.html, so the first
// paint already takes the right path. A page the gateway did not serve
// (the dev server) calls `loadFlights()` for the same answer by fetch.
//
// Unlisted is off. With no answer at all every flight is off, so a
// failed read shows the old path — the safe one.

import { flightIsOn, flightsFromAnswer, type FlightsOn } from './flights-inline';

export { flightsFromAnswer, type FlightsOn };

const inlined = flightsFromAnswer((globalThis as { __BOSS_FLIGHTS__?: unknown }).__BOSS_FLIGHTS__);

export const flights = $state<{ on: FlightsOn }>({ on: inlined });

/// Fetch the viewer's flights, for a page that did not carry them. A
/// failed read leaves every flight off.
export async function loadFlights(): Promise<void> {
  try {
    const r = await fetch('/api/flights/mine');
    flights.on = r.ok ? flightsFromAnswer(await r.json()) : null;
  } catch {
    flights.on = null;
  }
}

/// Is the flight `code` on for this viewer? Only when the server said so.
export function flightOn(code: string): boolean {
  return flightIsOn(flights.on, code);
}
