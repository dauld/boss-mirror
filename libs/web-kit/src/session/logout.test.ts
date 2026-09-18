// Sign out navigates to /login only after the gateway CONFIRMED the
// logout. Until 2026-09-18 SignInControl POSTed /api/auth/logout and
// set window.location whatever the answer — "best-effort, redirect
// regardless" — so a refused logout looked exactly like a successful
// one while the session stayed live: the chrome said signed out and
// the record said otherwise. Found by the interaction crawl's
// refused-write leg (car f09aafe1, KNOWN_GAPS #5; backlog a5dff6f1).
//
// These tests pin the ONE judgement the control renders from: a 2xx is
// signed-out; anything else is a refusal that names its status and the
// body's reason; a fetch that never reached the gateway is unreachable.
// The control stays on the page and shows the last two.

import { describe, expect, test } from 'bun:test';
import { requestLogout, type LogoutFetch } from './logout';

const answer = (status: number, body: unknown): LogoutFetch => async () =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

describe('requestLogout', () => {
  test('a 2xx is a confirmed sign-out', async () => {
    expect(await requestLogout(answer(200, {}))).toEqual({ kind: 'signed-out' });
    expect(await requestLogout(answer(204, ''))).toEqual({ kind: 'signed-out' });
  });

  test('a refusal carries the status and the reason the body gives', async () => {
    const out = await requestLogout(answer(403, { error: 'refused by policy: no' }));
    expect(out).toEqual({ kind: 'refused', status: 403, detail: 'refused by policy: no' });
  });

  test('a refusal with no readable reason still names its status', async () => {
    const out = await requestLogout(async () => new Response('', { status: 500 }));
    expect(out).toEqual({ kind: 'refused', status: 500, detail: '' });
  });

  test('a fetch that never reached the gateway is unreachable, not signed out', async () => {
    const out = await requestLogout(async () => {
      throw new TypeError('Failed to fetch');
    });
    expect(out).toEqual({ kind: 'unreachable', detail: 'Failed to fetch' });
  });

  test('it POSTs the logout endpoint', async () => {
    const seen: Array<{ url: string; method: string | undefined }> = [];
    await requestLogout(async (url, init) => {
      seen.push({ url, method: init?.method });
      return new Response('', { status: 200 });
    });
    expect(seen).toEqual([{ url: '/api/auth/logout', method: 'POST' }]);
  });
});
