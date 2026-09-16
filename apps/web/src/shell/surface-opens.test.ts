// The SPA half of the surface-open record (backlog 628f182b): the
// pattern is the router's vocabulary with ids replaced, the recorder
// posts once per change of pattern, and a failed post is nobody's
// problem.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { QUERY_FIELDS, makeSurfaceOpenRecorder, routePattern, type SurfaceOpen } from './surface-opens';
import { parseRoute } from '../router';

describe('routePattern', () => {
  test('a catalogued surface is its own path', () => {
    expect(routePattern({ kind: 'systemCodebase' }, '/it/codebase')).toBe('/it/codebase');
    expect(routePattern({ kind: 'systemYard' }, '/it')).toBe('/it');
    expect(routePattern({ kind: 'me' }, '/')).toBe('/');
  });

  test('the /dashboard mount and a trailing slash are not part of the pattern', () => {
    expect(routePattern({ kind: 'systemCodebase' }, '/dashboard/it/codebase/')).toBe('/it/codebase');
  });

  test('a detail page carries the field name, never the id', () => {
    const uuid = '0123abcd-4567-89ef-0123-456789abcdef';
    expect(routePattern({ kind: 'jobDetail', jobId: uuid }, `/ux/jobs/${uuid}`)).toBe('/ux/jobs/:jobId');
    expect(routePattern({ kind: 'account', accountId: 'account-00001' }, '/ux/accounts/account-00001')).toBe(
      '/ux/accounts/:accountId',
    );
    expect(
      routePattern({ kind: 'workflowDetail', kindSlug: 'seasonal-release' }, '/it/registry/seasonal-release'),
    ).toBe('/it/registry/:kindSlug');
  });

  test('two ids on one path are both replaced', () => {
    expect(
      routePattern({ kind: 'stepFocus', jobId: 'job-1', stepId: 'step-9' }, '/ux/jobs/job-1/steps/step-9'),
    ).toBe('/ux/jobs/:jobId/steps/:stepId');
  });

  test('an encoded id in the pathname still matches its decoded field', () => {
    expect(routePattern({ kind: 'part', partSku: 'HOP 01' }, '/ux/parts/HOP%2001')).toBe('/ux/parts/:partSku');
  });

  test('a query-string field never rewrites a static segment', () => {
    // `fromLabel: 'jobs'` is free text that happens to equal a segment.
    expect(
      routePattern(
        { kind: 'stepFocus', jobId: 'j', stepId: 's', from: '/it', fromLabel: 'jobs' },
        '/ux/jobs/j/steps/s',
      ),
    ).toBe('/ux/jobs/:jobId/steps/:stepId');
    expect(routePattern({ kind: 'search', q: 'search' }, '/ux/search')).toBe('/ux/search');
    expect(routePattern({ kind: 'jobs', workflow: 'jobs' }, '/ux/jobs')).toBe('/ux/jobs');
  });

  test('an id that is a substring of a static segment is left alone', () => {
    expect(routePattern({ kind: 'asset', assetId: 'asset' }, '/ux/assets/asset')).toBe('/ux/assets/:assetId');
  });

  test('the router and the pattern agree on every parsed route (CLAUDE.md 9a)', () => {
    // Round trip through the real parser: the pattern of a parsed
    // route never carries the id the parser extracted.
    for (const [path, expected] of [
      ['/ux/jobs/abc', '/ux/jobs/:jobId'],
      ['/ux/vendors/acme%20co', '/ux/vendors/:vendorLookup'],
      ['/ux/people/emp-032', '/ux/people/:empId'],
      ['/ux/finance/inv-1', '/ux/finance/:invoiceId'],
      ['/ux/purchase-orders/po-7', '/ux/purchase-orders/:poId'],
      ['/it/registry/rules/some-rule', '/it/registry/rules/:ruleName'],
      ['/it/registry/step-plugins/review-design', '/it/registry/step-plugins/:pluginSlug'],
      ['/ux/manual/intro', '/ux/manual/:slug'],
      ['/it/operate/audit', '/it/operate/audit'],
    ] as const) {
      expect(routePattern(parseRoute(path), path)).toBe(expected);
    }
  });
});

describe('QUERY_FIELDS', () => {
  test('names every field router.ts reads off the query string', () => {
    // Every `(r as { NAME?: string }).NAME = …` assignment in router.ts is a
    // query-string field, plus search's `q`. A new one that is not in
    // QUERY_FIELDS would be treated as a path id, so this names it.
    const src = readFileSync(new URL('../router.ts', import.meta.url).pathname, 'utf8');
    const fromSource = new Set<string>(['q']);
    for (const m of src.matchAll(/\(r as \{ (\w+)\?: string \}\)/g)) fromSource.add(m[1]!);
    expect([...fromSource].sort()).toEqual([...QUERY_FIELDS].sort());
  });
});

describe('makeSurfaceOpenRecorder', () => {
  test('posts once per change of pattern, with the client clock', async () => {
    const posted: SurfaceOpen[] = [];
    const clock = new Date('2026-09-16T12:00:00Z');
    const record = makeSurfaceOpenRecorder(
      async (o) => {
        posted.push(o);
      },
      () => clock,
    );
    record('/it');
    record('/it'); // the same surface re-parsed: not a second open
    record('/it/codebase');
    record('/it'); // away and back: a new open
    expect(posted).toEqual([
      { route: '/it', at: '2026-09-16T12:00:00.000Z' },
      { route: '/it/codebase', at: '2026-09-16T12:00:00.000Z' },
      { route: '/it', at: '2026-09-16T12:00:00.000Z' },
    ]);
  });

  test('a failed post is swallowed and the next navigation still records', async () => {
    let calls = 0;
    const record = makeSurfaceOpenRecorder(async () => {
      calls += 1;
      throw new Error('502 upstream unavailable');
    });
    expect(() => record('/it')).not.toThrow();
    expect(() => record('/it/codebase')).not.toThrow();
    await Promise.resolve();
    expect(calls).toBe(2);
  });
});
