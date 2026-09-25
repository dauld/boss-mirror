import { afterEach, describe, expect, test } from 'bun:test';
import { completeWithPresence, performPresenceCeremony, shownAfter } from './presence';

// Backlog 2e893e27 (2026-09-21): performPresenceCeremony caught
// navigator.credentials.get with a bare `catch {` and threw one
// sentence — 'declined or timed out' — for every outcome, and threw
// away the gateway's own refusal text on begin and finish. These drive
// the ceremony itself, not only the message table, because the defect
// was at the call site: a correct table the site never consults fixes
// nothing (the enrolment half, f1fd9168, was the same shape).

const realFetch = globalThis.fetch;
const realNavigator = globalThis.navigator;

afterEach(() => {
  globalThis.fetch = realFetch;
  Object.defineProperty(globalThis, 'navigator', {
    value: realNavigator,
    configurable: true,
  });
});

const BEGIN = {
  challenge_id: 'c-1',
  publicKey: {
    challenge: 'AAAA',
    rpId: 'example.test',
    allowCredentials: [{ type: 'public-key', id: 'AQID' }],
    userVerification: 'required',
    timeout: 60000,
  },
};

type Answer = Readonly<{ status: number; body: string }>;

/** Route the two ceremony endpoints to canned answers. */
const stubFetch = (begin: Answer, finish?: Answer) => {
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = String(input);
    const a = url.endsWith('/assert/begin') ? begin : finish;
    if (!a) throw new Error(`unexpected fetch ${url}`);
    return new Response(a.body, { status: a.status });
  }) as unknown as typeof fetch;
};

/** A browser whose passkey prompt resolves or rejects as given. */
const stubBrowser = (get: () => Promise<unknown>) => {
  Object.defineProperty(globalThis, 'navigator', {
    value: { credentials: { get } },
    configurable: true,
  });
};

const domError = (name: string) => {
  const e = new Error(`${name} raised`);
  e.name = name;
  return e;
};

const SHOWN = {
  title: 'Approve the plan: wipe on forge',
  metadata: { plan: 'PLAN wipe target-a\n', args: ['target-a'] },
} as const;

const failureOf = async (): Promise<string> => {
  try {
    await performPresenceCeremony('job-1', 'step-1', SHOWN);
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
  throw new Error('the ceremony was expected to fail');
};

describe('a presence ceremony failure says WHICH failure', () => {
  test('an origin mismatch is not reported as declined or timed out', async () => {
    stubFetch({ status: 200, body: JSON.stringify(BEGIN) });
    stubBrowser(() => Promise.reject(domError('SecurityError')));
    const msg = await failureOf();
    expect(msg).toContain('SecurityError');
    expect(msg).not.toContain('declined or timed out');
  });

  test('a prompt that named no passkey says so', async () => {
    const empty = {
      ...BEGIN,
      publicKey: { ...BEGIN.publicKey, allowCredentials: [] },
    };
    stubFetch({ status: 200, body: JSON.stringify(empty) });
    stubBrowser(() => Promise.reject(domError('NotAllowedError')));
    expect(await failureOf()).toContain('did not name any of your passkeys');
  });

  test('the gateway refusing to begin carries its own reason', async () => {
    stubFetch({ status: 502, body: 'challenge mint failed' });
    stubBrowser(() => Promise.reject(new Error('never reached')));
    const msg = await failureOf();
    expect(msg).toContain('502');
    expect(msg).toContain('challenge mint failed');
  });

  test('the gateway refusing the signature carries its own reason', async () => {
    stubFetch(
      { status: 200, body: JSON.stringify(BEGIN) },
      { status: 410, body: 'challenge already spent or expired — begin again' },
    );
    const bytes = new Uint8Array([1, 2, 3]).buffer;
    stubBrowser(() =>
      Promise.resolve({
        id: 'AQID',
        rawId: bytes,
        type: 'public-key',
        response: {
          authenticatorData: bytes,
          clientDataJSON: bytes,
          signature: bytes,
          userHandle: null,
        },
      }),
    );
    const msg = await failureOf();
    expect(msg).toContain('410');
    expect(msg).toContain('challenge already spent or expired');
  });
});

// Backlog fd7090cc, the security re-review of 2026-09-25: the begin sent
// only the ids, so the gateway bound the challenge to the step as it
// read it at that instant — not to what the approver had on screen. The
// begin now names the step content the surface SHOWED, and the gateway
// refuses (412) a begin whose shown content is not the step as it stands.
describe('a presence ceremony signs what was shown', () => {
  test('the begin carries the ids and the shown title and metadata', async () => {
    let sent: unknown = null;
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input).endsWith('/assert/begin')) sent = JSON.parse(String(init?.body));
      return new Response('changed', { status: 412 });
    }) as unknown as typeof fetch;
    await failureOf();
    expect(sent).toEqual({ job_id: 'job-1', step_id: 'step-1', shown: SHOWN });
  });

  test('a step that changed since it was shown says so', async () => {
    stubFetch({
      status: 412,
      body: 'this step changed since it was shown — reload it and read it again before approving',
    });
    const msg = await failureOf();
    expect(msg).toContain('changed since it was shown');
    expect(msg).not.toContain('No passkey enrolled');
  });
});

// Backlog 3ce3c15f (review of car 5b30ccf9, 2026-09-25): a completion
// the jobs API refuses for presence — no ticket held after a reload, a
// held one past its two-minute life, or a step whose sign-offs the user
// does not carry — used to stop the surface at the raw 422. It is now
// answered with ONE ceremony on the step as shown and ONE retry carrying
// the ticket that ceremony was issued. Never a loop; never a ticket the
// gateway did not issue.
describe('completeWithPresence answers a presence refusal once', () => {
  const STEP_PUT = '/api/jobs/job-1/steps/step-1';
  const REFUSAL = JSON.stringify({ error: 'step requires stronger assurance', required: 'presence' });
  const bytes = new Uint8Array([1, 2, 3]).buffer;
  const withPasskey = () =>
    stubBrowser(() =>
      Promise.resolve({
        id: 'AQID',
        rawId: bytes,
        type: 'public-key',
        response: { authenticatorData: bytes, clientDataJSON: bytes, signature: bytes, userHandle: null },
      }),
    );

  /** A jobs API + gateway pair: ceremonies issue `fresh-n`; the step PUT
   *  honours only `accepts`; records what every PUT and begin carried. */
  const server = (accepts: (ticket: string | null) => boolean, begin?: Answer) => {
    const seen = { puts: [] as (string | null)[], bodies: [] as unknown[], begins: [] as unknown[] };
    let n = 0;
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith('/assert/begin')) {
        seen.begins.push(JSON.parse(String(init?.body)));
        return begin
          ? new Response(begin.body, { status: begin.status })
          : new Response(JSON.stringify(BEGIN), { status: 200 });
      }
      if (url.endsWith('/assert/finish')) {
        n += 1;
        return new Response(JSON.stringify({ ticket: `fresh-${n}` }), { status: 200 });
      }
      if (url === STEP_PUT && init?.method === 'PUT') {
        const t = new Headers(init.headers).get('x-presence-ticket');
        seen.puts.push(t);
        seen.bodies.push(JSON.parse(String(init.body)));
        return accepts(t)
          ? new Response('{}', { status: 200 })
          : new Response(REFUSAL, { status: 422 });
      }
      throw new Error(`unexpected fetch ${url}`);
    }) as unknown as typeof fetch;
    return seen;
  };

  test('a held ticket the server honours completes with no ceremony', async () => {
    const seen = server((t) => t === 'held');
    const res = await completeWithPresence('job-1', 'step-1', SHOWN, 'held');
    expect(res.kind).toBe('ok');
    expect(seen.puts).toEqual(['held']);
    expect(seen.begins).toEqual([]);
    expect(seen.bodies).toEqual([{ status: 'completed' }]);
  });

  test('no ticket held (a reload after the stamp): one ceremony on the shown step, one retry', async () => {
    withPasskey();
    const seen = server((t) => t === 'fresh-1');
    const res = await completeWithPresence('job-1', 'step-1', SHOWN);
    expect(res.kind).toBe('ok');
    expect(seen.puts).toEqual([null, 'fresh-1']);
    expect(seen.begins).toEqual([{ job_id: 'job-1', step_id: 'step-1', shown: SHOWN }]);
  });

  test('a held ticket past its life: one fresh ceremony, and the retry carries the fresh ticket', async () => {
    withPasskey();
    const seen = server((t) => t === 'fresh-1');
    const res = await completeWithPresence('job-1', 'step-1', SHOWN, 'expired');
    expect(res.kind).toBe('ok');
    expect(seen.puts).toEqual(['expired', 'fresh-1']);
    expect(seen.begins.length).toBe(1);
  });

  test('refused again after the fresh tap: failed, named, and never a second ceremony', async () => {
    withPasskey();
    const seen = server(() => false);
    const res = await completeWithPresence('job-1', 'step-1', SHOWN);
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') {
      expect(res.error).toContain('refused again after a fresh passkey tap');
      expect(res.error).toContain('422');
    }
    expect(seen.puts).toEqual([null, 'fresh-1']);
    expect(seen.begins.length).toBe(1);
  });

  // Backlog d82b5f60: every refusal of the retry read "refused again after
  // a fresh passkey tap", so a 409 for stale stamps looked like presence.
  test('a retry refused for another reason carries its own reason, not "refused again"', async () => {
    withPasskey();
    let puts = 0;
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith('/assert/begin')) return new Response(JSON.stringify(BEGIN), { status: 200 });
      if (url.endsWith('/assert/finish'))
        return new Response(JSON.stringify({ ticket: 'fresh-1' }), { status: 200 });
      puts += 1;
      return puts === 1
        ? new Response(REFUSAL, { status: 422 })
        : new Response(JSON.stringify({ missing_or_stale_roles: ['ceo'] }), { status: 409 });
    }) as unknown as typeof fetch;
    const res = await completeWithPresence('job-1', 'step-1', SHOWN);
    expect(res).toEqual({ kind: 'failed', error: 'sign-offs outstanding: ceo' });
    expect(puts).toBe(2);
  });

  test('a ceremony that fails is named, and the completion is not re-sent', async () => {
    const seen = server(() => false, { status: 409, body: 'no passkey' });
    const res = await completeWithPresence('job-1', 'step-1', SHOWN);
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') expect(res.error).toContain('No passkey enrolled');
    expect(seen.puts).toEqual([null]);
  });

  test('a refusal that is not about presence is returned as it is, with no ceremony', async () => {
    const seen = { begins: 0, puts: 0 };
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      if (String(input).includes('/assert/')) seen.begins += 1;
      else seen.puts += 1;
      return new Response(JSON.stringify({ missing_or_stale_roles: ['ceo'] }), { status: 409 });
    }) as unknown as typeof fetch;
    const res = await completeWithPresence('job-1', 'step-1', SHOWN);
    expect(res).toEqual({ kind: 'failed', error: 'sign-offs outstanding: ceo' });
    expect(seen).toEqual({ begins: 0, puts: 1 });
  });
});

describe('shownAfter folds a surface’s own write the way the merge door does', () => {
  test('a key sent is set, a key sent as null or undefined is deleted', () => {
    expect(
      shownAfter({ a: 1, b: 'x', c: true }, { b: 'y', c: null, d: 'new', e: undefined }),
    ).toEqual({ a: 1, b: 'y', d: 'new' });
  });

  test('the rendered metadata is not mutated', () => {
    const rendered = { a: 1 };
    shownAfter(rendered, { a: 2 });
    expect(rendered).toEqual({ a: 1 });
  });
});
