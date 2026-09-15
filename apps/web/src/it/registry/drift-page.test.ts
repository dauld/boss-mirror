import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// The Drift tab renders the newest `maintenance-protocol-drift` packet
// (4ae9969e, car 2 of 8f4e9cc0). This pins its three honest states to
// the template the way yard-status-page.test.ts pins the yard: the
// states are the ones the packet named, and each must be told apart
// from the others in the markup — a read that failed, a cadence that
// has not filed yet, and a packet that exists with no measurement are
// three different facts, and an empty table would collapse all three
// into "0 adrift", which is the confident wrong answer (CLAUDE.md
// §Doors).
describe('the registry drift page tells its three empty states apart', () => {
  const src = readFileSync(join(import.meta.dir, 'ProtocolDriftPage.svelte'), 'utf8');
  const markup = src.slice(src.indexOf('</script>'));

  it('reads the kind once, through the parser, at the fetch call site', () => {
    expect(src).toContain("from './drift'");
    expect(src).toContain('loadDriftPackets(');
    expect(src).toContain('newestMeasured(');
  });

  it('a failed read carries the shared failure marker and never an empty table', () => {
    const failed = markup.indexOf("page.kind === 'failed'");
    expect(failed).toBeGreaterThan(-1);
    const arm = markup.slice(failed, markup.indexOf('{:else', failed));
    expect(arm).toContain('load-failed');
    expect(arm).toContain('{page.error}');
    expect(arm).not.toContain('<table');
  });

  it('no packet yet says the 05:20 measurement has not filed', () => {
    // First filing expected 2026-09-15 05:20Z (car 1, train #376). The
    // tab lands before the first packet, so this is what renders first.
    expect(markup).toContain('the 05:20 measurement has not filed');
  });

  it('packets without a measurement are counted as failed runs, not read as agreement', () => {
    expect(markup).toContain('unmeasured');
    expect(markup).toContain('none carries a measurement');
  });

  it('the measured header names head, at, and the admitted / compared / adrift counts', () => {
    for (const field of ['measured.head', 'measured.at', 'measured.live_admitted', 'measured.fields_compared', 'drift.counts.fields']) {
      expect(markup, `the header reads ${field}`).toContain(field);
    }
    // A null head is said with the row's reason, never rendered as a blank sha.
    expect(markup).toContain('head_why');
  });

  it('draws one row per drifted field: kind, field, live version, both excerpts', () => {
    const rows = markup.indexOf('{#each newest.drift.fields as');
    expect(rows).toBeGreaterThan(-1);
    const row = markup.slice(rows, markup.indexOf('{/each}', rows));
    for (const cell of ['.kind', '.field', '.live_version', '.tree_window', '.live_window', '.at']) {
      expect(row, `the drift row carries ${cell}`).toContain(cell);
    }
    // The other two directions the script names are drawn, not dropped.
    expect(markup).toContain('drift.unauthored');
    expect(markup).toContain('drift.pending');
  });
});
