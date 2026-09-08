import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// The Train Yard reads top to bottom in the order a change travels.
// Until the-yard-is-a-floor-you-can-follow that order was seven
// numbered table blocks (00 DELIVERY … 06 AWAITING PROOF), hand-ordered
// in the template and pinned here. The rail map now carries the
// protocol order itself — approach → gates → dock → track → arrivals is
// the map's geometry, tested in yard-floor.test.ts — and the page reads:
// the alerts strip, the map, the deck (departure board + entity panel),
// then the sections the map does not draw: the delivery scoreboard,
// recent arrivals, awaiting proof, and the flow line. The regions are
// still hand-ordered in the template, so this pins their sequence; a
// region moved out of order is a wrong page, not a style choice.
describe('the yard page flows in protocol order', () => {
  const src = readFileSync(join(import.meta.dir, 'YardPage.svelte'), 'utf8');
  const markup = src.slice(src.indexOf('</script>'));

  it('lays out alerts → map → board → entity panel → the lower sections, in that order', () => {
    const landmarks = [
      'class="yard-alerts"',
      '<YardMap',
      '<DepartureBoard',
      'class="yard-panel yard-entity"',
      'DELIVERY</div>',
      'RECENT ARRIVALS</div>',
      'AWAITING PROOF',
      'ARRIVED → PROVEN',
    ];
    const positions = landmarks.map(l => markup.indexOf(l));
    positions.forEach((p, i) => expect(p, `${landmarks[i]} is in the template`).toBeGreaterThan(-1));
    expect([...positions].sort((a, b) => a - b)).toEqual(positions);
  });

  it('names the whole flow, proof included, in the subtitle and the footer line', () => {
    expect(src).toContain('Gated → parked → boarded → departed → arrived → proven');
    expect(src).toContain('ARRIVED → PROVEN');
  });

  it('the entity panel has a home for every selectable thing on the map', () => {
    // One branch per selection kind the floor can produce — a machine
    // the map draws with nowhere to show its facts is a dead button.
    for (const kind of ['car', 'train', 'track', 'bay', 'dock', 'garage', 'approach', 'arrivals', 'conductor', 'runner', 'cluster']) {
      expect(markup, `entity panel renders sel.kind === '${kind}'`).toContain(`sel.kind === '${kind}'`);
    }
  });

  it('the factory floor is gone — the map replaced it', () => {
    expect(src).not.toContain('YardFactory');
    expect(src).not.toContain('yard-factory');
  });
});
