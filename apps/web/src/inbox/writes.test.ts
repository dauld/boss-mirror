// The inbox's two writes — Mark read and Send — each swallowed a
// refusal: `markRead` never read `r.ok` and caught every throw into
// nothing, and `send` read `r.ok` only to close the modal, with no
// catch at all (backlog 129da587 and 2a6fab80, page audit 5477d9eb
// GAPs 5 and 4). `postWrite` is the one place either write resolves,
// and it can only resolve done or refused-with-a-reason — so a page
// that stores its answer has a sentence to render for every failure.

import { afterEach, describe, expect, test } from 'bun:test';
import { postEach, postWrite } from './writes';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function stubFetch(fn: (url: string, init?: RequestInit) => Promise<Response>) {
  globalThis.fetch = fn as unknown as typeof fetch;
}

describe('postWrite', () => {
  test('a 2xx is done, and the write went out as a POST', async () => {
    const seen: Array<{ url: string; method: string | undefined }> = [];
    stubFetch(async (url, init) => {
      seen.push({ url, method: init?.method });
      return new Response(null, { status: 204 });
    });
    expect(await postWrite('/api/messages/m1/read')).toEqual({ kind: 'done' });
    expect(seen).toEqual([{ url: '/api/messages/m1/read', method: 'POST' }]);
  });

  test('a refusal names its status and carries the server message', async () => {
    stubFetch(async () => new Response('not your message', { status: 403 }));
    expect(await postWrite('/api/messages/m1/read')).toEqual({
      kind: 'refused',
      reason: 'HTTP 403: not your message',
    });
  });

  test('a refusal with an empty body still names its status', async () => {
    stubFetch(async () => new Response('', { status: 500 }));
    expect(await postWrite('/api/messages/send', { body: '{}' })).toEqual({
      kind: 'refused',
      reason: 'HTTP 500',
    });
  });

  test('a thrown fetch is refused with its message — never an unhandled rejection', async () => {
    stubFetch(async () => {
      throw new TypeError('Failed to fetch');
    });
    expect(await postWrite('/api/messages/send')).toEqual({
      kind: 'refused',
      reason: 'Failed to fetch',
    });
  });

  test('the caller init rides along, with the method pinned to POST', async () => {
    const seen: Array<RequestInit | undefined> = [];
    stubFetch(async (_url, init) => {
      seen.push(init);
      return new Response('{}', { status: 200 });
    });
    await postWrite('/api/messages/send', {
      headers: { 'Content-Type': 'application/json' },
      body: '{"a":1}',
    });
    expect(seen.map((i) => [i?.method, i?.body])).toEqual([['POST', '{"a":1}']]);
  });
});

// The bulk controls (backlog 5963a322, GAP 8): Mark all read and
// Archive selected are one POST per message — the server records one
// event per row — and every refusal is kept against its own row, so a
// bulk write that half-lands says which half.
describe('postEach', () => {
  test('one POST per id, in order, each to its own url', async () => {
    const seen: Array<[string, string | undefined]> = [];
    stubFetch(async (url, init) => {
      seen.push([url, init?.method]);
      return new Response(null, { status: 204 });
    });
    const out = await postEach(['a', 'b'], (id) => `/api/messages/${id}/archive`);
    expect(seen).toEqual([
      ['/api/messages/a/archive', 'POST'],
      ['/api/messages/b/archive', 'POST'],
    ]);
    expect(out).toEqual({ done: ['a', 'b'], refused: {} });
  });

  test('a refusal is kept against its id and does not stop the rest', async () => {
    stubFetch(async (url) =>
      url.includes('/b/')
        ? new Response('not your message', { status: 403 })
        : new Response(null, { status: 204 }),
    );
    const out = await postEach(['a', 'b', 'c'], (id) => `/api/messages/${id}/read`);
    expect(out).toEqual({
      done: ['a', 'c'],
      refused: { b: 'HTTP 403: not your message' },
    });
  });

  test('nothing asked is nothing done', async () => {
    stubFetch(async () => {
      throw new Error('no write should go out');
    });
    expect(await postEach([], (id) => id)).toEqual({ done: [], refused: {} });
  });
});
