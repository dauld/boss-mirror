// The inbox read's parse (backlog e2679b23 (c)). It used to be
// `Array.isArray(raw) ? raw : []`, so a 200 whose body was anything but
// a list — an envelope, null, an error string — became an EMPTY inbox
// and the page said "Nothing is waiting on you". A parse that throws is
// a failed read under fetchRemote, which the page renders with Retry.

import { describe, expect, test } from 'bun:test';
import { parseInbox } from './read';

const PATH = '/api/messages/inbox/emp-001';

describe('parseInbox', () => {
  test('a list is the inbox, as given', () => {
    const rows = [{ id: 'm1' }, { id: 'm2' }];
    expect(parseInbox(PATH)(rows)).toEqual(rows as never);
  });

  test('an empty list is an empty inbox — the one body that may say so', () => {
    expect(parseInbox(PATH)([])).toEqual([]);
  });

  test.each([
    [{ data: [] }, 'an object'],
    [null, 'null'],
    ['message store down', 'a string'],
    [0, 'a number'],
    [true, 'a boolean'],
  ] as const)('%p is refused, naming the path and what came back', (body, what) => {
    expect(() => parseInbox(PATH)(body)).toThrow(
      `${PATH}: HTTP 200, but the body is ${what}, not a list`,
    );
  });
});
