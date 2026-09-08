import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// The Train Yard's first write (backlog 7a24caf3): an operator asks the
// conductor to cancel a troubled train by stamping `cancel_requested`
// on its Job. The rule for WHEN the button may appear lives in yard.ts
// as `canOfferCancel`, the stamp's shape in `cancelRequestBody`; the
// page must use both and invent neither. A source-level pin, in the
// TriageBoard.test.ts idiom, because that is where the drift would
// appear: a button copied into the in-transit block, a hand-built
// PATCH body, a role string retyped.
describe('the yard page offers cancel only through the rule', () => {
  const src = readFileSync(join(import.meta.dir, 'YardPage.svelte'), 'utf8');
  // Comments discuss the rule freely; only executable code is pinned.
  const code = src
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:])\/\/.*$/gm, '$1');

  test('renders the train block with its partition named — in-yard and in-transit', () => {
    // The button's first guard is WHICH list the train is in, and the
    // snippet cannot know that unless the caller says so.
    expect(code).toMatch(/split\.inYard as t \(t\.id\)\}\{@render trainBlock\(t, 'in-yard'\)\}/);
    expect(code).toMatch(
      /split\.inTransit as t \(t\.id\)\}\{@render trainBlock\(t, 'in-transit'\)\}/,
    );
  });

  test('a single selected train is rendered with its partition read off the same split', () => {
    // The entity panel shows one train at a time; the cancel rule still
    // needs WHICH side of the departure line it is on, and the only
    // honest answer is the split the track view renders — not a
    // partition retyped at the render site.
    expect(code).toMatch(/\{@render trainBlock\(t, partitionOf\(t\)\)\}/);
    expect(code).toMatch(/partitionOf = \(t: TrainRow\): YardPartition =>\s*split\.inYard\.some/);
  });

  test('the button is guarded by canOfferCancel, with the partition and the viewer', () => {
    expect(code).toMatch(/from '\.\/yard'/);
    expect(code).toMatch(/canOfferCancel\(t,\s*partition,\s*viewerPrivileged\)/);
    // Exactly one place offers it.
    expect(code.match(/canOfferCancel\(/g)?.length).toBe(1);
  });

  test('the PATCH body comes from the builder, never hand-shaped', () => {
    expect(code).toMatch(/cancelRequestBody\(/);
    expect(code).toMatch(/method:\s*'PATCH'/);
    expect(code).toMatch(/\/api\/jobs\/\$\{encodeURIComponent\([a-zA-Z.]+\)\}\/metadata/);
    // No object literal with the stamp's key: one definition, in yard.ts.
    expect(code).not.toMatch(/cancel_requested\s*:/);
  });

  test('privilege is the session role compared to CANCEL_ROLE, not a retyped string', () => {
    expect(code).toMatch(/session\.value\.user\.role\s*===\s*CANCEL_ROLE/);
    expect(code).not.toContain("'platform-admin'");
  });

  test('the page says it writes, and how the write is gated', () => {
    // The header comment used to say every read is audit-readonly-safe
    // by construction; a page with a write has to say so.
    const header = src.slice(0, src.indexOf('import { onMount }'));
    expect(header).toMatch(/cancel_requested/);
    expect(header).toMatch(/job:update/);
  });

  test('the pending chip says the effect is asynchronous', () => {
    expect(src).toMatch(/cancel requested — the conductor acts within 10 min/);
  });
});
