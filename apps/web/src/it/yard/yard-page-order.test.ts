import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// The Train Yard reads top to bottom in the order a change travels —
// approach (gating) → dock → yard (boarding, CI) → transit → arrival →
// proof — after the delivery scoreboard (David, feedback 7d31e246). The
// blocks are hand-ordered in the template, so this reads the template
// and pins the headings' sequence; a block moved out of protocol order
// is a wrong page, not a style choice.
describe('the yard page flows in protocol order', () => {
  const src = readFileSync(join(import.meta.dir, 'YardPage.svelte'), 'utf8');
  const headings = [...src.matchAll(/(\d\d) — ([A-Z][A-Z ·]+?)(?=\s*<|\s*$)/gm)].map(
    m => `${m[1] ?? ''} ${(m[2] ?? '').trim()}`
  );

  it('numbers the sections in the order a change travels', () => {
    expect(headings).toEqual([
      '00 DELIVERY',
      '01 THE APPROACH',
      '02 LOADING DOCK',
      '03 IN THE YARD',
      '04 DEPARTED · IN TRANSIT',
      '05 RECENT ARRIVALS',
      '06 AWAITING PROOF',
    ]);
  });

  it('names the whole flow, proof included, in the subtitle and the footer line', () => {
    expect(src).toContain('Gated → parked → boarded → departed → arrived → proven');
    expect(src).toContain('ARRIVED → PROVEN');
  });
});
