import { describe, expect, it } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

// The Train Yard reads top to bottom in the order a change travels.
// Until the-yard-is-a-floor-you-can-follow that order was seven
// numbered table blocks (00 DELIVERY … 06 AWAITING PROOF), hand-ordered
// in the template and pinned here. The rail map now carries the
// protocol order itself — approach → gates → dock → track → arrivals is
// the map's geometry, tested in yard-floor.test.ts — and the deck under
// it reads: the alerts strip, the deck (departure board + entity
// panel), then the lower deck (production and signals), then the
// sections the map does not draw: the delivery scoreboard, recent
// arrivals, awaiting proof, and the flow line. The regions are still
// hand-ordered in the template, so this pins their sequence; a region
// moved out of order is a wrong page, not a style choice.
//
// The deck is FloorDeck.svelte since design fe77a1d2 car 3: the Train
// Yard page it lived in (YardPage.svelte) had nothing left of a page
// once the region map drew its floor, so the deck is a component the
// map page mounts and the page is deleted.
describe('the floor deck flows in protocol order', () => {
  const src = readFileSync(join(import.meta.dir, 'FloorDeck.svelte'), 'utf8');
  const markup = src.slice(src.indexOf('</script>'));

  // The map left the deck on design fe77a1d2 car 2: the region map
  // above it draws that region's slice of the floor, so the deck runs
  // alerts → board → entity panel, and draws no second map.
  it('lays out alerts → board → entity panel → the lower sections, in that order', () => {
    const landmarks = [
      'class="yard-alerts"',
      '<DepartureBoard',
      'class="yard-panel yard-entity"',
      '<ProductionPanel',
      '<SignalsPanel',
      'DELIVERY</div>',
      'RECENT ARRIVALS</div>',
      'AWAITING PROOF',
      'ARRIVED → PROVEN',
    ];
    const positions = landmarks.map(l => markup.indexOf(l));
    positions.forEach((p, i) => expect(p, `${landmarks[i]} is in the template`).toBeGreaterThan(-1));
    expect([...positions].sort((a, b) => a - b)).toEqual(positions);
  });

  it('names the whole flow, proof included, in the footer line', () => {
    // The subtitle that also named it was the retired page's header;
    // the map page's own header stands above the deck now.
    expect(markup).toContain('GATED → PARKED → BOARDED → <em>DEPARTED</em> → ARRIVED → PROVEN');
  });

  it('the entity panel has a home for every selectable thing on the map', () => {
    // One branch per selection kind the floor can produce — a machine
    // the map draws with nowhere to show its facts is a dead button.
    for (const kind of ['car', 'train', 'track', 'bay', 'dock', 'garage', 'approach', 'arrivals', 'cancelled', 'inspection-shed', 'conductor', 'runner', 'cluster']) {
      expect(markup, `entity panel renders sel.kind === '${kind}'`).toContain(`sel.kind === '${kind}'`);
    }
  });

  it('the factory floor is gone — the map replaced it', () => {
    expect(src).not.toContain('YardFactory');
    expect(src).not.toContain('yard-factory');
  });

  it('is a component the map page mounts, and the Train Yard page is deleted', () => {
    // No page header of its own and no `embedded` switch: the deck is
    // only ever mounted by the map page, whose page carries the one
    // header. A second mount site would be a second page. Since car N2
    // of design e765b3fc that one site is a snippet the page renders
    // under a region's map AND inside a station's panel — still one
    // `<FloorDeck`, focused on the region it is handed.
    expect(src).not.toContain('PageHeader');
    expect(src).not.toContain('embedded');
    const map = readFileSync(join(import.meta.dir, 'MapPage.svelte'), 'utf8');
    expect(map).toContain("import FloorDeck from './FloorDeck.svelte'");
    expect(map).toMatch(/<FloorDeck\s+focus=\{region\}/);
    expect(map.split('<FloorDeck').length - 1).toBe(1);
    expect(map).not.toMatch(/import YardPage|<YardPage/);
    expect(existsSync(join(import.meta.dir, 'YardPage.svelte'))).toBe(false);
  });
});
