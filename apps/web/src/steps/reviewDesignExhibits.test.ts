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

async function mountWith(metadata: Record<string, unknown>): Promise<Rendered> {
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
  g.fetch = (url: string) => Promise.reject(new Error(`nothing may be fetched: ${url}`));

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

  test('a bound question names its exhibit, and an exhibit with no inline html is listed, not dropped', async () => {
    const { container } = await mountWith(designWithExhibits());
    const buttons = walk(container).filter(
      (n) => n.tagName === 'BUTTON' && (n.getAttribute('title') ?? '').includes('exhibit E1'),
    );
    expect(buttons.length).toBe(1);
    const text = allText(container);
    expect(text).toContain('Warm palette board');
    expect(text).toContain('asked about in Q1');
    // E2 carries no inline html: named, with what the surface cannot do.
    expect(text).toContain('A larger board');
    expect(text).toContain('not carried inline');
    expect(text).toContain('Exhibits (2)');
  });

  test('a design with no exhibits renders no frame and no exhibits section', async () => {
    const md = designWithExhibits();
    const { container } = await mountWith({ ...md, exhibits: undefined });
    expect(walk(container).filter((n) => n.tagName === 'IFRAME')).toEqual([]);
    expect(allText(container)).not.toContain('Exhibits (');
  });
});
