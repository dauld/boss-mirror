// sign-off.js v2 against the real bundle, stubbed host — the
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
  for (let i = 0; i < 12; i++) await Promise.resolve();
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
