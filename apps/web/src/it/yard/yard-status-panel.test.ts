import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// The yard STATUS reading is the one an operator uses when the map is
// too small to read, and until b6522ff9 it drew every lane the server
// states except one: `held_cars` was parsed by yard-status.ts and
// rendered nowhere, so a car held out for two reds — the one state an
// operator must act on — appeared on the yard map only. This pins the
// lane to the template the way yard-page-order.test.ts pins the yard
// page: the markup iterates the server's lane, names the strikes
// through the same `redsCell` the dock table uses, and says 'none held'
// out loud rather than going quiet — a lane that vanishes when empty is
// how this one went unread in the first place.
//
// It was a page, /it/operate/yard-status, until car N3 of design
// e765b3fc (2026-09-25) made it three stations' panels on the Department
// Map — "a train's block and phase are the track's detail, and the
// boarding predicate is the dock's" — so each lane is pinned to the
// station that draws it as well as to its order.
describe('the yard status panel draws the held-cars lane', () => {
  const src = readFileSync(join(import.meta.dir, 'YardStatusPanel.svelte'), 'utf8');
  const markup = src.slice(src.indexOf('</script>'));

  it('iterates status.held_cars into a table with branch, reason, reds and since', () => {
    expect(markup).toContain('{#each s.held_cars as');
    const lane = markup.slice(markup.indexOf('{#each s.held_cars as'));
    for (const cell of ['.branch', '.reason', 'redsCell(', '.parked_since']) {
      expect(lane, `the held-cars row carries ${cell}`).toContain(cell);
    }
  });

  it('draws the lane beside the dock, and says none held when it is empty', () => {
    const dock = markup.indexOf('THE DOCK</div>');
    const held = markup.indexOf('HELD CARS</div>');
    const recent = markup.indexOf('RECENT TRAINS</div>');
    expect(dock).toBeGreaterThan(-1);
    expect(held).toBeGreaterThan(dock);
    expect(recent).toBeGreaterThan(held);
    expect(markup).toContain('none held');
  });
});

describe('each lane is drawn in the panel of the station it describes', () => {
  const src = readFileSync(join(import.meta.dir, 'YardStatusPanel.svelte'), 'utf8');
  const markup = src.slice(src.indexOf('</script>'));
  /** The station whose `{#if part === '…'}` block a lane's heading sits
   *  in — the nearest opening before it. */
  function stationOf(heading: string): string | null {
    const at = markup.indexOf(heading);
    if (at < 0) return null;
    const opens = [...markup.slice(0, at).matchAll(/\{#if part === '([a-z-]+)'\}/g)];
    return opens.length === 0 ? null : opens[opens.length - 1]![1]!;
  }

  it('the trains in flight and the recent trains are the track’s', () => {
    expect(stationOf('IN FLIGHT</div>')).toBe('track');
    expect(stationOf('RECENT TRAINS</div>')).toBe('track');
  });

  it('the dock, its boarding sentence and the held cars are the dock’s', () => {
    expect(stationOf('THE DOCK</div>')).toBe('dock');
    expect(stationOf('{s.boarding.summary}')).toBe('dock');
    expect(stationOf('HELD CARS</div>')).toBe('dock');
  });

  it('the stranded and the held greens are the garage’s', () => {
    expect(stationOf('STRANDED GREENS</div>')).toBe('garage');
    expect(stationOf('HELD GREENS</div>')).toBe('garage');
  });

  it('is a panel, not a page — no page header of its own', () => {
    expect(src).not.toContain('PageHeader');
  });
});
