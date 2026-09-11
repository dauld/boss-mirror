// sign-off.js (v2, then v3) against the real bundle, stubbed host — the
// correctionVerdictPlugin posture. The shapes pinned here are the two
// David hit blind on 2026-08-19 (19db52de): a decision sign-off whose
// case and contract never rendered, and a required-at-done field whose
// completion 400 was swallowed. v1's row was retired live for exactly
// these gaps; this suite is what earns re-publishing it.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';

const BUNDLE = new URL('../../../../infra/step-plugins/sign-off.js', import.meta.url);

type Handler = (ev: { target: FakeNode }) => void;

class FakeNode {
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
    return Promise.resolve({
      ok: (result as { __status?: number }).__status === undefined,
      status: (result as { __status?: number }).__status ?? 200,
      json: async () => result,
      text: async () =>
        typeof (result as { __text?: string }).__text === 'string'
          ? (result as { __text: string }).__text
          : JSON.stringify(result),
    });
  };
  // eslint-disable-next-line no-new-func
  new Function(readFileSync(BUNDLE, 'utf8'))();
  if (!mountFn) throw new Error('bundle registered no plugin');
  return { mount: mountFn as (c: unknown, p: unknown) => unknown, calls };
}

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

const methods = (calls: FetchCall[]) => calls.map((c) => c.method);

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
