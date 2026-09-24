// Exhibits on the review-design surface (design 26a89f11, backlog
// 73ef81fa): a design packet carries its own HTML renderings, and the
// surface — which runs in the reviewer's AUTHENTICATED session — shows
// them only inside a sandboxed frame. What is pinned here is the
// security posture the design decided, read off the DOM the real bundle
// builds:
//
//   - every exhibit renders in an <iframe> whose sandbox is exactly
//     `allow-scripts` (never allow-same-origin, so its origin is opaque),
//     loaded by `srcdoc` whose FIRST head element is a CSP of
//     `default-src 'none'` with inline style and script only;
//   - the exhibit's markup never reaches this document — no node's
//     innerHTML carries it, and renderMarkdown never sees it;
//   - the doc's markdown prose still goes through the host's
//     escape-first renderer, exactly as before;
//   - a question's binding renders a control naming its exhibit, and an
//     exhibit with no inline html is listed, never dropped.
//
// Same posture as reviewDesignPlugin.test.ts: load the REAL bundle
// against a stubbed host, because nothing compiles these files.

import { describe, expect, test } from 'bun:test';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

const BUNDLE = new URL('../../../../infra/step-plugins/review-design.js', import.meta.url);

type MountFn = (
  container: unknown,
  props: { step: unknown; jobId: string; onUpdate: () => void },
) => unknown;

/// A DOM node that remembers what it was made of: its tag, its
/// attributes, and every innerHTML assignment — which is the channel an
/// exhibit must never travel.
class FakeNode {
  tagName: string;
  className = '';
  textContent = '';
  innerHTML = '';
  style: Record<string, string> = {};
  attrs: Record<string, string> = {};
  children: FakeNode[] = [];
  classList = {
    add: () => {},
    remove: () => {},
    toggle: () => {},
  };
  constructor(tag = '#text') {
    this.tagName = tag.toUpperCase();
  }
  appendChild(c: FakeNode) {
    this.children.push(c);
    return c;
  }
  replaceChildren(...cs: FakeNode[]) {
    this.children = cs;
  }
  remove() {}
  addEventListener() {}
  scrollIntoView() {}
  focus() {}
  setAttribute(k: string, v: string) {
    this.attrs[k] = String(v);
  }
  getAttribute(k: string) {
    return this.attrs[k] ?? null;
  }
}

function walk(n: FakeNode): FakeNode[] {
  return [n, ...n.children.flatMap(walk)];
}

function allText(n: FakeNode): string {
  return walk(n)
    .map((x) => x.textContent)
    .join(' ');
}

type Rendered = { container: FakeNode; renderedMarkdown: string[] };

type FetchFn = (url: string, init?: Record<string, unknown>) => Promise<unknown>;

const NO_FETCH: FetchFn = (url) => Promise.reject(new Error(`nothing may be fetched: ${url}`));

async function mountWith(
  metadata: Record<string, unknown>,
  fetchImpl: FetchFn = NO_FETCH,
): Promise<Rendered> {
  let registered: MountFn | null = null;
  const renderedMarkdown: string[] = [];
  const g = globalThis as unknown as Record<string, unknown>;
  g.Node = FakeNode;
  g.window = {
    __boss_register_step_plugin: (_kind: string, mount: MountFn) => {
      registered = mount;
    },
    // The host's escape-first renderer. Recorded, so the test can say
    // exactly what went through it.
    __boss_markdown: (md: string) => {
      renderedMarkdown.push(md);
      return '<p>RENDERED-PROSE</p>';
    },
    sessionStorage: { getItem: () => null, setItem() {}, removeItem() {} },
  };
  g.document = {
    getElementById: () => null,
    createElement: (tag: string) => new FakeNode(tag),
    createTextNode: (s: string) => {
      const n = new FakeNode();
      n.textContent = s;
      return n;
    },
    head: new FakeNode('head'),
  };
  g.fetch = fetchImpl;

  // eslint-disable-next-line no-new-func
  new Function(readFileSync(BUNDLE, 'utf8'))();
  if (!registered) throw new Error('bundle registered no plugin');

  const container = new FakeNode('div');
  (registered as MountFn)(container, {
    step: { id: 'step-1', kind: 'review-design', status: 'ready', title: 'review', metadata },
    jobId: 'job-1',
    onUpdate() {},
  });
  for (let i = 0; i < 10; i++) await Promise.resolve();
  // A by-reference exhibit's fetch and digest settle on later turns.
  await new Promise((r) => setTimeout(r, 30));
  return { container, renderedMarkdown };
}

// An exhibit that TRIES everything the sandbox and the policy exist to
// stop. Its bytes are the payload; the frame is what makes them inert.
const HOSTILE =
  '<style>b{color:red}</style><b>warm palette</b>' +
  '<script>parent.document.title = "owned"; fetch("/api/jobs")</script>';

const designWithExhibits = () => ({
  title: 'The brand theme',
  markdown: '# The claim\n\nTwo palettes; pick one.',
  questions: [
    { anchor: 'Q1', title: 'Which palette?', proposal: 'The warm one.', exhibits: ['E1'] },
    { anchor: 'Q2', title: 'Ship it this week?', proposal: 'Yes.' },
  ],
  exhibits: [
    { anchor: 'E1', title: 'Warm palette board', html: HOSTILE },
    { anchor: 'E2', title: 'A larger board', file_ref: 'f-123' },
  ],
  resolutions: [],
});

describe('exhibits on the review-design surface', () => {
  test('an exhibit renders only in an allow-scripts sandbox, by srcdoc, under default-src none', async () => {
    const { container } = await mountWith(designWithExhibits());
    const frames = walk(container).filter((n) => n.tagName === 'IFRAME');
    expect(frames.length).toBe(1);
    const frame = frames[0]!;

    // Exactly `allow-scripts`: the same-origin token would hand the
    // exhibit this page's origin, cookies and API as the reviewer.
    expect(frame.getAttribute('sandbox')).toBe('allow-scripts');
    expect(frame.getAttribute('sandbox')).not.toContain('same-origin');
    expect(frame.getAttribute('src')).toBeNull();

    const srcdoc = frame.getAttribute('srcdoc') ?? '';
    const head = srcdoc.slice(0, srcdoc.indexOf(HOSTILE));
    expect(head).toContain(
      `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; ` +
        `style-src 'unsafe-inline'; script-src 'unsafe-inline'">`,
    );
    // The policy precedes every byte of the exhibit, and the exhibit
    // rides verbatim inside the frame's own document.
    expect(srcdoc.indexOf('Content-Security-Policy')).toBeLessThan(srcdoc.indexOf(HOSTILE));
    expect(srcdoc).toContain(HOSTILE);
  });

  test('the exhibit never reaches this document, and the prose still goes through renderMarkdown', async () => {
    const { container, renderedMarkdown } = await mountWith(designWithExhibits());
    const injected = walk(container).filter((n) => n.innerHTML.includes('warm palette'));
    expect(injected.map((n) => n.tagName)).toEqual([]);
    // The prose: through the escape-first renderer, once, and only it.
    expect(renderedMarkdown).toEqual(['# The claim\n\nTwo palettes; pick one.']);
    expect(walk(container).some((n) => n.innerHTML === '<p>RENDERED-PROSE</p>')).toBe(true);
  });

  test('a bound question names its exhibit, and an exhibit that cannot render is listed, not dropped', async () => {
    const { container } = await mountWith(designWithExhibits());
    const buttons = walk(container).filter(
      (n) => n.tagName === 'BUTTON' && (n.getAttribute('title') ?? '').includes('exhibit E1'),
    );
    expect(buttons.length).toBe(1);
    const text = allText(container);
    expect(text).toContain('Warm palette board');
    expect(text).toContain('asked about in Q1');
    // E2's file could not be fetched here: named, with what failed.
    expect(text).toContain('A larger board');
    expect(text).toContain('file f-123 could not be fetched');
    expect(text).toContain('Exhibits (2)');
  });

  test('an element carrying neither html nor a file_ref is listed as having nothing to render', async () => {
    const md = designWithExhibits();
    const { container } = await mountWith({
      ...md,
      exhibits: [{ anchor: 'E3', title: 'An empty promise' }],
    });
    const text = allText(container);
    expect(text).toContain('An empty promise');
    expect(text).toContain('carries neither inline html nor a file_ref');
    expect(walk(container).filter((n) => n.tagName === 'IFRAME')).toEqual([]);
  });

  test('a design with no exhibits renders no frame and no exhibits section', async () => {
    const md = designWithExhibits();
    const { container } = await mountWith({ ...md, exhibits: undefined });
    expect(walk(container).filter((n) => n.tagName === 'IFRAME')).toEqual([]);
    expect(allText(container)).not.toContain('Exhibits (');
  });
});

// THE FILE_REFS ARM (design 26a89f11): an exhibit over the 256 KB inline
// bound rides as `{anchor, title, file_ref, sha256, size_bytes}`. The
// surface fetches the bytes as the reviewer, checks them against the
// record, and hands them to THE SAME frame an inline exhibit gets — so
// the security pins above are asserted again here, on the frame a
// by-reference exhibit ends in.
describe('a by-reference exhibit on the review-design surface', () => {
  const BIG = `${HOSTILE}${'<i>frame</i>'.repeat(40)}`;
  const BIG_BYTES = new TextEncoder().encode(BIG);
  const BIG_SHA = createHash('sha256').update(BIG_BYTES).digest('hex');

  const byRef = (over: Record<string, unknown> = {}) => ({
    title: 'The motion prototype',
    markdown: '# The claim',
    questions: [{ anchor: 'Q1', title: 'Which motion?', proposal: 'This one.', exhibits: ['E2'] }],
    exhibits: [
      {
        anchor: 'E2',
        title: 'Motion prototype',
        file_ref: 'f-123',
        sha256: BIG_SHA,
        size_bytes: BIG_BYTES.length,
        ...over,
      },
    ],
    resolutions: [],
  });

  function serving(body: Uint8Array | string, status = 200) {
    const calls: { url: string; init?: Record<string, unknown> }[] = [];
    const fetchImpl: FetchFn = (url, init) => {
      calls.push({ url, init });
      return Promise.resolve(new Response(body, { status }));
    };
    return { calls, fetchImpl };
  }

  test('its bytes are fetched as the reviewer and rendered in the same sandboxed frame', async () => {
    const { calls, fetchImpl } = serving(BIG_BYTES);
    const { container } = await mountWith(byRef(), fetchImpl);

    expect(calls.map((c) => c.url)).toEqual(['/api/files/f-123']);
    expect(calls[0]!.init?.credentials).toBe('same-origin');

    const frames = walk(container).filter((n) => n.tagName === 'IFRAME');
    expect(frames.length).toBe(1);
    const frame = frames[0]!;
    expect(frame.getAttribute('sandbox')).toBe('allow-scripts');
    expect(frame.getAttribute('src')).toBeNull();
    const srcdoc = frame.getAttribute('srcdoc') ?? '';
    expect(srcdoc.slice(0, srcdoc.indexOf(HOSTILE))).toContain(
      `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; ` +
        `style-src 'unsafe-inline'; script-src 'unsafe-inline'">`,
    );
    expect(srcdoc).toContain(BIG);
    // Never parsed into this document.
    expect(walk(container).filter((n) => n.innerHTML.includes('warm palette'))).toEqual([]);

    const text = allText(container);
    expect(text).toContain('sha256 checked against the record');
    expect(text).toContain('file f-123');
    expect(text).toContain('asked about in Q1');
  });

  test('bytes that do not match the recorded sha256 are refused, naming both digests', async () => {
    const { fetchImpl } = serving(BIG_BYTES);
    const wrong = 'ab'.repeat(32);
    const { container } = await mountWith(byRef({ sha256: wrong }), fetchImpl);
    expect(walk(container).filter((n) => n.tagName === 'IFRAME')).toEqual([]);
    const text = allText(container);
    expect(text).toContain('does not match');
    expect(text).toContain(wrong);
    expect(text).toContain(BIG_SHA);
  });

  test('a store that answers other bytes, or no bytes, is refused rather than rendered', async () => {
    // A switched-off store answers 200 with an envelope: wrong size.
    const off = serving('{"kind":"unconfigured"}');
    let r = await mountWith(byRef(), off.fetchImpl);
    expect(walk(r.container).filter((n) => n.tagName === 'IFRAME')).toEqual([]);
    expect(allText(r.container)).toContain('the record says');

    // A detached file answers 410.
    const gone = serving('file detached', 410);
    r = await mountWith(byRef(), gone.fetchImpl);
    expect(walk(r.container).filter((n) => n.tagName === 'IFRAME')).toEqual([]);
    expect(allText(r.container)).toContain('HTTP 410');
  });

  test('a file_ref with no recorded digest still renders, and says it was not checked', async () => {
    const { fetchImpl } = serving(BIG_BYTES);
    const { container } = await mountWith(
      byRef({ sha256: undefined, size_bytes: undefined }),
      fetchImpl,
    );
    expect(walk(container).filter((n) => n.tagName === 'IFRAME').length).toBe(1);
    expect(allText(container)).toContain('sha256 NOT checked');
  });
});
