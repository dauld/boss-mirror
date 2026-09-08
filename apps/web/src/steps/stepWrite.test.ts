// The shared write path for platform step surfaces (packet cc9d7fc6).
// The class under test: a non-ok PUT must come back as a visible,
// typed failure — never a silent success — and the failure message
// must carry whatever the server said about why.

import { afterEach, describe, expect, test } from 'bun:test';
import { WRITE_RETRY, describeWriteFailure, putStep, writeStep } from './stepWrite';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function stubFetch(fn: (url: string, init?: RequestInit) => Promise<Response>) {
  globalThis.fetch = fn as unknown as typeof fetch;
}

// The retry backoff must not be spent in tests — the same no_wait
// policy the CLI's retry tests use.
const noWait = { sleep: async () => {} };

describe('describeWriteFailure', () => {
  test('uses the JSON error field when the server sent one', () => {
    expect(
      describeWriteFailure(400, JSON.stringify({ error: 'scheduled_at is required at done' })),
    ).toBe('HTTP 400 — scheduled_at is required at done');
  });

  test('uses message / detail fields as fallbacks', () => {
    expect(describeWriteFailure(422, JSON.stringify({ message: 'no' }))).toBe('HTTP 422 — no');
    expect(describeWriteFailure(403, JSON.stringify({ detail: 'denied' }))).toBe('HTTP 403 — denied');
  });

  test('names outstanding sign-off roles on the 409 conflict shape', () => {
    const body = JSON.stringify({ missing_or_stale_roles: ['ceo', 'qa-lead'] });
    expect(describeWriteFailure(409, body)).toBe('sign-offs outstanding: ceo, qa-lead');
  });

  test('falls back to plain text, and to the bare status when empty', () => {
    expect(describeWriteFailure(500, 'boom')).toBe('HTTP 500 — boom');
    expect(describeWriteFailure(500, '')).toBe('HTTP 500');
    expect(describeWriteFailure(400, '   ')).toBe('HTTP 400');
  });

  test('a bare JSON string body reads as the message', () => {
    expect(describeWriteFailure(404, JSON.stringify('not found'))).toBe('HTTP 404 — not found');
  });

  test('truncates a long body instead of flooding the surface', () => {
    const msg = describeWriteFailure(500, 'x'.repeat(1000));
    expect(msg.length).toBeLessThanOrEqual(220);
    expect(msg.startsWith('HTTP 500 — ')).toBe(true);
  });
});

describe('writeStep', () => {
  test('an ok response is kind ok and carries the response', async () => {
    stubFetch(async () => new Response('{}', { status: 200 }));
    const res = await writeStep('/api/x', { method: 'PUT' });
    expect(res.kind).toBe('ok');
    if (res.kind === 'ok') expect(res.response.status).toBe(200);
  });

  test('a 400 is kind failed with the server message — never silent', async () => {
    stubFetch(
      async () => new Response(JSON.stringify({ error: 'bad shape' }), { status: 400 }),
    );
    const res = await writeStep('/api/x', { method: 'PUT' });
    expect(res).toEqual({ kind: 'failed', error: 'HTTP 400 — bad shape' });
  });

  test('a thrown fetch (network down) is kind failed, not an exception', async () => {
    stubFetch(async () => {
      throw new TypeError('Failed to fetch');
    });
    const res = await writeStep('/api/x', { method: 'PUT' }, noWait);
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') expect(res.error).toContain('Failed to fetch');
  });
});

describe('writeStep — deploy-roll retry (packet 04cc82ab)', () => {
  test('a 503 on an idempotent PUT is retried and then succeeds', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      return calls < 3
        ? new Response('rolling', { status: 503 })
        : new Response('{}', { status: 200 });
    });
    const res = await writeStep('/api/x', { method: 'PUT' }, noWait);
    expect(res.kind).toBe('ok');
    expect(calls).toBe(3);
  });

  test('a refused connection on a PUT is retried across the roll', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      if (calls < 2) throw new TypeError('Failed to fetch');
      return new Response('{}', { status: 200 });
    });
    const res = await writeStep('/api/x', { method: 'PUT' }, noWait);
    expect(res.kind).toBe('ok');
    expect(calls).toBe(2);
  });

  test('a 503 that outlasts the budget surfaces the failure, bounded', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      return new Response('still down', { status: 503 });
    });
    const res = await writeStep('/api/x', { method: 'PUT' }, noWait);
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') expect(res.error).toContain('503');
    expect(calls).toBe(WRITE_RETRY.attempts);
  });

  test('a 4xx is an answer, never retried', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      return new Response(JSON.stringify({ error: 'bad shape' }), { status: 400 });
    });
    const res = await writeStep('/api/x', { method: 'PUT' }, noWait);
    expect(res).toEqual({ kind: 'failed', error: 'HTTP 400 — bad shape' });
    expect(calls).toBe(1);
  });

  test('a 500 is an application answer — surfaced now, not after a backoff', async () => {
    // The app RAN and failed (db down). Retrying it would only hide the
    // error behind the roll budget; a roll is a 502/503/504, not a 500.
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      return new Response(JSON.stringify({ error: 'db down' }), { status: 500 });
    });
    const res = await writeStep('/api/x', { method: 'PUT' }, noWait);
    expect(res).toEqual({ kind: 'failed', error: 'HTTP 500 — db down' });
    expect(calls).toBe(1);
  });

  test('a non-idempotent POST is NOT retried — one blip must not become two creates', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      return new Response('rolling', { status: 503 });
    });
    const res = await writeStep('/api/x', { method: 'POST' }, noWait);
    expect(res.kind).toBe('failed');
    expect(calls).toBe(1);
  });

  test('a POST that throws is NOT retried either — the send may have landed', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      throw new TypeError('Failed to fetch');
    });
    const res = await writeStep('/api/x', { method: 'POST' }, noWait);
    expect(res.kind).toBe('failed');
    expect(calls).toBe(1);
  });
});

describe('putStep', () => {
  test('PUTs the JSON body to the step endpoint', async () => {
    let seenUrl = '';
    let seenInit: RequestInit | undefined;
    stubFetch(async (url, init) => {
      seenUrl = url;
      seenInit = init;
      return new Response('{}', { status: 200 });
    });
    const res = await putStep('job-1', 'step-9', { status: 'completed' });
    expect(res.kind).toBe('ok');
    expect(seenUrl).toBe('/api/jobs/job-1/steps/step-9');
    expect(seenInit?.method).toBe('PUT');
    expect(JSON.parse(String(seenInit?.body))).toEqual({ status: 'completed' });
  });
});
