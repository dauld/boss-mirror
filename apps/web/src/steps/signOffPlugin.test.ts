// sign-off.js (v2, then v3) against the real bundle, stubbed host — the
// correctionVerdictPlugin posture. The shapes pinned here are the two
// David hit blind on 2026-08-19 (19db52de): a decision sign-off whose
// case and contract never rendered, and a required-at-done field whose
// completion 400 was swallowed. v1's row was retired live for exactly
// these gaps; this suite is what earns re-publishing it.

import { afterEach, describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import {
  canonical,
  notShown,
  scrollNote,
  signedRows,
  signedText,
  type ShownStep,
} from './presence';
import { genMetadata, genString, genValue, rng } from './signedInputs.testkit';

const BUNDLE = new URL('../../../../infra/step-plugins/sign-off.js', import.meta.url);

type Handler = (ev: { target: FakeNode }) => void;

class FakeNode {
  // Layout, which a fake DOM does not have: a test that needs a box to
  // scroll says which boxes do (FakeNode.scrolls), and every other box
  // holds exactly its content.
  static scrolls: (n: FakeNode) => boolean = () => false;
  get clientHeight() {
    return 100;
  }
  get scrollHeight() {
    return FakeNode.scrolls(this) ? 400 : 100;
  }
  clientWidth = 100;
  scrollWidth = 100;
  className = '';
  textContent = '';
  value = '';
  disabled = false;
  children: FakeNode[] = [];
  listeners = new Map<string, Handler[]>();
  appendChild(c: FakeNode) {
    this.children.push(c);
    return c;
  }
  replaceChildren(...cs: FakeNode[]) {
    this.children = cs;
  }
  remove() {}
  setAttribute() {}
  addEventListener(name: string, fn: Handler) {
    const l = this.listeners.get(name) ?? [];
    l.push(fn);
    this.listeners.set(name, l);
  }
  fire(name: string) {
    for (const fn of this.listeners.get(name) ?? []) fn({ target: this });
  }
}

function walk(n: FakeNode, out: FakeNode[] = []): FakeNode[] {
  out.push(n);
  for (const c of n.children) walk(c, out);
  return out;
}
const byClass = (root: FakeNode, cls: string) =>
  walk(root).filter((n) => n.className.split(' ').includes(cls));
const allText = (root: FakeNode) =>
  walk(root)
    .map((n) => n.textContent)
    .join(' ');
const buttonNamed = (root: FakeNode, label: string) =>
  walk(root).find(
    (n) =>
      n.listeners.has('click') &&
      walk(n)
        .map((x) => x.textContent)
        .join('')
        .trim() === label,
  );

/** The "what your passkey signs" block as drawn: key -> the text on screen. */
const signedBlock = (root: FakeNode): Map<string, string> =>
  new Map(
    byClass(root, 'step-signed-row').map((row) => {
      const text = (cls: string) =>
        byClass(row, cls)
          .flatMap((n) => walk(n))
          .map((n) => n.textContent)
          .join('');
      return [text('step-signed-key'), text('step-signed-value')] as const;
    }),
  );

type FetchCall = { url: string; method: string; body: unknown };

function loadBundle(routes: (url: string, init?: RequestInit) => unknown) {
  let mountFn: ((c: unknown, p: unknown) => unknown) | null = null;
  const calls: FetchCall[] = [];
  const g = globalThis as unknown as Record<string, unknown>;
  g.Node = FakeNode;
  g.window = {
    __boss_register_step_plugin: (_kind: string, mount: (c: unknown, p: unknown) => unknown) => {
      mountFn = mount;
    },
  };
  g.document = {
    createElement: () => new FakeNode(),
    createTextNode: (s: string) => {
      const n = new FakeNode();
      n.textContent = s;
      return n;
    },
  };
  g.fetch = (url: string, init?: RequestInit) => {
    calls.push({
      url,
      method: init?.method ?? 'GET',
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });
    const result = routes(url, init);
    if (result === undefined) return Promise.reject(new Error(`unrouted: ${url}`));
    const status = (result as { __status?: number }).__status ?? 200;
    const res = {
      ok: status >= 200 && status < 300,
      status,
      json: async () => result,
      text: async () =>
        typeof (result as { __text?: string }).__text === 'string'
          ? (result as { __text: string }).__text
          : JSON.stringify(result),
      // sign() reads a 422 through clone() so the body stays readable.
      clone: () => res,
    };
    return Promise.resolve(res);
  };
  // eslint-disable-next-line no-new-func
  new Function(readFileSync(BUNDLE, 'utf8'))();
  if (!mountFn) throw new Error('bundle registered no plugin');
  const mount = mountFn as ((c: unknown, p: unknown) => unknown) & { signed?: PluginSigned };
  return { mount, calls, signed: mount.signed };
}

/** The plugin's own copies of the functions presence.ts defines, as the
 *  bundle exposes them on its mount function for this pin. */
type PluginSigned = {
  signedText: (v: unknown) => string;
  canonical: (v: unknown) => string;
  notShown: (shown: ShownStep, screen: ShownStep | null) => string[];
  scrollNote: (text: string) => string;
};

async function settled() {
  for (let i = 0; i < 48; i++) await Promise.resolve();
}

// The publish approval's exact shape: sign-off kind, one required
// string field, no counter-signatures, a brief on the step.
const publishStep = () => ({
  id: 'step-1',
  kind: 'sign-off',
  title: 'Approve publishing to the public mirror',
  status: 'ready',
  sign_offs_required: [] as string[],
  sign_offs: [],
  fields: [{ name: 'approved', field_type: 'string', required: true }],
  metadata: {
    authority_role: 'platform-admin',
    context_md: 'APPROVE THIS ONE: 62 commits, secrets clean, 22 newly-public files read.',
  },
});

describe('sign-off v2', () => {
  test('renders the case for the decision from the step, no fetch needed', async () => {
    const { mount, calls } = loadBundle(() => undefined);
    const c = new FakeNode();
    mount(c, { step: publishStep(), jobId: 'job-1', onUpdate() {} });
    await settled();
    expect(allText(c)).toContain('62 commits');
    expect(allText(c)).toContain('written for this step');
    expect(calls.length).toBe(0);
  });

  test('falls back to the packet as filed when the step carries nothing', async () => {
    const { mount } = loadBundle((url) =>
      url === '/api/jobs/job-1' ? { metadata: { message: 'the filed case' }, steps: [] } : undefined,
    );
    const step = publishStep();
    delete (step.metadata as Record<string, unknown>).context_md;
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    await settled();
    expect(allText(c)).toContain('the filed case');
    expect(allText(c)).toContain('the packet as filed');
    // The packet's own text is outside the step's shape hash: a passkey
    // on this step does not sign it, and the card says so (6093cf13).
    expect(byClass(c, 'step-signoff-context-unsigned').map((n) => allText(n).trim())).toEqual([
      'not signed',
    ]);
  });

  test('the packet briefing is labelled not signed; the step’s own context is not', async () => {
    const briefed = loadBundle((url) =>
      url === '/api/jobs/job-1' ? { metadata: { context_md: 'the briefing' }, steps: [] } : undefined,
    );
    const step = publishStep();
    delete (step.metadata as Record<string, unknown>).context_md;
    const c = new FakeNode();
    briefed.mount(c, { step, jobId: 'job-1', onUpdate() {} });
    await settled();
    expect(allText(c)).toContain('the briefing');
    expect(byClass(c, 'step-signoff-context-unsigned').length).toBe(1);

    const own = loadBundle(() => undefined);
    const c2 = new FakeNode();
    own.mount(c2, { step: publishStep(), jobId: 'job-1', onUpdate() {} });
    await settled();
    expect(allText(c2)).toContain('62 commits');
    expect(byClass(c2, 'step-signoff-context-unsigned').length).toBe(0);
  });

  test('an empty required field blocks Approve/Reject and says which', async () => {
    const { mount } = loadBundle(() => undefined);
    const step = publishStep();
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    expect(buttonNamed(c, 'Approve')?.disabled).toBe(true);
    expect(buttonNamed(c, 'Reject')?.disabled).toBe(true);
    expect(buttonNamed(c, 'Request changes')?.disabled).toBe(false);
    expect(allText(c)).toContain('approved');

    const input = byClass(c, 'step-signoff-input')[0]!;
    input.value = 'true';
    input.fire('input');
    expect(buttonNamed(c, 'Approve')?.disabled).toBe(false);
  });

  test('a pre-filled required field arrives ready to approve', () => {
    const { mount } = loadBundle(() => undefined);
    const step = publishStep();
    (step.metadata as Record<string, unknown>).approved = 'true';
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    expect(byClass(c, 'step-signoff-input')[0]!.value).toBe('true');
    expect(buttonNamed(c, 'Approve')?.disabled).toBe(false);
  });

  test('Approve records the decision WITH the fields, then completes', async () => {
    const { mount, calls } = loadBundle(() => ({}));
    const step = publishStep();
    (step.metadata as Record<string, unknown>).approved = 'true';
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    // The decision travels through the step metadata PATCH: a bare
    // object of ONLY the keys this surface owns, merged server-side —
    // no snapshot spread, so a concurrent writer's keys survive (the
    // lost update that reverted a review's title/markdown on
    // 2026-09-02).
    const patches = calls.filter((x) => x.method === 'PATCH');
    expect(patches.length).toBe(1);
    expect(patches[0]!.url).toBe('/api/jobs/job-1/steps/step-1/metadata');
    const merged = patches[0]!.body as Record<string, unknown>;
    expect(merged.decision).toBe('approved');
    expect(merged.approved).toBe('true');
    expect(typeof merged.decided_at).toBe('string');

    // Then completion — recorded first, completed second.
    expect(calls.findIndex((x) => x.method === 'PATCH')).toBeLessThan(
      calls.findIndex((x) => x.method === 'PUT'),
    );
    const puts = calls.filter((x) => x.method === 'PUT');
    expect(puts.length).toBe(1);
    expect((puts[0]!.body as { status: string }).status).toBe('completed');
    // Status-only: a completion that carried metadata would replace
    // the row's metadata wholesale with whatever the client held.
    expect((puts[0]!.body as Record<string, unknown>).metadata).toBeUndefined();
  });

  test('an outstanding counter-signature blocks completion, not the decision', async () => {
    const { mount, calls } = loadBundle(() => ({}));
    const step = publishStep();
    (step.metadata as Record<string, unknown>).approved = 'true';
    step.sign_offs_required = ['controller'];
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    expect(allText(c)).toContain('controller');
    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    // The decision lands through the metadata PATCH; completion (a
    // status PUT) is what the outstanding counter-signature blocks.
    const patches = calls.filter((x) => x.method === 'PATCH');
    expect(patches.length).toBe(1);
    expect((patches[0]!.body as Record<string, unknown>).decision).toBe('approved');
    expect(calls.filter((x) => x.method === 'PUT').length).toBe(0);
  });

  test('a completion refusal is shown, never swallowed', async () => {
    const { mount } = loadBundle((url, init) => {
      if (init?.method === 'PUT' && init.body && String(init.body).includes('completed')) {
        return { __status: 400, __text: "required field 'approved' is missing" };
      }
      return {};
    });
    const step = publishStep();
    (step.metadata as Record<string, unknown>).approved = 'true';
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    buttonNamed(c, 'Approve')!.fire('click');
    await settled();
    expect(allText(c)).toContain("required field 'approved' is missing");
  });

  test('Request changes records without completing', async () => {
    const { mount, calls } = loadBundle(() => ({}));
    const step = publishStep();
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    buttonNamed(c, 'Request changes')!.fire('click');
    await settled();
    // Recorded through the metadata PATCH, and nothing completes: no
    // status write at all.
    const patches = calls.filter((x) => x.method === 'PATCH');
    expect(patches.length).toBe(1);
    expect((patches[0]!.body as Record<string, unknown>).decision).toBe('changes-requested');
    expect(calls.filter((x) => x.method === 'PUT').length).toBe(0);
  });

  // Backlog da322e8f, measured 2026-09-23 23:20Z: page-audit routes a
  // changes-requested review to `revise`, whose ready_when requires
  // `steps.review.done`, and this surface never completed the review —
  // three founder change requests (/ux/parts, /ux/products,
  // /ux/vendors) sat recorded and went nowhere. The PROTOCOL now says
  // which reading it means: a step whose metadata carries
  // `changes_requested_completes: true` (a Workflow metadata_defaults
  // key) completes on Request changes exactly as on Approve; every
  // other sign-off keeps the record-and-stay-open behaviour above.
  test('Request changes completes when the protocol declares it a route', async () => {
    const { mount, calls } = loadBundle(() => ({}));
    const step = publishStep();
    (step.metadata as Record<string, unknown>).approved = 'true';
    (step.metadata as Record<string, unknown>).changes_requested_completes = true;
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    buttonNamed(c, 'Request changes')!.fire('click');
    await settled();
    const patches = calls.filter((x) => x.method === 'PATCH');
    expect(patches.length).toBe(1);
    expect((patches[0]!.body as Record<string, unknown>).decision).toBe('changes-requested');
    // Recorded first, completed second — the same order as Approve.
    const puts = calls.filter((x) => x.method === 'PUT');
    expect(puts.length).toBe(1);
    expect(puts[0]!.url).toBe('/api/jobs/job-1/steps/step-1');
    expect((puts[0]!.body as { status: string }).status).toBe('completed');
    expect(calls.findIndex((x) => x.method === 'PATCH')).toBeLessThan(
      calls.findIndex((x) => x.method === 'PUT'),
    );
    expect(allText(c)).toContain('Completed');
  });

  test('a completing Request changes waits on required fields like Approve does', () => {
    const { mount } = loadBundle(() => ({}));
    const step = publishStep();
    (step.metadata as Record<string, unknown>).changes_requested_completes = true;
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    // `approved` is required-at-done and empty: a click that completes
    // could only 400, so the button waits with the others.
    expect(buttonNamed(c, 'Request changes')?.disabled).toBe(true);
    expect(buttonNamed(c, 'Approve')?.disabled).toBe(true);
  });
});

// ---------------------------------------------------------------------
// v3 — the signature follows the decision (feedback 221b4b5c).
//
// Measured on 2026-09-05 15:40 in the gateway log, on the emergency
// merge's "Approve the train bypass" step: POST …/sign-offs 200 →
// PATCH …/metadata 204 (the surface re-saved decision=approved with a
// NEW decided_at) → PUT complete 409 {missing_or_stale_roles:
// [platform-admin]}. The server binds a stamp to step_shape_hash(title,
// metadata) — values included — so the surface's own save made its own
// signature stale two seconds after taking it. David: "I tried to
// approve the emergency merge, but it doesn't appear to have taken."
//
// The stub below enforces exactly that server rule, so the suite can
// replay the measured sequence and refuse the inversion.

const META = '/api/jobs/job-1/steps/step-1/metadata';
const SIGN = '/api/jobs/job-1/steps/step-1/sign-offs';
const STEP = '/api/jobs/job-1/steps/step-1';

type Stamp = { role: string; authority_id: string; stamped_at: string; shape_hash: string };

// The server's stamp contract, in miniature: a stamp pins the shape
// (title + metadata, canonical) at the instant of signing; completion
// refuses while any required role's stamp is missing or pinned to an
// older shape. Metadata is the server's OWN copy — the surface's local
// cache must never be what the stub hashes.
function signingServer(step: ReturnType<typeof publishStep>) {
  const server = {
    title: step.title,
    metadata: JSON.parse(JSON.stringify(step.metadata)) as Record<string, unknown>,
  };
  const canonical = (m: Record<string, unknown>) =>
    JSON.stringify(Object.fromEntries(Object.entries(m).sort(([a], [b]) => (a < b ? -1 : 1))));
  const shape = () => `${server.title}|${canonical(server.metadata)}`;
  const stamps: Stamp[] = [];
  const puts: number[] = [];
  const stampAs = (role: string) => {
    stamps.push({
      role,
      authority_id: 'emp-david',
      stamped_at: '2026-09-05T15:40:20Z',
      shape_hash: shape(),
    });
  };
  const routes = (url: string, init?: RequestInit) => {
    const m = init?.method ?? 'GET';
    if (url === META && m === 'PATCH') {
      Object.assign(server.metadata, JSON.parse(String(init?.body)));
      return {};
    }
    if (url === SIGN && m === 'POST') {
      stampAs((JSON.parse(String(init?.body)) as { role: string }).role);
      return { ...step, metadata: server.metadata, sign_offs: stamps.slice() };
    }
    if (url === '/api/jobs/job-1' && m === 'GET') {
      return {
        metadata: {},
        steps: [{ ...step, metadata: server.metadata, sign_offs: stamps.slice() }],
      };
    }
    if (url === STEP && m === 'PUT') {
      const current = shape();
      const missing = step.sign_offs_required.filter(
        (r) => !stamps.some((s) => s.role === r && s.shape_hash === current),
      );
      if (missing.length > 0) {
        puts.push(409);
        return {
          __status: 409,
          __text: JSON.stringify({ error: 'sign-offs incomplete', missing_or_stale_roles: missing }),
        };
      }
      puts.push(200);
      return {};
    }
    return undefined;
  };
  return { routes, stamps, puts, shape, stampAs };
}

// The emergency merge's approval as David found it at 15:40: one
// required role (his own), the decision already recorded by the 14:21
// attempts, no signature yet.
const bypassStep = () => {
  const step = publishStep();
  step.title = 'Approve the train bypass';
  step.sign_offs_required = ['platform-admin'];
  (step.metadata as Record<string, unknown>).approved = 'true';
  (step.metadata as Record<string, unknown>).decision = 'approved';
  (step.metadata as Record<string, unknown>).decided_at = '2026-09-05T14:21:53Z';
  return step;
};

// The same step DECLARING presence, as a passkey-gated sign-off does.
// Since design f623e425 D3 a surface draws the signed content for a
// presence step up front, and refuses a ceremony on a step whose content
// it has not drawn — so the suites below, whose server demands presence,
// declare it the way ops-request's approve step does.
const presenceStep = () => Object.assign(bypassStep(), { assurance_required: 'presence' });

const methods =(calls: FetchCall[]) => calls.map((c) => c.method);

describe('sign-off v3 — the signature follows the decision', () => {
  test('the stub refuses the measured sequence and accepts the corrected one', () => {
    const bad = signingServer(bypassStep());
    bad.routes(META, { method: 'PATCH', body: JSON.stringify({ decision: 'approved', decided_at: 't1' }) });
    bad.routes(SIGN, { method: 'POST', body: JSON.stringify({ role: 'platform-admin' }) });
    bad.routes(META, { method: 'PATCH', body: JSON.stringify({ decision: 'approved', decided_at: 't2' }) });
    const refused = bad.routes(STEP, { method: 'PUT', body: JSON.stringify({ status: 'completed' }) }) as {
      __status: number;
      __text: string;
    };
    expect(refused.__status).toBe(409);
    expect(refused.__text).toContain('"missing_or_stale_roles":["platform-admin"]');

    const good = signingServer(bypassStep());
    good.routes(META, { method: 'PATCH', body: JSON.stringify({ decision: 'approved', decided_at: 't1' }) });
    good.routes(SIGN, { method: 'POST', body: JSON.stringify({ role: 'platform-admin' }) });
    good.routes(STEP, { method: 'PUT', body: JSON.stringify({ status: 'completed' }) });
    expect(good.puts).toEqual([200]);
  });

  test('2026-09-05 replayed: sign, then Approve — no metadata write after the signature, and it completes', async () => {
    const step = bypassStep();
    const srv = signingServer(step);
    const { mount, calls } = loadBundle(srv.routes);
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Sign off as platform-admin')!.fire('click');
    await settled();
    expect(srv.stamps.length).toBe(1);
    const shapeWhenSigned = srv.stamps[0]!.shape_hash;

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    const seq = methods(calls);
    const signedAt = seq.indexOf('POST');
    expect(signedAt).toBeGreaterThanOrEqual(0);
    // The decision it already carried is what was signed: nothing
    // re-saves it, so the signature's shape is the completion's shape.
    expect(seq.slice(signedAt)).not.toContain('PATCH');
    expect(srv.shape()).toBe(shapeWhenSigned);
    expect(srv.puts).toEqual([200]);
    expect(allText(c)).toContain('Completed');
  });

  test('one tap for a single signer: decision saved, then signed, then completed — each stage visible', async () => {
    const step = bypassStep();
    delete (step.metadata as Record<string, unknown>).decision;
    delete (step.metadata as Record<string, unknown>).decided_at;
    const srv = signingServer(step);
    const { mount, calls } = loadBundle(srv.routes);
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    const seq = methods(calls);
    const patchAt = seq.indexOf('PATCH');
    const postAt = seq.indexOf('POST');
    const putAt = seq.indexOf('PUT');
    expect(patchAt).toBeGreaterThanOrEqual(0);
    expect(patchAt).toBeLessThan(postAt);
    expect(postAt).toBeLessThan(putAt);
    expect(seq.filter((m) => m === 'PATCH').length).toBe(1);
    // The stamp pinned the shape WITH the decision in it, and that
    // shape held through completion.
    expect(srv.stamps[0]!.shape_hash).toBe(srv.shape());
    expect(srv.shape()).toContain('"decision":"approved"');
    expect(srv.puts).toEqual([200]);

    const text = allText(c);
    expect(text).toContain('Decision saved');
    expect(text).toContain('Signed as platform-admin');
    expect(text).toContain('Completed');
  });

  test('changing a signed decision re-saves BEFORE re-signing, then completes', async () => {
    const step = bypassStep();
    const srv = signingServer(step);
    srv.stampAs('platform-admin');
    (step as { sign_offs: Stamp[] }).sign_offs = srv.stamps.slice();
    const { mount, calls } = loadBundle(srv.routes);
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Reject')!.fire('click');
    await settled();

    const seq = methods(calls);
    expect(seq.indexOf('PATCH')).toBeLessThan(seq.indexOf('POST'));
    expect(seq.indexOf('POST')).toBeLessThan(seq.indexOf('PUT'));
    expect(seq.slice(seq.indexOf('POST'))).not.toContain('PATCH');
    expect(srv.shape()).toContain('"decision":"rejected"');
    expect(srv.stamps.length).toBe(2);
    expect(srv.puts).toEqual([200]);
  });

  test('a completion 409 is shown verbatim and the named role is offered its signature again', async () => {
    const step = bypassStep();
    const srv = signingServer(step);
    srv.stampAs('platform-admin');
    (step as { sign_offs: Stamp[] }).sign_offs = srv.stamps.slice();
    const body = '{"error":"sign-offs incomplete","missing_or_stale_roles":["platform-admin"]}';
    const { mount } = loadBundle((url, init) =>
      url === STEP && init?.method === 'PUT' ? { __status: 409, __text: body } : srv.routes(url, init),
    );
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    expect(buttonNamed(c, 'Sign off as platform-admin')).toBeUndefined();

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    expect(allText(c)).toContain(`409: ${body}`);
    expect(buttonNamed(c, 'Sign off as platform-admin')).toBeDefined();
  });
});

// ---------------------------------------------------------------------
// A presence-gated step COMPLETES with the ticket its signature was
// granted on (backlog b568044a, round-4 review of car 7abc0154,
// 2026-09-25).
//
// The jobs API judges assurance on every request that leaves the open
// states, from that request's own `x-boss-presence` (steps.rs
// is_leaving_open -> judge_assurance). This surface ran the ceremony for
// the stamp and then sent the completion PUT bare, so the stamp landed
// and the completion answered 422 — every passkey approval stopped one
// write short, and the approve-then-execute path worked only against a
// stub. The stub below refuses exactly that, as the server does.

describe('sign-off — a presence step completes with its own ticket', () => {
  const BEGIN = '/api/auth/passkey/assert/begin';
  const FINISH = '/api/auth/passkey/assert/finish';
  const TICKET = 'ticket-from-this-ceremony';
  const beginOptions = {
    challenge_id: 'chal-1',
    shape_hash: 'h',
    publicKey: {
      challenge: 'AAAA',
      rpId: 'boss.test',
      allowCredentials: [{ type: 'public-key', id: 'AAAA' }],
      userVerification: 'required',
      timeout: 60000,
    },
  };
  const buf = () => new Uint8Array([1, 2, 3]).buffer;
  const credential = {
    id: 'cred',
    rawId: buf(),
    type: 'public-key',
    response: { authenticatorData: buf(), clientDataJSON: buf(), signature: buf(), userHandle: null },
  };
  const ticketOn = (init?: RequestInit) =>
    (init?.headers as Record<string, string> | undefined)?.['x-presence-ticket'];

  /** The signing server, presence-gated on BOTH doors: a stamp or a
   *  completion that carries no ticket is refused 422, as the jobs API
   *  refuses it. Counts the ceremonies, and records what each completion
   *  PUT carried. */
  const presenceServer = (step: ReturnType<typeof bypassStep>) => {
    const srv = signingServer(step);
    let ceremonies = 0;
    const completionTickets: (string | undefined)[] = [];
    const refusal = { __status: 422, required: 'presence', produced: 'session' };
    const routes = (url: string, init?: RequestInit) => {
      const m = init?.method ?? 'GET';
      if (url === SIGN && m === 'POST' && ticketOn(init) !== TICKET) return refusal;
      if (url === BEGIN) return beginOptions;
      if (url === FINISH) {
        ceremonies += 1;
        return { ticket: TICKET };
      }
      if (url === STEP && m === 'PUT') {
        completionTickets.push(ticketOn(init));
        if (ticketOn(init) !== TICKET) return refusal;
      }
      return srv.routes(url, init);
    };
    return { srv, routes, ceremonies: () => ceremonies, completionTickets };
  };
  const withPasskey = () => {
    (globalThis as unknown as Record<string, unknown>).navigator = {
      credentials: { get: async () => credential },
    };
  };

  test('Approve: one ceremony, the stamp and the completion both carry its ticket, and it completes', async () => {
    const step = presenceStep();
    delete (step.metadata as Record<string, unknown>).decision;
    delete (step.metadata as Record<string, unknown>).decided_at;
    const { srv, routes, ceremonies, completionTickets } = presenceServer(step);
    const { mount } = loadBundle(routes);
    withPasskey();
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    expect(ceremonies()).toBe(1);
    expect(srv.stamps.length).toBe(1);
    expect(completionTickets).toEqual([TICKET]);
    expect(srv.puts).toEqual([200]);
    expect(allText(c)).toContain('Completed');
  });

  test('Reject: the same — a rejection is a completion and needs the same ticket', async () => {
    const step = presenceStep();
    const { srv, routes, ceremonies, completionTickets } = presenceServer(step);
    const { mount } = loadBundle(routes);
    withPasskey();
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Reject')!.fire('click');
    await settled();

    expect(srv.shape()).toContain('"decision":"rejected"');
    expect(ceremonies()).toBe(1);
    expect(completionTickets).toEqual([TICKET]);
    expect(srv.puts).toEqual([200]);
    expect(allText(c)).toContain('Completed');
  });

  test('signed by the role button, then Approve: the completion carries the ticket that signature minted', async () => {
    const step = presenceStep();
    const { srv, routes, ceremonies, completionTickets } = presenceServer(step);
    const { mount } = loadBundle(routes);
    withPasskey();
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Sign off as platform-admin')!.fire('click');
    await settled();
    expect(srv.stamps.length).toBe(1);

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    // No second passkey prompt: the decision was already the signed one,
    // so the shape the ticket binds still stands.
    expect(ceremonies()).toBe(1);
    expect(completionTickets).toEqual([TICKET]);
    expect(srv.puts).toEqual([200]);
  });

  test('no ceremony ran, so no ticket rides the completion', async () => {
    const step = presenceStep();
    delete (step.metadata as Record<string, unknown>).decision;
    const srv = signingServer(step);
    const tickets: (string | undefined)[] = [];
    const { mount } = loadBundle((url, init) => {
      if (url === STEP && init?.method === 'PUT') tickets.push(ticketOn(init));
      return srv.routes(url, init);
    });
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    expect(srv.puts).toEqual([200]);
    expect(tickets).toEqual([undefined]);
  });
});

// ---------------------------------------------------------------------
// A completion refused for presence is answered with ONE tap and retried
// ONCE (backlog 3ce3c15f, from the review of car 5b30ccf9, 2026-09-25).
//
// Two ways the completion reached the jobs API with no ticket it would
// accept, both measured on the car's tree: after a RELOAD the stamp is
// already on the step, so sign() is skipped and nothing is held; and a
// ticket held from an earlier signature outlives its two-minute life. The
// server answered 422 {required:"presence"} and this surface printed the
// raw refusal — the only way through was to change the comment so the
// shape moved and a signature was forced. And the held ticket was never
// cleared, so a spent one rode every later completion from this mount.
//
// The server below issues a DISTINCT ticket per ceremony and accepts only
// the ones it currently honours, so a surface that retried with the held
// ticket, looped, or minted anything itself cannot pass.

describe('sign-off — a completion refused for presence gets one tap and one retry', () => {
  const BEGIN = '/api/auth/passkey/assert/begin';
  const FINISH = '/api/auth/passkey/assert/finish';
  const beginOptions = {
    challenge_id: 'chal-1',
    publicKey: {
      challenge: 'AAAA',
      rpId: 'boss.test',
      allowCredentials: [{ type: 'public-key', id: 'AAAA' }],
      userVerification: 'required',
      timeout: 60000,
    },
  };
  const buf = () => new Uint8Array([1, 2, 3]).buffer;
  const credential = {
    id: 'cred',
    rawId: buf(),
    type: 'public-key',
    response: { authenticatorData: buf(), clientDataJSON: buf(), signature: buf(), userHandle: null },
  };
  const ticketOn = (init?: RequestInit) =>
    (init?.headers as Record<string, string> | undefined)?.['x-presence-ticket'];
  const refusal = { __status: 422, required: 'presence', produced: 'session' };

  /** Presence-gated on both doors; ceremony n issues `ticket-n`; a ticket
   *  is honoured only while it is in `live` (expire one by deleting it). */
  const presenceServer = (step: ReturnType<typeof bypassStep>) => {
    const srv = signingServer(step);
    const live = new Set<string>();
    let ceremonies = 0;
    const completionTickets: (string | undefined)[] = [];
    const begins: unknown[] = [];
    let completionAnswer: ((init?: RequestInit) => unknown) | null = null;
    const routes = (url: string, init?: RequestInit) => {
      const m = init?.method ?? 'GET';
      const t = ticketOn(init);
      if (url === SIGN && m === 'POST' && !(t && live.has(t))) return refusal;
      if (url === BEGIN) {
        begins.push(JSON.parse(String(init?.body)));
        return beginOptions;
      }
      if (url === FINISH) {
        ceremonies += 1;
        const ticket = `ticket-${ceremonies}`;
        live.add(ticket);
        return { ticket };
      }
      if (url === STEP && m === 'PUT') {
        completionTickets.push(t);
        if (completionAnswer) {
          const answer = completionAnswer(init);
          if (answer !== undefined) return answer;
        }
        if (!(t && live.has(t))) return refusal;
      }
      return srv.routes(url, init);
    };
    return {
      srv,
      routes,
      live,
      begins,
      ceremonies: () => ceremonies,
      completionTickets,
      answerCompletion: (fn: (init?: RequestInit) => unknown) => {
        completionAnswer = fn;
      },
    };
  };
  const withPasskey = () => {
    (globalThis as unknown as Record<string, unknown>).navigator = {
      credentials: { get: async () => credential },
    };
  };

  test('after a reload with the stamp already recorded: one tap on the current content, retried, completed', async () => {
    const step = presenceStep();
    const server = presenceServer(step);
    // Signed before the reload: the stamp pins the step as it stands, and
    // this mount holds no ticket of its own.
    server.srv.stampAs('platform-admin');
    (step as { sign_offs: Stamp[] }).sign_offs = server.srv.stamps.slice();
    const { mount } = loadBundle(server.routes);
    withPasskey();
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    expect(server.ceremonies()).toBe(1);
    expect(server.completionTickets).toEqual([undefined, 'ticket-1']);
    expect(server.srv.puts).toEqual([200]);
    // The tap signs the step as this surface shows it — the server's own
    // copy, since nothing was re-saved.
    expect(server.begins).toEqual([
      { job_id: 'job-1', step_id: 'step-1', shown: { title: step.title, metadata: step.metadata } },
    ]);
    expect(allText(c)).toContain('Completed');
    expect(allText(c)).not.toContain('422');
  });

  test('a held ticket past its life: the completion is refused, one fresh tap, retried with the fresh ticket', async () => {
    const step = presenceStep();
    const server = presenceServer(step);
    const { mount } = loadBundle(server.routes);
    withPasskey();
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Sign off as platform-admin')!.fire('click');
    await settled();
    expect(server.ceremonies()).toBe(1);
    // Two minutes pass: the ticket that signature minted is no longer honoured.
    server.live.delete('ticket-1');

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    expect(server.ceremonies()).toBe(2);
    expect(server.completionTickets).toEqual(['ticket-1', 'ticket-2']);
    expect(server.srv.puts).toEqual([200]);
    expect(allText(c)).toContain('Completed');
  });

  test('a presence step this user signs no role on: the completion alone asks for the tap', async () => {
    const step = presenceStep();
    step.sign_offs_required = [];
    const server = presenceServer(step);
    const { mount } = loadBundle(server.routes);
    withPasskey();
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    expect(server.ceremonies()).toBe(1);
    expect(server.completionTickets).toEqual([undefined, 'ticket-1']);
    expect(server.srv.puts).toEqual([200]);
  });

  test('refused again after the fresh tap: no second ceremony, no third write, and the refusal says so', async () => {
    const step = presenceStep();
    const server = presenceServer(step);
    server.srv.stampAs('platform-admin');
    (step as { sign_offs: Stamp[] }).sign_offs = server.srv.stamps.slice();
    // A server that will not honour any ticket on the completion.
    server.answerCompletion(() => refusal);
    const { mount } = loadBundle(server.routes);
    withPasskey();
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    expect(server.ceremonies()).toBe(1);
    expect(server.completionTickets).toEqual([undefined, 'ticket-1']);
    expect(allText(c)).toContain('refused again after a fresh passkey tap');
    expect(allText(c)).not.toContain('Completed');
  });

  test('a recovery ceremony that fails is named, and the completion is not re-sent', async () => {
    const step = presenceStep();
    const server = presenceServer(step);
    server.srv.stampAs('platform-admin');
    (step as { sign_offs: Stamp[] }).sign_offs = server.srv.stamps.slice();
    const { mount } = loadBundle((url, init) =>
      url === BEGIN ? { __status: 409, __text: 'no passkey' } : server.routes(url, init),
    );
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    expect(server.completionTickets).toEqual([undefined]);
    expect(allText(c)).toContain('No passkey enrolled');
  });

  test('the held ticket is spent on the completion it rode: a later attempt does not carry it', async () => {
    const step = presenceStep();
    const server = presenceServer(step);
    let first = true;
    // The first completion is refused for a reason that is not presence.
    server.answerCompletion(() => {
      if (!first) return undefined;
      first = false;
      return { __status: 400, __text: "required field 'approved' is missing" };
    });
    const { mount } = loadBundle(server.routes);
    withPasskey();
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Sign off as platform-admin')!.fire('click');
    await settled();
    buttonNamed(c, 'Approve')!.fire('click');
    await settled();
    expect(allText(c)).toContain("required field 'approved' is missing");

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    // Second attempt went bare, was refused for presence, and took its own tap.
    expect(server.completionTickets).toEqual(['ticket-1', undefined, 'ticket-2']);
    expect(server.ceremonies()).toBe(2);
    expect(server.srv.puts).toEqual([200]);
  });

  // Backlog d82b5f60 (review of car 66de0e4b, 2026-09-25). The recovery
  // ceremony's begin is refused 412 when the step no longer matches what
  // this surface showed — another writer moved it. The surface returned
  // there without asking the host to refresh, and kept "Decision saved:
  // rejected" on screen as if that were what the step now holds.
  test('a recovery ceremony refused 412 asks the host to refresh and leaves no stale "Decision saved"', async () => {
    const step = presenceStep();
    step.sign_offs_required = [];
    const server = presenceServer(step);
    const { mount } = loadBundle((url, init) =>
      url === BEGIN
        ? { __status: 412, __text: 'the step changed since it was shown' }
        : server.routes(url, init),
    );
    withPasskey();
    let updates = 0;
    const c = new FakeNode();
    mount(c, {
      step,
      jobId: 'job-1',
      onUpdate() {
        updates += 1;
      },
    });

    buttonNamed(c, 'Reject')!.fire('click');
    await settled();

    expect(server.completionTickets).toEqual([undefined]);
    expect(updates).toBe(1);
    expect(allText(c)).toContain('412');
    expect(allText(c)).toContain('reopen it to read it as it stands');
    expect(allText(c)).not.toContain('Decision saved');
  });

  test('a stamp ceremony refused 412 after the decision landed does the same', async () => {
    const step = presenceStep();
    delete (step.metadata as Record<string, unknown>).decision;
    const server = presenceServer(step);
    const { mount } = loadBundle((url, init) =>
      url === BEGIN
        ? { __status: 412, __text: 'the step changed since it was shown' }
        : server.routes(url, init),
    );
    withPasskey();
    let updates = 0;
    const c = new FakeNode();
    mount(c, {
      step,
      jobId: 'job-1',
      onUpdate() {
        updates += 1;
      },
    });

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    expect(server.completionTickets).toEqual([]);
    expect(updates).toBe(1);
    expect(allText(c)).toContain('reopen it to read it as it stands');
    expect(allText(c)).not.toContain('Decision saved');
  });

  // The held ticket was cleared only after the completion RESOLVED, so a
  // completion whose fetch threw kept it, and the next attempt re-sent it.
  test('a completion whose request threw still spends the held ticket', async () => {
    const step = presenceStep();
    const server = presenceServer(step);
    let throwNext = false;
    const { mount } = loadBundle((url, init) => {
      if (throwNext && url === STEP && init?.method === 'PUT') {
        throwNext = false;
        server.completionTickets.push(ticketOn(init));
        return undefined; // the stub rejects an unrouted request: a network failure
      }
      return server.routes(url, init);
    });
    withPasskey();
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Sign off as platform-admin')!.fire('click');
    await settled();
    throwNext = true;
    buttonNamed(c, 'Approve')!.fire('click');
    await settled();
    expect(allText(c)).toContain('Could not record the decision');

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    expect(server.completionTickets).toEqual(['ticket-1', undefined, 'ticket-2']);
    expect(server.ceremonies()).toBe(2);
    expect(server.srv.puts).toEqual([200]);
  });

  // "Refused again after a fresh passkey tap" named ANY refusal of the
  // retry, so a 409 for stale stamps read as a presence failure.
  test('a retry refused 409 after the tap is labelled by its own reason', async () => {
    const step = presenceStep();
    const server = presenceServer(step);
    server.srv.stampAs('platform-admin');
    (step as { sign_offs: Stamp[] }).sign_offs = server.srv.stamps.slice();
    const body = '{"error":"sign-offs incomplete","missing_or_stale_roles":["platform-admin"]}';
    let n = 0;
    server.answerCompletion(() => {
      n += 1;
      return n === 1 ? refusal : { __status: 409, __text: body };
    });
    const { mount } = loadBundle(server.routes);
    withPasskey();
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });

    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    expect(server.ceremonies()).toBe(1);
    expect(server.completionTickets).toEqual([undefined, 'ticket-1']);
    expect(allText(c)).toContain(`409: ${body}`);
    expect(allText(c)).not.toContain('refused again');
    expect(buttonNamed(c, 'Sign off as platform-admin')).toBeDefined();
  });
});

// ---------------------------------------------------------------------
// The presence ceremony names what failed (backlog f3436d99).
//
// The plugin runs its own copy of the ceremony — a bundle cannot import
// the app's presence.ts — and until this car it dropped the gateway's
// refusal text at both ends: 'presence ceremony unavailable (502)' said
// nothing about WHICH of the gateway's steps refused (job fetch, stored
// passkeys, challenge mint), and 'assertion rejected (410)' hid 'challenge
// already spent or expired — begin again'. The app's copy carries the
// text beside the status since 2e893e27; the plugin says the same.

describe('sign-off — the presence ceremony names what failed', () => {
  const BEGIN = '/api/auth/passkey/assert/begin';
  const FINISH = '/api/auth/passkey/assert/finish';
  const presenceGated = (url: string, init?: RequestInit) =>
    url === SIGN && init?.method === 'POST' ? { __status: 422, required: 'presence' } : undefined;
  const beginOptions = {
    challenge_id: 'chal-1',
    shape_hash: 'h',
    publicKey: {
      challenge: 'AAAA',
      rpId: 'boss.test',
      allowCredentials: [{ type: 'public-key', id: 'AAAA' }],
      userVerification: 'required',
      timeout: 60000,
    },
  };
  const buf = () => new Uint8Array([1, 2, 3]).buffer;
  const credential = {
    id: 'cred',
    rawId: buf(),
    type: 'public-key',
    response: { authenticatorData: buf(), clientDataJSON: buf(), signature: buf(), userHandle: null },
  };

  test('a refused begin shows the gateway text beside the status', async () => {
    const refusal = 'job fetch: 403 Forbidden';
    const { mount } = loadBundle(
      (url, init) =>
        presenceGated(url, init) ?? (url === BEGIN ? { __status: 502, __text: refusal } : undefined),
    );
    const c = new FakeNode();
    mount(c, { step: presenceStep(), jobId: 'job-1', onUpdate() {} });
    buttonNamed(c, 'Sign off as platform-admin')!.fire('click');
    await settled();
    expect(allText(c)).toContain(`presence ceremony unavailable (502): ${refusal}`);
  });

  test('a refused finish shows the gateway text beside the status', async () => {
    const refusal = 'challenge already spent or expired — begin again';
    const { mount, calls } = loadBundle(
      (url, init) =>
        presenceGated(url, init) ??
        (url === BEGIN ? beginOptions : url === FINISH ? { __status: 410, __text: refusal } : undefined),
    );
    (globalThis as unknown as Record<string, unknown>).navigator = {
      credentials: { get: async () => credential },
    };
    const c = new FakeNode();
    mount(c, { step: presenceStep(), jobId: 'job-1', onUpdate() {} });
    buttonNamed(c, 'Sign off as platform-admin')!.fire('click');
    await settled();
    expect(calls.map((x) => x.url)).toContain(FINISH);
    expect(allText(c)).toContain(`assertion rejected (410): ${refusal}`);
  });
});

// ---------------------------------------------------------------------
// The begin names what this surface RENDERED (backlog fd7090cc, the
// security re-review of 2026-09-25).
//
// The begin used to send only {job_id, step_id}, and the gateway bound
// the challenge to the step as it read it at that instant — so a writer
// who swapped the plan between this surface's render and the key press
// had the swap signed. The begin now names the step as this surface
// rendered it, with the decision its own gesture saved folded in, and
// the gateway refuses a begin whose shown content is not the step as it
// stands (crates/core/boss-gateway/tests/a_passkey_signs_what_was_shown.rs).

describe('sign-off — the begin names the step as this surface rendered it', () => {
  const BEGIN = '/api/auth/passkey/assert/begin';
  const FINISH = '/api/auth/passkey/assert/finish';
  const beginOptions = {
    challenge_id: 'chal-1',
    shape_hash: 'h',
    publicKey: {
      challenge: 'AAAA',
      rpId: 'boss.test',
      allowCredentials: [{ type: 'public-key', id: 'AAAA' }],
      userVerification: 'required',
      timeout: 60000,
    },
  };
  const buf = () => new Uint8Array([1, 2, 3]).buffer;
  const credential = {
    id: 'cred',
    rawId: buf(),
    type: 'public-key',
    response: { authenticatorData: buf(), clientDataJSON: buf(), signature: buf(), userHandle: null },
  };
  /** The signing server, presence-gated: a stamp without a ticket is 422. */
  const presenceServer = (step: ReturnType<typeof bypassStep>) => {
    const srv = signingServer(step);
    const routes = (url: string, init?: RequestInit) => {
      const ticket = (init?.headers as Record<string, string> | undefined)?.['x-presence-ticket'];
      if (url === SIGN && init?.method === 'POST' && !ticket) {
        return { __status: 422, required: 'presence' };
      }
      if (url === BEGIN) return beginOptions;
      if (url === FINISH) return { ticket: 'ticket-1' };
      return srv.routes(url, init);
    };
    const current = () =>
      (srv.routes('/api/jobs/job-1', { method: 'GET' }) as { steps: { metadata: unknown }[] })
        .steps[0]!.metadata;
    return { srv, routes, current };
  };
  const shownIn = (calls: FetchCall[]) =>
    (calls.find((x) => x.url === BEGIN)?.body as { shown?: unknown } | undefined)?.shown;

  test('the begin names the step as rendered, with this gesture’s decision folded in', async () => {
    const step = presenceStep();
    delete (step.metadata as Record<string, unknown>).decision;
    delete (step.metadata as Record<string, unknown>).decided_at;
    const title = step.title;
    const { srv, routes, current } = presenceServer(step);
    const { mount, calls } = loadBundle(routes);
    (globalThis as unknown as Record<string, unknown>).navigator = {
      credentials: { get: async () => credential },
    };
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    buttonNamed(c, 'Approve')!.fire('click');
    await settled();
    // What it names is exactly the server's own copy after the save —
    // so an honest begin hashes to the step as it stands.
    expect(shownIn(calls)).toEqual({ title, metadata: current() });
    expect(srv.stamps.length).toBe(1);
  });

  test('a plan swapped on the server after the render is not what the begin names', async () => {
    const step = presenceStep();
    const rendered = JSON.parse(JSON.stringify(step.metadata)) as Record<string, unknown>;
    const { routes, current } = presenceServer(step);
    const { mount, calls } = loadBundle(routes);
    (globalThis as unknown as Record<string, unknown>).navigator = {
      credentials: { get: async () => credential },
    };
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    // Someone else writes the step between the render and the key press.
    routes(META, { method: 'PATCH', body: JSON.stringify({ plan: 'SWAPPED' }) });
    buttonNamed(c, 'Sign off as platform-admin')!.fire('click');
    await settled();
    const shown = shownIn(calls) as { metadata: Record<string, unknown> };
    // The begin carries what was rendered, never a fresh read — so the
    // gateway sees the difference and refuses.
    expect(shown.metadata).toEqual(rendered);
    expect(shown.metadata).not.toEqual(current());
  });
});

// ---------------------------------------------------------------------
// THE APPROVER READS THE BYTES THE PASSKEY SIGNS (adversarial re-review
// of fd7090cc, 2026-09-25). On a presence-assured step a declared field
// that already holds a value is the document being signed — an
// ops-request's `plan`, rendered on the host. It rendered as a one-line
// text input, and a text input drops newlines, so the approver read a
// plan flattened onto one line and could edit it under the signature.
// It renders read-only, exactly, and the decision never re-writes it.

describe('sign-off — a presence step shows the signed document as it is', () => {
  const PLAN = 'PLAN reap 2 pods in boss-dev\n  pod-a  Evicted\n  pod-b  Evicted\nargv: reap <plan-sha256>\n';
  const planStep = () => {
    const step = bypassStep();
    step.title = 'Approve the plan: reap-terminated-pods on forge';
    step.fields = [{ name: 'plan', field_type: 'string', required: true }];
    const md = step.metadata as Record<string, unknown>;
    delete md.approved;
    delete md.decision;
    delete md.decided_at;
    md.plan = PLAN;
    return Object.assign(step, { assurance_required: 'presence' });
  };

  test('the plan renders read-only with its newlines, and Approve is open', () => {
    const { mount } = loadBundle(() => undefined);
    const c = new FakeNode();
    mount(c, { step: planStep(), jobId: 'job-1', onUpdate() {} });
    expect(byClass(c, 'step-signoff-input').length).toBe(0);
    // Since design f623e425 D3 the plan is drawn in the block of every
    // signed key, byte for byte — and once: the declared field is not
    // drawn a second time beside it.
    // Drawn as its bytes: quoted, each line break shown as \n (6093cf13).
    expect(signedBlock(c).get('plan')).toBe(signedText(PLAN));
    expect(walk(c).filter((n) => n.textContent === signedText(PLAN)).length).toBe(1);
    expect(buttonNamed(c, 'Approve')?.disabled).toBe(false);
  });

  test('the decision patch carries the decision, never the signed plan', async () => {
    const step = planStep();
    const srv = signingServer(step);
    const { mount, calls } = loadBundle(srv.routes);
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    buttonNamed(c, 'Reject')!.fire('click');
    await settled();
    const patch = calls.find((x) => x.url === META && x.method === 'PATCH')?.body as Record<
      string,
      unknown
    >;
    expect(patch.decision).toBe('rejected');
    expect('plan' in patch).toBe(false);
  });
});

// ---------------------------------------------------------------------
// EVERY KEY THE PASSKEY SIGNS IS ON SCREEN (design f623e425 D3; backlog
// 6c9183de extends b and c, 2026-09-25). The passkey binds
// step_shape_hash(title, metadata) — every metadata key — and this
// surface drew only the declared fields: an ops-request approve step's
// verb, host, args and rendered_plan_sha256, a planted decision, or any
// key someone added, were signed by the per-role button on one tap and
// never seen. The block below is drawn from the step's own keys, and the
// ceremony refuses, before any request, a step whose block is not drawn.
//
// The plugin cannot import presence.ts (a bundle is self-contained), so
// its copy of the rendering and of the refusal is pinned HERE to the
// app's: the same text per value (signedText) and the same keys refused
// (notShown) — CLAUDE.md §9a, a fact that lives twice gets an equality
// test.

describe('sign-off — the passkey signs only what this surface drew', () => {
  const BEGIN = '/api/auth/passkey/assert/begin';
  const FINISH = '/api/auth/passkey/assert/finish';
  const beginOptions = {
    challenge_id: 'chal-1',
    publicKey: {
      challenge: 'AAAA',
      rpId: 'boss.test',
      allowCredentials: [{ type: 'public-key', id: 'AAAA' }],
      userVerification: 'required',
      timeout: 60000,
    },
  };
  const buf = () => new Uint8Array([1, 2, 3]).buffer;
  const withPasskey = () => {
    (globalThis as unknown as Record<string, unknown>).navigator = {
      credentials: {
        get: async () => ({
          id: 'cred',
          rawId: buf(),
          type: 'public-key',
          response: {
            authenticatorData: buf(),
            clientDataJSON: buf(),
            signature: buf(),
            userHandle: null,
          },
        }),
      },
    };
  };
  const PLAN = 'PLAN wipe target-a on forge\n  /dev/sdb  by-id/ata-X  1.8T\nargv: wipe target-a\n';
  // The ops-request approve step as the runner leaves it, plus a planted
  // key the runner never writes.
  const opsStep = (declared = true) => {
    const step = publishStep();
    step.title = 'Approve the plan: wipe on forge';
    step.sign_offs_required = ['platform-admin'];
    step.fields = [{ name: 'plan', field_type: 'string', required: true }];
    Object.assign(step.metadata, {
      plan: PLAN,
      verb: 'wipe',
      host: 'forge',
      args: ['target-a'],
      rendered_plan_sha256: 'ab'.repeat(32),
      planted: { by: 'someone else' },
    });
    return declared ? Object.assign(step, { assurance_required: 'presence' }) : step;
  };
  type Shown = { title: string; metadata: Record<string, unknown> };
  /** Presence-gated stamp door; records each begin's `shown` beside the
   *  block that was on screen at that instant. */
  const presenceServer = (step: ReturnType<typeof publishStep>, screen: () => FakeNode) => {
    const srv = signingServer(step);
    const begins: { shown: Shown; onScreen: Map<string, string> }[] = [];
    const routes = (url: string, init?: RequestInit) => {
      const ticket = (init?.headers as Record<string, string> | undefined)?.['x-presence-ticket'];
      if (url === SIGN && init?.method === 'POST' && !ticket) {
        return { __status: 422, required: 'presence' };
      }
      if (url === BEGIN) {
        begins.push({
          shown: (JSON.parse(String(init?.body)) as { shown: Shown }).shown,
          onScreen: signedBlock(screen()),
        });
        return beginOptions;
      }
      if (url === FINISH) return { ticket: 'ticket-1' };
      return srv.routes(url, init);
    };
    return { srv, routes, begins };
  };
  const drawn = (shown: Shown) =>
    new Map(
      Object.keys(shown.metadata)
        .sort()
        .map((k) => [k, signedText(shown.metadata[k])]),
    );

  test('a presence step draws every key its passkey would sign, as the app renders each value', () => {
    const step = opsStep();
    const { mount } = loadBundle(() => undefined);
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    const block = signedBlock(c);
    expect([...block.keys()]).toEqual(Object.keys(step.metadata).sort());
    for (const k of ['plan', 'verb', 'host', 'args', 'rendered_plan_sha256', 'planted']) {
      expect(block.get(k)).toBe(signedText((step.metadata as Record<string, unknown>)[k]));
    }
    expect(allText(c)).toContain(step.title);
  });

  test('Approve: the block on screen when the passkey is asked is exactly what it signs', async () => {
    const step = opsStep();
    const c = new FakeNode();
    const server = presenceServer(step, () => c);
    const { mount } = loadBundle(server.routes);
    withPasskey();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    buttonNamed(c, 'Approve')!.fire('click');
    await settled();

    expect(server.begins.length).toBe(1);
    const { shown, onScreen } = server.begins[0]!;
    expect(onScreen).toEqual(drawn(shown));
    // The gesture's own decision and time were drawn before the tap.
    expect(onScreen.get('decision')).toBe('approved');
    expect(onScreen.has('decided_at')).toBe(true);
    expect(server.srv.stamps.length).toBe(1);
  });

  test('the per-role button alone: a planted decision is on screen when the passkey signs it', async () => {
    const step = opsStep();
    (step.metadata as Record<string, unknown>).decision = 'approved';
    const c = new FakeNode();
    const server = presenceServer(step, () => c);
    const { mount } = loadBundle(server.routes);
    withPasskey();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    buttonNamed(c, 'Sign off as platform-admin')!.fire('click');
    await settled();

    expect(server.begins.length).toBe(1);
    const { shown, onScreen } = server.begins[0]!;
    expect(onScreen).toEqual(drawn(shown));
    expect(onScreen.get('decision')).toBe('approved');
  });

  test('a step that never declared presence: the first tap signs nothing and draws the block; the next signs what it drew', async () => {
    const step = opsStep(false);
    const c = new FakeNode();
    const server = presenceServer(step, () => c);
    const { mount, calls } = loadBundle(server.routes);
    withPasskey();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    expect(signedBlock(c).size).toBe(0);

    buttonNamed(c, 'Sign off as platform-admin')!.fire('click');
    await settled();
    expect(calls.some((x) => x.url === BEGIN)).toBe(false);
    expect(server.srv.stamps.length).toBe(0);
    // It names what the passkey would have signed unseen — the same keys
    // the app's own check names for a surface that drew nothing.
    const unseen = notShown({ title: step.title, metadata: step.metadata }, null);
    expect(allText(c)).toContain(`nothing was signed: your passkey would sign ${unseen.join(', ')}`);
    expect([...signedBlock(c).keys()]).toEqual(Object.keys(step.metadata).sort());

    buttonNamed(c, 'Sign off as platform-admin')!.fire('click');
    await settled();
    expect(server.begins.length).toBe(1);
    const { shown, onScreen } = server.begins[0]!;
    expect(onScreen).toEqual(drawn(shown));
    expect(server.srv.stamps.length).toBe(1);
  });

  test('a completed step draws no signing block — nothing is asked of a passkey there', () => {
    const step = Object.assign(opsStep(), { status: 'completed' });
    const { mount } = loadBundle(() => undefined);
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    expect(signedBlock(c).size).toBe(0);
  });
});

// ---------------------------------------------------------------------
// Backlog 6093cf13 (adversarial review of car 30674304, 2026-09-25).

const nodeText = (n: FakeNode) =>
  walk(n)
    .map((x) => x.textContent)
    .join('');

const passkeyAnswers = (answer: () => unknown) => {
  (globalThis as unknown as Record<string, unknown>).navigator = {
    credentials: { get: async () => answer() },
  };
};

/** The signing server, presence-gated on the stamp door. */
function gatedServer(step: ReturnType<typeof publishStep>) {
  const BEGIN = '/api/auth/passkey/assert/begin';
  const FINISH = '/api/auth/passkey/assert/finish';
  const srv = signingServer(step);
  const routes = (url: string, init?: RequestInit) => {
    const ticket = (init?.headers as Record<string, string> | undefined)?.['x-presence-ticket'];
    if (url === SIGN && init?.method === 'POST' && !ticket) {
      return { __status: 422, required: 'presence' };
    }
    if (url === BEGIN) {
      return {
        challenge_id: 'chal-1',
        publicKey: {
          challenge: 'AAAA',
          rpId: 'boss.test',
          allowCredentials: [{ type: 'public-key', id: 'AAAA' }],
          userVerification: 'required',
          timeout: 60000,
        },
      };
    }
    if (url === FINISH) return { ticket: 'ticket-1' };
    return srv.routes(url, init);
  };
  return { srv, routes, BEGIN, FINISH };
}

const aCredential = () => {
  const buf = () => new Uint8Array([1, 2, 3]).buffer;
  return {
    id: 'cred',
    rawId: buf(),
    type: 'public-key',
    response: { authenticatorData: buf(), clientDataJSON: buf(), signature: buf(), userHandle: null },
  };
};

// A mount's cleanup only removed its root, so a gesture begun on step A
// kept running after the rail switched to B: it drew into the detached
// tree, its own copy of "what is on screen" still matched, and the passkey
// prompt came up over B to sign A. ApprovalSurface refuses this (its
// shownNow answers from the step on screen); the plugin now does too.
describe('sign-off — a gesture that outlives its mount signs nothing', () => {
  const presenceOnly = () => {
    const step = presenceStep();
    delete (step.metadata as Record<string, unknown>).decision;
    delete (step.metadata as Record<string, unknown>).decided_at;
    return step;
  };

  test('control: the same gesture, still mounted, does ask the passkey', async () => {
    const step = presenceOnly();
    const server = gatedServer(step);
    const { mount, calls } = loadBundle(server.routes);
    passkeyAnswers(aCredential);
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    buttonNamed(c, 'Approve')!.fire('click');
    await settled();
    expect(calls.some((x) => x.url === server.BEGIN)).toBe(true);
    expect(server.srv.stamps.length).toBe(1);
  });

  test('Approve, then the rail moves on before the passkey is asked: no begin, no stamp', async () => {
    const step = presenceOnly();
    const server = gatedServer(step);
    const { mount, calls } = loadBundle(server.routes);
    passkeyAnswers(aCredential);
    const c = new FakeNode();
    const dispose = mount(c, { step, jobId: 'job-1', onUpdate() {} }) as () => void;
    buttonNamed(c, 'Approve')!.fire('click');
    dispose();
    await settled();
    expect(calls.some((x) => x.url === server.BEGIN)).toBe(false);
    expect(server.srv.stamps.length).toBe(0);
    expect(server.srv.puts).toEqual([]);
    expect(allText(c)).toContain('Nothing was signed');
    // Unmounted, nothing is drawn as signed.
    expect(signedBlock(c).size).toBe(0);
  });

  test('the role button, then the rail moves on: the same', async () => {
    const step = presenceOnly();
    const server = gatedServer(step);
    const { mount, calls } = loadBundle(server.routes);
    passkeyAnswers(aCredential);
    const c = new FakeNode();
    const dispose = mount(c, { step, jobId: 'job-1', onUpdate() {} }) as () => void;
    buttonNamed(c, 'Sign off as platform-admin')!.fire('click');
    dispose();
    await settled();
    expect(calls.some((x) => x.url === server.BEGIN)).toBe(false);
    expect(server.srv.stamps.length).toBe(0);
    expect(allText(c)).toContain('Nothing was signed');
  });

  test('the rail moves on while the passkey prompt is up: its answer is never sent', async () => {
    const step = presenceOnly();
    const server = gatedServer(step);
    const { mount, calls } = loadBundle(server.routes);
    let dispose = () => {};
    passkeyAnswers(() => {
      dispose();
      return aCredential();
    });
    const c = new FakeNode();
    dispose = mount(c, { step, jobId: 'job-1', onUpdate() {} }) as () => void;
    buttonNamed(c, 'Approve')!.fire('click');
    await settled();
    expect(calls.some((x) => x.url === server.BEGIN)).toBe(true);
    expect(calls.some((x) => x.url === server.FINISH)).toBe(false);
    expect(server.srv.stamps.length).toBe(0);
    expect(allText(c)).toContain('Nothing was signed');
  });
});

// The rendering the plugin draws and the refusal it runs are copies of
// presence.ts (a bundle cannot import it), pinned HERE on generated
// inputs — CLAUDE.md §9a. The equality used to cover signedText and one
// empty-screen notShown; canonical was re-implemented in this file's stub.
describe('sign-off — the plugin draws and refuses exactly as the app does', () => {
  const plugin = (): PluginSigned => {
    const { signed } = loadBundle(() => undefined);
    if (!signed) throw new Error('the bundle exposes no signed-rendering functions on mount');
    return signed;
  };

  test('signedText and canonical agree on generated values', () => {
    const p = plugin();
    const r = rng(0x5160ff);
    for (let i = 0; i < 3000; i++) {
      const v = genValue(r);
      expect([v, p.signedText(v), p.canonical(v)]).toEqual([v, signedText(v), canonical(v)]);
    }
  });

  test('notShown agrees on generated steps and screens', () => {
    const p = plugin();
    const r = rng(0x6093);
    const screens = (shown: ShownStep): (ShownStep | null)[] => {
      const md = shown.metadata;
      const keys = Object.keys(md);
      const k = keys[Math.floor(r() * keys.length)];
      const without = { ...md };
      if (k !== undefined) delete without[k];
      const moved = k === undefined ? { ...md } : { ...md, [k]: genValue(r) };
      return [
        null,
        { title: shown.title, metadata: JSON.parse(JSON.stringify(md)) },
        { title: genString(r), metadata: { ...md } },
        { title: shown.title, metadata: without },
        { title: shown.title, metadata: moved },
        { title: shown.title, metadata: { ...md, [genString(r)]: genValue(r) } },
        { title: shown.title, metadata: Object.fromEntries(Object.entries(md).reverse()) },
      ];
    };
    for (let i = 0; i < 500; i++) {
      const shown = { title: genString(r), metadata: genMetadata(r) };
      for (const screen of screens(shown)) {
        expect([shown, screen, p.notShown(shown, screen)]).toEqual([
          shown,
          screen,
          notShown(shown, screen),
        ]);
      }
    }
  });

  test('the overflow note agrees', () => {
    const p = plugin();
    const r = rng(3);
    for (let i = 0; i < 200; i++) {
      const text = signedText(genValue(r));
      expect(p.scrollNote(text)).toBe(scrollNote(text));
    }
  });

  test('a hostile step is drawn as the app draws it: its title, key names and values', () => {
    const step = presenceStep();
    step.title = ' Approve  the plan ';
    Object.assign(step.metadata, {
      '4​2': '42',
      host: 'fоrge‮xcod.exe',
      count: 42,
      approved: 'true',
    });
    const { mount } = loadBundle(() => undefined);
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    const rows = signedRows({ title: step.title, metadata: step.metadata });
    expect([...signedBlock(c)]).toEqual(rows.map((row) => [row.label, row.text]));
    expect(signedBlock(c).get('host')).toBe('"f\\u{043E}rge\\u{202E}xcod.exe"');
    expect(signedBlock(c).get('"4\\u{200B}2"')).toBe('"42"');
    expect(signedBlock(c).get('count')).toBe('42');
    expect(byClass(c, 'step-signed-title').map(nodeText)).toEqual([signedText(step.title)]);
  });

  test('the drawn title keeps its whitespace, and a value box scrolls rather than clips', () => {
    const css = readFileSync(new URL('../styles.css', import.meta.url), 'utf8');
    const rule = (sel: string) =>
      css.match(new RegExp(`${sel.replace('.', '\\.')}\\s*\\{[^}]*\\}`))?.[0] ?? '';
    expect(rule('.step-signed-title')).toContain('white-space: pre-wrap');
    expect(rule('.step-signed-value')).toContain('overflow: auto');
  });
});

// A value longer than its box scrolled inside it, with nothing saying so:
// rendered is not read. A box that scrolls now carries a note under it
// naming how much there is (6093cf13).
describe('sign-off — a value that scrolls in its box says so', () => {
  afterEach(() => {
    FakeNode.scrolls = () => false;
  });
  const LONG = Array.from({ length: 40 }, (_, i) => `  line ${i}`).join('\n');

  test('the long value carries the note; the short ones do not', () => {
    FakeNode.scrolls = (n) =>
      n.className === 'step-signed-value' && nodeText(n).split('\n').length > 12;
    const step = presenceStep();
    (step.metadata as Record<string, unknown>).plan = LONG;
    const { mount } = loadBundle(() => undefined);
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    const notes = byClass(c, 'step-signed-overflow').map(nodeText).filter((t) => t !== '');
    expect(notes).toEqual([scrollNote(signedText(LONG))]);
  });

  test('nothing scrolls, nothing is noted', () => {
    const step = presenceStep();
    (step.metadata as Record<string, unknown>).plan = LONG;
    const { mount } = loadBundle(() => undefined);
    const c = new FakeNode();
    mount(c, { step, jobId: 'job-1', onUpdate() {} });
    expect(byClass(c, 'step-signed-overflow').map(nodeText).filter((t) => t !== '')).toEqual([]);
  });
});
