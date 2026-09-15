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

// Car 3b: the approve. One control per adrift kind, reading the verb's
// own answers; the force path two clicks from the plain one.
describe('the drift page approves a publish through the ops-request packet', () => {
  const page = readFileSync(join(import.meta.dir, 'ProtocolDriftPage.svelte'), 'utf8');
  const control = readFileSync(join(import.meta.dir, 'ApprovePublish.svelte'), 'utf8');
  const markup = control.slice(control.indexOf('</script>'));

  it('the page groups the drift rows by kind and hands each its latest request and the execute role', () => {
    expect(page).toContain("from './approve'");
    for (const call of ['adriftKinds(', 'latestFor(', 'loadPublishRequests(', 'loadExecuteAuthorityRole(', '<ApprovePublish']) {
      expect(page, `the page uses ${call}`).toContain(call);
    }
    // The viewer is the actor: id and role come from the session, never typed.
    expect(page).toContain('session.value.user.id');
    expect(page).toContain('session.value.user.role');
  });

  it('a failed requests read withholds the controls with the failure marker, never offers them blind', () => {
    const failed = page.indexOf("requests.kind === 'failed'");
    expect(failed).toBeGreaterThan(-1);
    const arm = page.slice(failed, page.indexOf('{:else', failed));
    expect(arm).toContain('load-failed');
    expect(arm).not.toContain('<ApprovePublish');
  });

  it('the control files through approveBody + fileApprove, inside the write gate, gated by the execute role', () => {
    expect(control).toContain('approveBody(');
    expect(control).toContain('fileApprove(');
    expect(control).toContain('approveAuthority(');
    expect(markup).toContain('<WriteGate>');
    // The refusal names the role in the disabled control's title, as the Abort control does.
    expect(markup).toContain('disabled={refusal !== null');
  });

  it('the force path exists only in the force mode and enables only on the typed field name', () => {
    const force = markup.indexOf("mode.kind === 'force'");
    expect(force).toBeGreaterThan(-1);
    const arm = markup.slice(force, markup.indexOf('{:else}', force));
    expect(arm).toContain("file('force')");
    expect(arm).toContain('forceConfirmed(typed, fieldNames)');
    expect(arm).toContain('erase');
    // Nowhere else files with force.
    expect(markup.slice(0, force)).not.toContain("file('force')");
    expect(markup.slice(markup.indexOf('{:else}', force))).not.toContain("file('force')");
  });

  it('the answer is the packet link plus exit code and the FULL output, never a tail', () => {
    expect(markup).toContain('href={`/jobs/${latest.id}`}');
    expect(markup).toContain('latest.exit_code');
    expect(markup).toContain('<pre class="ap-output">{latest.output}</pre>');
    expect(control).not.toMatch(/output\.slice\(|output\.split\(/);
  });
});
