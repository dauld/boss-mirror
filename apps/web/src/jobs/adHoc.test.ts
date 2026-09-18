import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { AD_HOC_KIND, registeredAdHoc } from './adHoc';

// "Create Ad Hoc Job" on /ux/jobs, /ux/service and /ux/sales preselected
// the `ad-hoc` Workflow whether or not the registry carried one. Under a
// registry without it the click changed nothing visible — the
// interaction crawl (car f2b8a01c, KNOWN_GAPS #6) reported it on all
// three routes (backlog a399613d). The control is now rendered off the
// registry: this is the read it renders from.
describe('registeredAdHoc', () => {
  const row = (kind: string) => ({ kind, label: kind, subject_kinds: ['asset'] });

  test('finds the ad-hoc row when the registry carries it', () => {
    expect(registeredAdHoc([row('sale'), row(AD_HOC_KIND)])?.kind).toBe('ad-hoc');
  });

  test('is null when no such kind is registered, or nothing is loaded yet', () => {
    expect(registeredAdHoc([row('sale')])).toBeNull();
    expect(registeredAdHoc([])).toBeNull();
  });

  test('the jobs list renders the button off that read and preselects the registered kind', () => {
    const src = readFileSync(join(import.meta.dir, 'JobsListPage.svelte'), 'utf8').replace(/\s+/g, ' ');
    const at = src.indexOf('Create Ad Hoc Job');
    expect(at).toBeGreaterThan(-1);
    const control = src.slice(src.lastIndexOf('{#if', at), at);
    expect(control).toContain('{#if adHoc}');
    expect(control).toContain('openNewJob({ kind: adHoc.kind })');
    expect(src).not.toContain("kind: 'ad-hoc'");
  });
});
