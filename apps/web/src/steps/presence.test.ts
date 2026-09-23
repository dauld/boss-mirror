import { afterEach, describe, expect, test } from 'bun:test';
import { performPresenceCeremony } from './presence';

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

const failureOf = async (): Promise<string> => {
  try {
    await performPresenceCeremony('job-1', 'step-1');
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
