import { afterEach, describe, expect, test } from 'bun:test';
import {
  canonical,
  completeWithPresence,
  NotShownRefusal,
  notShown,
  performPresenceCeremony,
  scrollNote,
  shownAfter,
  signedRows,
  signedText,
} from './presence';
import { genString, genValue, rng, TRICKY } from './signedInputs.testkit';

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

/** The surface's answer to "what is on screen now": exactly SHOWN. */
const ON_SCREEN = () => SHOWN;

const failureOf = async (): Promise<string> => {
  try {
    await performPresenceCeremony('job-1', 'step-1', SHOWN, ON_SCREEN);
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
    const res = await completeWithPresence('job-1', 'step-1', SHOWN, ON_SCREEN, 'held');
    expect(res.kind).toBe('ok');
    expect(seen.puts).toEqual(['held']);
    expect(seen.begins).toEqual([]);
    expect(seen.bodies).toEqual([{ status: 'completed' }]);
  });

  test('no ticket held (a reload after the stamp): one ceremony on the shown step, one retry', async () => {
    withPasskey();
    const seen = server((t) => t === 'fresh-1');
    const res = await completeWithPresence('job-1', 'step-1', SHOWN, ON_SCREEN);
    expect(res.kind).toBe('ok');
    expect(seen.puts).toEqual([null, 'fresh-1']);
    expect(seen.begins).toEqual([{ job_id: 'job-1', step_id: 'step-1', shown: SHOWN }]);
  });

  test('a held ticket past its life: one fresh ceremony, and the retry carries the fresh ticket', async () => {
    withPasskey();
    const seen = server((t) => t === 'fresh-1');
    const res = await completeWithPresence('job-1', 'step-1', SHOWN, ON_SCREEN, 'expired');
    expect(res.kind).toBe('ok');
    expect(seen.puts).toEqual(['expired', 'fresh-1']);
    expect(seen.begins.length).toBe(1);
  });

  test('refused again after the fresh tap: failed, named, and never a second ceremony', async () => {
    withPasskey();
    const seen = server(() => false);
    const res = await completeWithPresence('job-1', 'step-1', SHOWN, ON_SCREEN);
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
    const res = await completeWithPresence('job-1', 'step-1', SHOWN, ON_SCREEN);
    expect(res).toEqual({ kind: 'failed', error: 'sign-offs outstanding: ceo' });
    expect(puts).toBe(2);
  });

  test('a ceremony that fails is named, and the completion is not re-sent', async () => {
    const seen = server(() => false, { status: 409, body: 'no passkey' });
    const res = await completeWithPresence('job-1', 'step-1', SHOWN, ON_SCREEN);
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
    const res = await completeWithPresence('job-1', 'step-1', SHOWN, ON_SCREEN);
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

// Design f623e425 D3 (backlog 6c9183de, extends b and c, 2026-09-25): the
// passkey binds step_shape_hash(title, metadata) — EVERY metadata key —
// and ApprovalSurface rendered only `decision` and `comment`. A planted
// plan, verb, host, args, rendered_plan_sha256 or decision was signed on
// one tap, unseen. The rows a surface renders are now derived from the
// very object the begin names, and the ceremony refuses, before any
// request, a key that is not on screen as it would be signed.
describe('a passkey signs only what the surface put on screen', () => {
  const OPS_APPROVE = {
    title: 'Approve the plan: wipe on forge',
    metadata: {
      plan: 'PLAN wipe target-a\n  /dev/sdb  by-id/ata-X\n',
      verb: 'wipe',
      host: 'forge',
      args: ['target-a'],
      rendered_plan_sha256: 'ab'.repeat(32),
      decision: 'approved',
      decided_at: '2026-09-25T15:00:00Z',
      planted_by_someone_else: { anything: true },
    },
  } as const;

  test('the rows are every key the shape hash covers, and nothing else', () => {
    const rows = signedRows(OPS_APPROVE);
    expect(rows.map((r) => r.key)).toEqual(Object.keys(OPS_APPROVE.metadata).sort());
    for (const k of ['plan', 'verb', 'host', 'args', 'rendered_plan_sha256', 'decision']) {
      expect(rows.some((r) => r.key === k)).toBe(true);
    }
  });

  test('a plain string renders as itself; a multi-line one quoted; anything else as its JSON', () => {
    const rows = new Map(signedRows(OPS_APPROVE).map((r) => [r.key, r.text]));
    expect(rows.get('verb')).toBe('wipe');
    // Every line break is drawn as \n before it breaks, so a line the box
    // wraps cannot be mistaken for a line the bytes break (6093cf13).
    expect(rows.get('plan')).toBe('"PLAN wipe target-a\\n\n  /dev/sdb  by-id/ata-X\\n\n"');
    expect(rows.get('args')).toBe(JSON.stringify(['target-a'], null, 2));
    expect(rows.get('planted_by_someone_else')).toContain('"anything": true');
  });

  test('nothing is unshown when the screen holds exactly what is signed', () => {
    expect(notShown(OPS_APPROVE, { ...OPS_APPROVE })).toEqual([]);
  });

  test('a key planted after the render is named, and so is a value that moved', () => {
    const onScreen = {
      title: OPS_APPROVE.title,
      metadata: { ...OPS_APPROVE.metadata, host: 'boss-gcp' } as Record<string, unknown>,
    };
    delete onScreen.metadata.planted_by_someone_else;
    expect(notShown(OPS_APPROVE, onScreen)).toEqual(['host', 'planted_by_someone_else']);
  });

  test('another title on screen is named; nothing on screen names every key', () => {
    expect(notShown(OPS_APPROVE, { ...OPS_APPROVE, title: 'Approve the hire' })).toEqual([
      'title',
    ]);
    expect(notShown(OPS_APPROVE, null)).toEqual([
      'title',
      ...Object.keys(OPS_APPROVE.metadata).sort(),
    ]);
  });

  test('a key shown and not signed is not a refusal — only an unshown signed one is', () => {
    const onScreen = {
      title: OPS_APPROVE.title,
      metadata: { ...OPS_APPROVE.metadata, extra_on_screen: 1 },
    };
    expect(notShown(OPS_APPROVE, onScreen)).toEqual([]);
  });

  test('the ceremony refuses an unshown key before any request, naming it', async () => {
    let fetched = 0;
    globalThis.fetch = (async () => {
      fetched += 1;
      return new Response(JSON.stringify(BEGIN), { status: 200 });
    }) as unknown as typeof fetch;
    const onScreen = { title: SHOWN.title, metadata: { plan: SHOWN.metadata.plan } };
    let refusal: unknown = null;
    try {
      await performPresenceCeremony('job-1', 'step-1', SHOWN, () => onScreen);
    } catch (e) {
      refusal = e;
    }
    expect(refusal).toBeInstanceOf(NotShownRefusal);
    expect((refusal as NotShownRefusal).keys).toEqual(['args']);
    expect((refusal as Error).message).toContain('args');
    expect(fetched).toBe(0);
  });

  test('a completion whose recovery tap would sign an unshown key is failed, not re-sent', async () => {
    const puts: (string | null)[] = [];
    let begins = 0;
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input).includes('/assert/')) {
        begins += 1;
        return new Response(JSON.stringify(BEGIN), { status: 200 });
      }
      puts.push(new Headers(init?.headers).get('x-presence-ticket'));
      return new Response(JSON.stringify({ required: 'presence' }), { status: 422 });
    }) as unknown as typeof fetch;
    const res = await completeWithPresence('job-1', 'step-1', SHOWN, () => null);
    expect(res.kind).toBe('failed');
    if (res.kind === 'failed') expect(res.error).toContain('does not show');
    expect(begins).toBe(0);
    expect(puts).toEqual([null]);
  });
});

// Backlog 7c53b1bf (adversarial review of car fcda5f8b, 2026-09-25): the
// ceremony read `onScreen()` once, before the begin. A rail switch or a
// navigation during the begin round trip then let the prompt come up over
// the step now shown and sign the one that was not — the plugin's copy
// re-checks after the begin and after the credential, and this one did
// not. It now reads the screen again at every await it crosses, hands the
// prompt the caller's abort signal, and never returns a ticket for a step
// that has left the screen, so no stamp is recorded for it.
describe('a ceremony whose step leaves the screen mid-flight signs nothing', () => {
  const OTHER = { title: 'Approve the hire', metadata: { plan: 'PLAN hire' } } as const;
  const credential = () => {
    const bytes = new Uint8Array([1, 2, 3]).buffer;
    return {
      id: 'AQID',
      rawId: bytes,
      type: 'public-key',
      response: { authenticatorData: bytes, clientDataJSON: bytes, signature: bytes, userHandle: null },
    };
  };
  /** Routes begin and finish; `during` runs inside the named one. */
  const ceremonyServer = (during: Partial<Record<'begin' | 'finish', () => void>>) => {
    const urls: string[] = [];
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input);
      urls.push(url);
      if (url.endsWith('/assert/begin')) {
        during.begin?.();
        return new Response(JSON.stringify(BEGIN), { status: 200 });
      }
      during.finish?.();
      return new Response(JSON.stringify({ ticket: 'ticket-1' }), { status: 200 });
    }) as unknown as typeof fetch;
    return urls;
  };
  const outcome = async (
    onScreen: () => typeof SHOWN | typeof OTHER | null,
    signal?: AbortSignal,
  ): Promise<unknown> => {
    try {
      return await performPresenceCeremony('job-1', 'step-1', SHOWN, onScreen, signal);
    } catch (e) {
      return e;
    }
  };

  test('control: the step stays on screen, and the ceremony returns its ticket', async () => {
    const urls = ceremonyServer({});
    stubBrowser(async () => credential());
    expect(await outcome(() => SHOWN)).toBe('ticket-1');
    expect(urls.map((u) => u.split('/').pop())).toEqual(['begin', 'finish']);
  });

  test('the rail moves on during the begin: the passkey is never asked', async () => {
    let screen: typeof SHOWN | typeof OTHER = SHOWN;
    const urls = ceremonyServer({ begin: () => (screen = OTHER) });
    let asked = 0;
    stubBrowser(async () => {
      asked += 1;
      return credential();
    });
    const result = await outcome(() => screen);
    expect(result).toBeInstanceOf(NotShownRefusal);
    expect(asked).toBe(0);
    expect(urls.some((u) => u.endsWith('/assert/finish'))).toBe(false);
  });

  test('the surface unmounts during the begin: the passkey is never asked', async () => {
    let screen: typeof SHOWN | null = SHOWN;
    ceremonyServer({ begin: () => (screen = null) });
    let asked = 0;
    stubBrowser(async () => {
      asked += 1;
      return credential();
    });
    expect(await outcome(() => screen)).toBeInstanceOf(NotShownRefusal);
    expect(asked).toBe(0);
  });

  test('the rail moves on while the prompt is up: its answer is never sent', async () => {
    let screen: typeof SHOWN | typeof OTHER = SHOWN;
    const urls = ceremonyServer({});
    stubBrowser(async () => {
      screen = OTHER;
      return credential();
    });
    expect(await outcome(() => screen)).toBeInstanceOf(NotShownRefusal);
    expect(urls.some((u) => u.endsWith('/assert/finish'))).toBe(false);
  });

  test('the rail moves on while the finish is in flight: no ticket comes back to be stamped', async () => {
    let screen: typeof SHOWN | typeof OTHER = SHOWN;
    ceremonyServer({ finish: () => (screen = OTHER) });
    stubBrowser(async () => credential());
    expect(await outcome(() => screen)).toBeInstanceOf(NotShownRefusal);
  });

  test('the prompt is handed the caller’s signal, and an abort reads as nothing signed', async () => {
    let screen: typeof SHOWN | null = SHOWN;
    ceremonyServer({});
    const gesture = new AbortController();
    let handed: AbortSignal | undefined;
    stubBrowser((opts?: unknown) => {
      handed = (opts as { signal?: AbortSignal } | undefined)?.signal;
      // The surface goes away while the prompt is up; the browser rejects
      // an aborted prompt with an AbortError.
      screen = null;
      gesture.abort();
      return Promise.reject(domError('AbortError'));
    });
    const result = await outcome(() => screen, gesture.signal);
    expect(handed).toBe(gesture.signal);
    expect(result).toBeInstanceOf(NotShownRefusal);
    expect((result as Error).message).toContain('Nothing was signed');
  });

  test('completeWithPresence hands its ceremony the same signal', async () => {
    const gesture = new AbortController();
    let handed: AbortSignal | undefined;
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url.endsWith('/assert/begin')) return new Response(JSON.stringify(BEGIN), { status: 200 });
      if (url.endsWith('/assert/finish')) {
        return new Response(JSON.stringify({ ticket: 'ticket-1' }), { status: 200 });
      }
      const ticket = new Headers(init?.headers).get('x-presence-ticket');
      return ticket
        ? new Response(JSON.stringify({ status: 'completed' }), { status: 200 })
        : new Response(JSON.stringify({ required: 'presence' }), { status: 422 });
    }) as unknown as typeof fetch;
    stubBrowser((opts?: unknown) => {
      handed = (opts as { signal?: AbortSignal } | undefined)?.signal;
      return Promise.resolve(credential());
    });
    const res = await completeWithPresence('job-1', 'step-1', SHOWN, ON_SCREEN, undefined, gesture.signal);
    expect(res.kind).toBe('ok');
    expect(handed).toBe(gesture.signal);
  });
});

// Backlog 6093cf13 (adversarial review of car 30674304, 2026-09-25): the
// check above compares BYTES to the bytes the surface rendered from, and
// the rendering drew a string byte for byte — so '42' and 42 drew alike, a
// JSON string drew like the object it encodes, a bidi override reordered
// what the approver read, and a zero-width space, a Cyrillic "о" or a
// trailing space were invisible. The passkey signs bytes; the approver
// reads glyphs. The rendering now draws any string that could be misread
// in double quotes, with every character that is not printable ASCII (or
// an em dash) written as an escape — so two values the passkey would sign
// differently can never be drawn alike.
describe('a signed value is drawn as the bytes it is', () => {
  const DRAWN: readonly (readonly [unknown, string])[] = [
    ['wipe', 'wipe'],
    ['forge-01.lan', 'forge-01.lan'],
    ['2026-09-25T15:00:00Z', '2026-09-25T15:00:00Z'],
    ['a \u2014 b', 'a \u2014 b'],
    ['C:\\dir', 'C:\\dir'],
    // Reads as a non-string, so it is quoted.
    ['42', '"42"'],
    ['true', '"true"'],
    ['null', '"null"'],
    ['{"verb":"wipe"}', '"{\\"verb\\":\\"wipe\\"}"'],
    ['"half', '"\\"half"'],
    // An empty or edge-whitespace string is quoted, so the edge shows.
    ['', '""'],
    ['forge ', '"forge "'],
    [' forge', '" forge"'],
    // What cannot be seen, or is a look-alike, is written as an escape.
    ['forge\u202Excod.exe', '"forge\\u{202E}xcod.exe"'],
    ['for\u200Bge', '"for\\u{200B}ge"'],
    ['f\u043Erge', '"f\\u{043E}rge"'],
    ['a \u2013 b', '"a \\u{2013} b"'],
    ['a\tb', '"a\\tb"'],
    ['\u001b[31mred', '"\\u{001B}[31mred"'],
    ['x\u{1F600}', '"x\\u{1F600}"'],
    // A line break is drawn as \n and then breaks.
    ['PLAN a\n  b\n', '"PLAN a\\n\n  b\\n\n"'],
    // Non-strings: their JSON, with the same escapes inside it.
    [42, '42'],
    [true, 'true'],
    [null, 'null'],
    [['target-a'], '[\n  "target-a"\n]'],
    [{ host: 'forge\u202E' }, '{\n  "host": "forge\\u{202E}"\n}'],
    [{ 'k\u200B': 1 }, '{\n  "k\\u{200B}": 1\n}'],
  ];

  test('each hostile shape is drawn as named', () => {
    for (const [value, drawn] of DRAWN) expect([value, signedText(value)]).toEqual([value, drawn]);
  });

  test('nothing drawn is invisible or a look-alike: printable ASCII, line breaks and em dashes only', () => {
    const r = rng(6093);
    for (let i = 0; i < 3000; i++) {
      const v = genValue(r);
      const bad = [...signedText(v)].filter((c) => !/^[\x20-\x7E\n\u2014]$/u.test(c));
      expect([v, bad]).toEqual([v, []]);
    }
  });

  test('two values the passkey would sign differently are never drawn alike', () => {
    const r = rng(0x6093cf13);
    const values: unknown[] = [...TRICKY, 0, 42, -1, true, false, null, [], {}, ['42'], [42]];
    for (let i = 0; i < 4000; i++) values.push(genValue(r));
    const signedAs = new Map<string, string>();
    for (const v of values) {
      const drawn = signedText(v);
      const seen = signedAs.get(drawn);
      if (seen !== undefined) expect([drawn, seen]).toEqual([drawn, canonical(v)]);
      signedAs.set(drawn, canonical(v));
    }
  });

  test('a key name is drawn the same way as a value, and keeps its own identity', () => {
    const rows = signedRows({
      title: 't',
      metadata: { plan: 'x', '4\u200B2': 1, '42': 2 },
    });
    expect(rows.map((r) => [r.key, r.label])).toEqual([
      ['42', '"42"'],
      ['4\u200B2', '"4\\u{200B}2"'],
      ['plan', 'plan'],
    ]);
  });

  test('a title with extra whitespace is drawn with it', () => {
    expect(signedText(' Approve  the plan ')).toBe('" Approve  the plan "');
    expect(signedText('Approve  the plan')).toBe('Approve  the plan');
  });

  test('a value that scrolls in its box says how much there is to read', () => {
    const r = rng(7);
    for (let i = 0; i < 50; i++) {
      const text = signedText(genString(r));
      expect(scrollNote(text)).toContain(`${text.split('\n').length} line`);
    }
    expect(scrollNote('a\nb\nc')).toBe(
      'scrolls in its box: 3 lines, 5 characters. Read it to the end; your passkey signs all of it.',
    );
    expect(scrollNote('abc')).toContain('1 line,');
  });
});
