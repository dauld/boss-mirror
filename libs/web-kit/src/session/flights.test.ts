// The viewer's flights, read the way the page carries them (design
// c4c2a607, backlog 73c31776). Run via `bun test`.

import { describe, expect, test } from 'bun:test';
import { flightIsOn, flightsFromAnswer } from './flights-inline';

describe('flightsFromAnswer — the answer the gateway inlines or the read returns', () => {
  test('a list of codes is the set of flights on for this viewer', () => {
    const on = flightsFromAnswer({ flights: ['it-map-motion', 'b'] });
    expect(on).not.toBeNull();
    expect(flightIsOn(on, 'it-map-motion')).toBe(true);
    expect(flightIsOn(on, 'b')).toBe(true);
  });

  test('a code the answer does not list is off', () => {
    const on = flightsFromAnswer({ flights: ['it-map-motion'] });
    expect(flightIsOn(on, 'something-else')).toBe(false);
  });

  test('anything that is not the answer is no answer, and no answer is every code off', () => {
    for (const raw of [undefined, null, 'it-map-motion', ['it-map-motion'], {}, { flights: 'x' }, { flights: ['x', 1] }]) {
      expect(flightsFromAnswer(raw)).toBeNull();
    }
    expect(flightIsOn(null, 'it-map-motion')).toBe(false);
  });
});
