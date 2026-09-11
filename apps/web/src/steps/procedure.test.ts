// A step's authored instructions must reach the person doing it
// (backlog 3c640ac3).
//
// David, 2026-09-11, having completed the `fold` step on design-doc
// dacea60d exactly right: "I am not sure that I completed the step
// right." The step's own procedure text answers him verbatim —
// "'Nothing' is a legitimate answer and still has to be written down
// with its reason" — and no surface rendered it. The uncertainty was
// made by the surface, not by the protocol.
//
// Two halves, in the idioms this directory already uses: the
// resolution is a pure function (decisionContext.ts), and where the
// panel is mounted is a source-level pin (TriageBoard.test.ts) because
// `bun test` has no Svelte pass, so a component cannot be mounted here.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { PROCEDURE_KEY, procedureFromStep } from './procedure';

/// The real thing, off the live step: three paragraphs, an emphasised
/// sentence, and a "do NOT" clause. A one-line fixture would not prove
/// the panel can carry what the registry actually authors.
const FOLD_PROCEDURE = [
  "State what CURRENT TRUTH gains from this doc now that its questions are answered, and where. 'Nothing' is a legitimate answer and still has to be written down with its reason — that is the whole point of the step existing.",
  'The fold is the current-truth reading, assembled per topic, with the decision history beside it rather than inlined (Q2). Today that is docs/architecture-decisions.md, maintained by hand because merging prose has nuance a generator cannot judge. This step does not automate that judgement; it makes skipping it visible.',
  'Do NOT flush decisions into a per-doc markdown file. Q4 settled that the decisions live on the packet and the fold reads them from there; writing them back into files is work in the direction we are abandoning.',
].join('\n\n');

describe('a step presents its own procedure', () => {
  test('the fold step’s authored instructions resolve intact', () => {
    const resolved = procedureFromStep({
      procedure: FOLD_PROCEDURE,
      fold_change: 'Nothing - this is just visualization exploration',
    });
    expect(resolved).toBe(FOLD_PROCEDURE);
    // The sentence David could not see, and the paragraph breaks that
    // make three paragraphs three paragraphs.
    expect(resolved).toContain("'Nothing' is a legitimate answer");
    expect(resolved).toContain('\n\n');
  });

  test('a step with no procedure resolves to nothing at all', () => {
    // The absence must be an absence, not an empty panel: the vast
    // majority of steps author no procedure and must render exactly as
    // they do today.
    expect(procedureFromStep({})).toBeNull();
    expect(procedureFromStep({ authority_role: 'platform-admin' })).toBeNull();
    expect(procedureFromStep({ procedure: '   \n ' })).toBeNull();
    expect(procedureFromStep({ procedure: '' })).toBeNull();
    // Not a string is not prose — a malformed row renders nothing
    // rather than `[object Object]`.
    expect(procedureFromStep({ procedure: 42 })).toBeNull();
    expect(procedureFromStep({ procedure: { text: 'x' } })).toBeNull();
  });

  test('the metadata key lives in one place', () => {
    expect(PROCEDURE_KEY).toBe('procedure');
  });
});

/// Comments may discuss the key freely; only executable code is pinned.
function codeOf(path: string): string {
  return readFileSync(new URL(path, import.meta.url), 'utf8')
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:])\/\/.*$/gm, '$1');
}

describe('the procedure panel', () => {
  const panel = codeOf('./StepProcedure.svelte');

  test('resolves through the pure function, inventing no second rule', () => {
    expect(panel).toContain("from './procedure'");
    expect(panel).toMatch(/procedureFromStep\(/);
    expect(panel).not.toMatch(/\[['"]procedure['"]\]/);
  });

  test('renders the prose as markdown, through the escape-first renderer', () => {
    // The precedent is DecisionContext: web-kit's renderMarkdown is
    // safe for {@html} because every character is escaped before any
    // tag it emits. A <pre> dump or raw interpolation would flatten
    // three paragraphs into one wall — which is the shape the generic
    // surface already showed it in.
    expect(panel).toContain("from '@boss/web-kit/markdown'");
    expect(panel).toMatch(/\{@html renderMarkdown\(/);
  });

  test('renders nothing when there is no procedure', () => {
    expect(panel).toMatch(/\{#if procedure\}/);
  });
});

describe('the panel is mounted where steps are completed', () => {
  const dispatcher = codeOf('./StepSurface.svelte');

  test('StepSurface mounts it for plugin-backed and platform surfaces alike', () => {
    // StepSurface is the one dispatcher behind every surface that
    // completes a step — JobDetailPage, StepFocusPage, StepGraph and My
    // Day's DecideModal all mount it — so this is the single mount
    // point. It sits ABOVE the plugin fork on purpose: not one of the
    // thirteen plugin bundles under infra/step-plugins reads
    // `procedure`, so a plugin-backed step is as blind to its own
    // instructions as the generic surface was.
    expect(dispatcher).toContain("import StepProcedure from './StepProcedure.svelte'");
    const mount = dispatcher.indexOf('<StepProcedure');
    const fork = dispatcher.indexOf('{#if pluginAvailable === true}');
    expect(mount).toBeGreaterThan(-1);
    expect(fork).toBeGreaterThan(-1);
    expect(mount).toBeLessThan(fork);
  });

  test('the generic surface does not also dump it as raw context', () => {
    // GenericSurface renders every undeclared string in metadata as
    // prose, so `procedure` DID reach the screen there — flattened into
    // a single paragraph under a lowercase key, below the assignee and
    // due-date rows. The panel above replaces that, and the key it
    // hides is imported, never retyped (CLAUDE.md §9a).
    const generic = codeOf('./GenericSurface.svelte');
    expect(generic).toContain("from './procedure'");
    expect(generic).toMatch(/HIDDEN_KEYS[\s\S]{0,400}PROCEDURE_KEY/);
  });
});
