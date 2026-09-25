// A malformed 200 is a failed read, not an empty one (backlog 67825067).
//
// readList and readEnvelope are the shape checks for a bare-list read
// and a `{ data: [...] }` read: each hands back what it read, or throws,
// and fetchRemote (or a page's own catch) turns the throw into the
// failure line. What neither may do is answer "no rows" for a body that
// did not say so — the false-empty class the inbox, the marshalling
// board and /it/design each had one layer below their fetch.

import { describe, expect, test } from 'bun:test';
import { readEnvelope, readList } from './shape';

const PATH = '/api/stations/load';

describe('readList', () => {
  test('a list is read as given, and [] is the only empty', () => {
    expect(readList(PATH, [{ id: 'm1' }])).toEqual([{ id: 'm1' }]);
    expect(readList(PATH, [])).toEqual([]);
  });

  test('an envelope where a list is due is refused, naming the read', () => {
    expect(() => readList(PATH, { data: [] })).toThrow(
      '/api/stations/load: HTTP 200, but the body is an object, not a list',
    );
    expect(() => readList(PATH, null)).toThrow('the body is null, not a list');
  });
});

describe('readEnvelope', () => {
  test('an envelope hands back its rows and the body they came in', () => {
    const body = { data: [{ station: 'a' }], total: 1, distinct_packets: 1 };
    const env = readEnvelope(PATH, body);
    expect(env.data).toEqual([{ station: 'a' }]);
    expect(env.body.distinct_packets).toBe(1);
  });

  test('an envelope with no rows is empty, and that is the only empty', () => {
    expect(readEnvelope(PATH, { data: [], total: 0 }).data).toEqual([]);
  });

  test('a list where the envelope is due is refused, naming the read', () => {
    // What the mocked api floor's catch-all answers, and what an old
    // gateway route could.
    expect(() => readEnvelope(PATH, [])).toThrow(
      '/api/stations/load: HTTP 200, but the body is a list, not a {data: [...]} envelope',
    );
  });

  test('an object with no data list is refused', () => {
    expect(() => readEnvelope(PATH, { error: 'not the shape' })).toThrow(
      '/api/stations/load: HTTP 200, but the body is an object with no data list, not a {data: [...]} envelope',
    );
    expect(() => readEnvelope(PATH, { data: 7 })).toThrow('an object with no data list');
  });

  test('null and scalars are refused, each named', () => {
    expect(() => readEnvelope(PATH, null)).toThrow('the body is null,');
    expect(() => readEnvelope(PATH, 'down')).toThrow('the body is a string,');
    expect(() => readEnvelope(PATH, 7)).toThrow('the body is a number,');
    expect(() => readEnvelope(PATH, undefined)).toThrow('the body is an undefined,');
  });
});
