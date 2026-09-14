import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// The yard STATUS page is the reading an operator uses when the map is
// too small to read, and until b6522ff9 it drew every lane the server
// states except one: `held_cars` was parsed by yard-status.ts and
// rendered nowhere, so a car held out for two reds — the one state an
// operator must act on — appeared on the yard map only. This pins the
// lane to the template the way yard-page-order.test.ts pins the yard
// page: the markup iterates the server's lane, names the strikes
// through the same `redsCell` the dock table uses, and says 'none held'
// out loud rather than going quiet — a lane that vanishes when empty is
// how this one went unread in the first place.
describe('the yard status page draws the held-cars lane', () => {
  const src = readFileSync(join(import.meta.dir, 'YardStatusPage.svelte'), 'utf8');
  const markup = src.slice(src.indexOf('</script>'));

  it('iterates status.held_cars into a table with branch, reason, reds and since', () => {
    expect(markup).toContain('{#each s.held_cars as');
    const lane = markup.slice(markup.indexOf('{#each s.held_cars as'));
    for (const cell of ['.branch', '.reason', 'redsCell(', '.parked_since']) {
      expect(lane, `the held-cars row carries ${cell}`).toContain(cell);
    }
  });

  it('draws the lane beside the dock, and says none held when it is empty', () => {
    const dock = markup.indexOf('01 — THE DOCK</div>');
    const held = markup.indexOf('HELD CARS</div>');
    const recent = markup.indexOf('02 — RECENT TRAINS</div>');
    expect(dock).toBeGreaterThan(-1);
    expect(held).toBeGreaterThan(dock);
    expect(recent).toBeGreaterThan(held);
    expect(markup).toContain('none held');
  });
});
