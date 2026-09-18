import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// The interaction crawl (car f2b8a01c, KNOWN_GAPS #7) found every step
// node on /it/registry/{kind} to be a <button> that does nothing:
// StepDag renders its nodes as buttons wired to an optional
// onNodeClick, and WorkflowDetailPage, StepDag's one consumer with no
// handler, never passed one (backlog 68409b1b). Two pins, one per half
// of the fix: the page now passes a handler that focuses the step's
// definition card below the diagram, and StepDag renders a node as a
// button ONLY when a handler is given — a control that does nothing is
// not a control.
const read = (rel: string): string =>
  readFileSync(join(import.meta.dir, rel), 'utf8').replace(/\s+/g, ' ');

describe('the workflow detail page wires StepDag nodes to the step definitions', () => {
  const src = read('WorkflowDetailPage.svelte');
  const markup = src.slice(src.indexOf('</script>'));

  it('passes a node handler and the selection to StepDag', () => {
    const dag = markup.slice(markup.indexOf('<StepDag'), markup.indexOf('/>', markup.indexOf('<StepDag')));
    expect(dag).toContain('onNodeClick=');
    expect(dag).toContain('selectedId=');
  });

  it('a step card is addressable by its slug and shows when it is focused', () => {
    const card = markup.slice(markup.indexOf('{#snippet stepCard'), markup.indexOf('{/snippet}'));
    expect(card).toContain('stepCardId(step.title)');
    expect(card).toContain('class:focused=');
    // The diff renders one slug twice; only the primary list is addressable.
    expect(card).toContain('focusable');
  });
});

describe('StepDag renders a node as a button only when it has a handler', () => {
  const src = read('../jobs/StepDag.svelte');
  const markup = src.slice(src.indexOf('</script>'));

  it('the node element is chosen by the presence of onNodeClick', () => {
    const node = markup.slice(markup.indexOf('{#each layout.placed as n'), markup.indexOf('{/each}', markup.indexOf('{#each layout.placed as n')));
    const button = node.indexOf('<button');
    const card = node.indexOf('<div class="node inert');
    expect(button).toBeGreaterThan(-1);
    expect(card).toBeGreaterThan(-1);
    // The button arm is guarded by the handler; the card is its else.
    expect(node.slice(0, button)).toContain('{#if onNodeClick}');
    expect(node.slice(button, card)).toContain('{:else}');
  });
});
