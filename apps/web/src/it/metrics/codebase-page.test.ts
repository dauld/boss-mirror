import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// The codebase page reads the daily `maintenance-codebase-metrics`
// packets. This pins its three empty states to the template the way
// drift-page.test.ts pins the Drift tab's: a read that FAILED, a
// cadence that has not filed yet (zero packets, a successful read),
// and packets that exist with no measurement are three different
// facts. Until 2026-09-18 the second wore `.load-failed` — the ONE
// class that says "this read failed" (tests/mocked/_routes.ts) — on a
// read that had answered 200 and `[]`, which the interaction crawl's
// empty-backend leg reported on /it/codebase and /it/design/codebase
// (car f2b8a01c, backlog 409feafd).
describe('the codebase page tells its three empty states apart', () => {
  const src = readFileSync(join(import.meta.dir, 'CodebaseTrendPage.svelte'), 'utf8');
  // Whitespace collapsed: a phrase that wraps in the source is still
  // one phrase on the page.
  const markup = src.slice(src.indexOf('</script>')).replace(/\s+/g, ' ');

  const arm = (marker: string): string => {
    const at = markup.indexOf(marker);
    expect(at, `the template has an arm for ${marker}`).toBeGreaterThan(-1);
    return markup.slice(at, markup.indexOf('{:else', at));
  };

  it('a failed read carries the shared failure marker', () => {
    const failed = arm("page.kind === 'failed'");
    expect(failed).toContain('load-failed');
    expect(failed).toContain('{page.error}');
  });

  it('no packet yet is an honest empty, not a failed read', () => {
    const none = arm('ready.total === 0');
    expect(none).toContain('has not filed');
    expect(none).not.toContain('load-failed');
  });

  it('packets without a measurement are counted as failed runs, and say so', () => {
    const unmeasured = arm('!newest');
    expect(unmeasured).toContain('unmeasured');
    expect(unmeasured).toContain('none carries a measurement');
    expect(unmeasured).not.toContain('has not filed');
  });
});
