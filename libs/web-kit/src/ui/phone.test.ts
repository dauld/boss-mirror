import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { PHONE_MAX_WIDTH, PHONE_QUERY } from './phone';

// The chrome bar and the two panels it opens each collapse at the
// phone width in their own CSS, where a constant cannot reach — so each
// is held equal to PHONE_QUERY here, and a component that drifts is
// named (CLAUDE.md 9a).
const CHROME = ['PerspectiveTabs.svelte', 'GlobalSearch.svelte', 'FeedbackControl.svelte'] as const;

const queries = (file: string): ReadonlyArray<string> =>
  [...readFileSync(join(import.meta.dir, '..', file), 'utf8').matchAll(/@media\s*(\([^)]*\))/g)].map((m) => m[1]!);

describe('the phone breakpoint', () => {
  it('is spelled from its width', () => {
    expect(PHONE_QUERY).toBe(`(max-width: ${PHONE_MAX_WIDTH}px)`);
  });

  for (const file of CHROME) {
    it(`${file} turns over at the phone width and no other`, () => {
      expect({ file, queries: queries(file) }).toEqual({ file, queries: [PHONE_QUERY] });
    });
  }
});
