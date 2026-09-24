import { describe, expect, test } from 'bun:test';
import { loadOwnerNames, ownerIdsOf, personIdsOf } from './ownerNames';

// Backlog 0268a829 (page audit 0ceeffa6, /ux/calendar GAP 8). The
// calendar named its owners by fetching the WHOLE roster, and a refusal
// or a network error was dropped (`if (r.ok)` with no else, and
// `catch { // ignore }`), so the owners silently became raw ids. These
// pin the two halves of the fix: read only the owners shown, and say so
// when a name could not be read. Lifted from calendar/ into data/ by
// backlog 1e73bd93, when the other pages that name a few people moved
// onto it.

type Answer = { status: number; body?: unknown } | Error;

/// A fetch that answers from a table keyed by URL and records what it
/// was asked, so a test can say which reads the page made.
function fakeFetch(answers: Record<string, Answer>) {
  const asked: string[] = [];
  const fn = async (url: string) => {
    asked.push(url);
    const a = answers[url];
    if (a === undefined) throw new Error(`unexpected read ${url}`);
    if (a instanceof Error) throw a;
    return {
      ok: a.status >= 200 && a.status < 300,
      status: a.status,
      json: async () => a.body,
    };
  };
  return { fn, asked };
}

describe('which owners the calendar asks about', () => {
  test('each owner shown once, sorted, and a row with no owner asks nothing', () => {
    const rows = [
      { owner_id: 'emp-b' },
      { owner_id: null },
      { owner_id: 'emp-a' },
      { owner_id: 'emp-b' },
    ];
    expect(ownerIdsOf(rows)).toEqual(['emp-a', 'emp-b']);
  });

  // Backlog 1e73bd93. A job may be owned by an agent, and the triage
  // board and the KB timeline name ACTORS, which are machines as often
  // as people. A machine has no people row, so asking about one is a
  // guaranteed 404 that would paint a false "couldn't load names" line;
  // formatActor already labels machines without a lookup.
  test('a machine owner is never asked about — it has no people row', () => {
    const rows = [
      { owner_id: 'agent-claude' },
      { owner_id: 'emp-a' },
      { owner_id: 'automation:train-conductor' },
    ];
    expect(ownerIdsOf(rows)).toEqual(['emp-a']);
  });
});

describe('which people a page asks about', () => {
  test('any ids at all: distinct, sorted, blanks and machines dropped', () => {
    expect(
      personIdsOf([
        'emp-b',
        null,
        undefined,
        '',
        'emp-a',
        'emp-b',
        'agent-claude',
        'claude@algedonic.dev',
        'claude:opus-5[1m]',
        'automation:rule:bill-approve',
        'system',
      ]),
    ).toEqual(['emp-a', 'emp-b']);
  });
});

describe('naming the owners shown', () => {
  test('reads only the owners shown, never the whole roster', async () => {
    const { fn, asked } = fakeFetch({
      '/api/people/emp-a': { status: 200, body: { id: 'emp-a', name: 'Ada' } },
      '/api/people/emp-b': { status: 200, body: { id: 'emp-b', name: 'Bo' } },
    });
    const out = await loadOwnerNames(['emp-a', 'emp-b'], fn);
    expect(asked.sort()).toEqual(['/api/people/emp-a', '/api/people/emp-b']);
    expect(asked).not.toContain('/api/people');
    expect(out.read).toEqual({ kind: 'ok' });
    expect(out.names.get('emp-a')).toBe('Ada');
    expect(out.names.get('emp-b')).toBe('Bo');
  });

  test('no owners shown is no read at all', async () => {
    const { fn, asked } = fakeFetch({});
    const out = await loadOwnerNames([], fn);
    expect(asked).toEqual([]);
    expect(out.read).toEqual({ kind: 'ok' });
    expect(out.names.size).toBe(0);
  });

  test('an id is path-encoded, so an odd id cannot address another route', async () => {
    const { fn, asked } = fakeFetch({
      '/api/people/a%2Fb': { status: 200, body: { id: 'a/b', name: 'Slash' } },
    });
    const out = await loadOwnerNames(['a/b'], fn);
    expect(asked).toEqual(['/api/people/a%2Fb']);
    expect(out.names.get('a/b')).toBe('Slash');
  });

  test('a refused read is a failure that names its read, and the names that did load are kept', async () => {
    const { fn } = fakeFetch({
      '/api/people/emp-a': { status: 200, body: { id: 'emp-a', name: 'Ada' } },
      '/api/people/emp-b': { status: 503, body: 'people down' },
    });
    const out = await loadOwnerNames(['emp-a', 'emp-b'], fn);
    expect(out.read).toEqual({ kind: 'failed', error: '/api/people/emp-b: HTTP 503' });
    expect(out.names.get('emp-a')).toBe('Ada');
    expect(out.names.has('emp-b')).toBe(false);
  });

  test('a network error is a failure too, not an ignored catch', async () => {
    const { fn } = fakeFetch({ '/api/people/emp-a': new Error('connection reset') });
    const out = await loadOwnerNames(['emp-a'], fn);
    expect(out.read).toEqual({ kind: 'failed', error: '/api/people/emp-a: connection reset' });
  });

  test('an employee with no name yet is not a failed read — the id is the honest label', async () => {
    const { fn } = fakeFetch({
      '/api/people/emp-a': { status: 200, body: { id: 'emp-a', name: null } },
    });
    const out = await loadOwnerNames(['emp-a'], fn);
    expect(out.read).toEqual({ kind: 'ok' });
    expect(out.names.has('emp-a')).toBe(false);
  });
});
