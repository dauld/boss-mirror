// The review-design bundle's behaviour when the packet carries nothing
// it can review.
//
// This file was three tests about the docs-API fetch: a real 404 ("docs
// ride trains"), the empty 200 a front that does not route
// /api/design/* answers (6f40b23f), and the job-metadata last resort.
// The fetch is gone — the corpus index it called was deleted on
// 2026-09-10 (backlog f5da586c) and the packet is the doc — so the
// first two are replaced by one test on what a pointer-only packet now
// says, and the third is unchanged because the job metadata is still
// the last place the bundle looks.
//
// Same test posture as correctionVerdictPlugin.test.ts: load the REAL
// bundle against a stubbed host, because nothing compiles or
// type-checks these files and a broken bundle renders nothing rather
// than degrading.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';

const BUNDLE = new URL('../../../../infra/step-plugins/review-design.js', import.meta.url);

type MountFn = (
  container: unknown,
  props: { step: unknown; jobId: string; onUpdate: () => void },
) => unknown;

/// Enough DOM for this bundle's h() helper: instanceof Node checks,
/// createTextNode, replaceChildren, setAttribute. Anything else it
/// reaches for throws, which is the signal we want.
class FakeNode {
  className = '';
  textContent = '';
  innerHTML = '';
  style: Record<string, string> = {};
  children: FakeNode[] = [];
  appendChild(c: FakeNode) {
    this.children.push(c);
    return c;
  }
  replaceChildren(...cs: FakeNode[]) {
    this.children = cs;
  }
  remove() {}
  addEventListener() {}
  setAttribute() {}
  querySelector() {
    return null;
  }
  querySelectorAll() {
    return [] as FakeNode[];
  }
}

function allText(n: FakeNode): string {
  return [n.textContent, ...n.children.map(allText)].join(' ');
}

type FetchStub = (url: string) => Promise<unknown>;

function loadBundle(fetchStub: FetchStub): { kind: string; mount: MountFn } {
  let registered: { kind: string; mount: MountFn } | null = null;
  const g = globalThis as unknown as Record<string, unknown>;
  g.Node = FakeNode;
  g.window = {
    __boss_register_step_plugin: (kind: string, mount: MountFn) => {
      registered = { kind, mount };
    },
  };
  g.document = {
    getElementById: () => null,
    createElement: () => new FakeNode(),
    createTextNode: (s: string) => {
      const n = new FakeNode();
      n.textContent = s;
      return n;
    },
    head: new FakeNode(),
  };
  g.fetch = fetchStub;

  // eslint-disable-next-line no-new-func
  new Function(readFileSync(BUNDLE, 'utf8'))();

  if (!registered) throw new Error('bundle registered no plugin');
  return registered;
}

const pointerOnlyStep = () => ({
  id: 'step-1',
  kind: 'review-design',
  status: 'ready',
  metadata: { doc_path: 'docs/design/example.md', resolutions: [] },
});

/// mount() fires load() without awaiting it; drain the microtask queue
/// so the fetch round trip and its renders have happened.
async function settled() {
  for (let i = 0; i < 10; i++) await Promise.resolve();
}

describe('the review-design bundle with nothing to review', () => {
  test('registers the review-design kind', () => {
    const { kind } = loadBundle(() => Promise.reject(new Error('offline')));
    expect(kind).toBe('review-design');
  });

  test('a pointer-only packet names the pointer and never fetches a docs API', async () => {
    // The packet carries `doc_path` and nothing else. Until 2026-09-10
    // this fetched the corpus index; now there is no service to ask, so
    // the surface must say that plainly and say what replaces it. Any
    // fetch at all is a failure, so the stub rejects everything.
    const asked: string[] = [];
    const { mount } = loadBundle((url: string) => {
      asked.push(url);
      return Promise.reject(new Error(`nothing may be fetched: ${url}`));
    });
    const container = new FakeNode();
    mount(container, { step: pointerOnlyStep(), jobId: 'job-1', onUpdate() {} });
    await settled();

    const text = allText(container);
    expect(text).toContain('carries only a pointer');
    expect(text).toContain('docs/design/example.md');
    // It points at the verb that files a reviewable packet rather than
    // telling the reader to wait for a train that will not help.
    expect(text).toContain('boss design');
    expect(text).not.toContain('docs ride');
    expect(asked.filter((u) => u.includes('/api/design/'))).toEqual([]);
    // Carried over from the empty-200 test, which is where it was
    // found: with `questions` empty the completion gate read a failed
    // load as "no questions" and offered to mark the review done on top
    // of the error. An unreadable doc is not reviewable.
    expect(text).not.toContain('Mark reviewed');
  });

  // A design-doc packet filed with its questions/prose on the JOB
  // metadata rather than the step reaches review as an empty step.
  // acedf981 and the `[sim] decision-routing probe` packets did exactly
  // this and dead-ended at "nothing to review" while their content sat
  // one fetch away. The job is the last place the bundle looks.
  test('content on the job renders instead of dead-ending at nothing-to-review', async () => {
    const emptyStep = {
      id: 'step-1',
      kind: 'review-design',
      status: 'ready',
      metadata: { resolutions: [] },
    };
    const { mount } = loadBundle((url: string) => {
      if (url.includes('/api/jobs/job-1')) {
        return Promise.resolve({
          ok: true,
          status: 200,
          json: async () => ({
            metadata: {
              title: 'Agent orientation',
              markdown: '# Orientation\n\nbody',
              questions: [{ anchor: 'Q1', title: 'First brick?' }],
            },
          }),
        });
      }
      return Promise.reject(new Error(`unexpected fetch: ${url}`));
    });
    const container = new FakeNode();
    mount(container, { step: emptyStep, jobId: 'job-1', onUpdate() {} });
    await settled();

    const text = allText(container);
    expect(text).toContain('First brick?');
    expect(text).not.toContain('nothing to review');
  });
});
