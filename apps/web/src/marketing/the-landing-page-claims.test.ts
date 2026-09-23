// THE CALLER (backlog e1524f57). Design 59a776c5 made every judgement
// pure and left fetching to "the caller" — and no caller was built, so
// the landing page's marks, the registry and the checker were complete
// and inert: `tenant.name` was registered and checked by nothing.
//
// This test IS the caller, and it lives here on purpose. Both declared
// source kinds are facts of the TREE — a repo file at a pointer, a path
// in this app's router — so a claim can only start to disagree when a
// commit changes the page, the registry or the file it points at, and
// every commit passes through the gate that runs this suite. A scheduled
// `check-the-claims` protocol adds nothing to that until a `registry`
// source kind exists: that one reads LIVE data, which moves without a
// commit, and it is the claim that will need a cadence.
//
// The page read is the component's source, not a rendered page: `bun
// test` has no Svelte pass, the marks are literal markup there, and the
// browser receives this markup through the bundle unchanged.

import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { parseRoute } from '../router';
import { CLAIMS } from './claims';
import { checkClaims, refuses, reportLines } from './check-claims';
import { resolveClaims, type ClaimReaders } from './resolve-claims';

const REPO = join(import.meta.dir, '..', '..', '..', '..');
const LANDING = join(import.meta.dir, '..', 'landing', 'LandingPage.svelte');

/** What the router does with a path it has no route for — asked of the
 *  router rather than spelled here, so a change to the catch-all cannot
 *  make every path look served. */
const FALLBACK = parseRoute('/no-such-route-exists-here').kind;

const READERS: ClaimReaders = {
  readTree: (where) => Bun.TOML.parse(readFileSync(join(REPO, where), 'utf8')),
  serves: (path) => path === '/' || parseRoute(path).kind !== FALLBACK,
};

describe('the landing page, checked against the tree', () => {
  const html = readFileSync(LANDING, 'utf8');
  const report = checkClaims(html, resolveClaims(CLAIMS, READERS), null);
  const lines = reportLines(report).join('\n');

  it('makes no claim the record disagrees with or cannot answer', () => {
    // The report lines ARE the failure message: a verdict must name
    // what failed, and which side — page or row — a person must fix.
    expect(refuses(report) ? lines : 'clean').toBe('clean');
  });

  it('checks every registered claim and marks nothing unregistered', () => {
    // `refuses` deliberately passes an unused row or an unregistered
    // mark (a check nobody reads is worse than a warning). HERE they
    // are drift between this page and claims.ts, so the caller holds
    // both to zero: every finding agrees, one per registered claim.
    const notAgreed = report.findings.filter((f) => f.kind !== 'agrees');
    expect(notAgreed.length === 0 ? 'all agree' : lines).toBe('all agree');
    expect(report.marked).toBe(CLAIMS.length);
  });
});

describe('the caller can fail', () => {
  // A resolver that could only ever agree would make the test above a
  // wrong green. Each leg here is a real reader pointed at a wrong row.
  const html = readFileSync(LANDING, 'utf8');

  it('refuses when the tree names a different tenant', () => {
    const moved = CLAIMS.map((c) =>
      c.id === 'tenant.name'
        ? { ...c, source: { kind: 'tree' as const, where: 'infra/estate/estate.toml', pointer: 'mirror_url' } }
        : c,
    );
    const r = checkClaims(html, resolveClaims(moved, READERS), null, moved);
    expect(refuses(r)).toBe(true);
    expect(r.findings.find((f) => f.id === 'tenant.name')?.kind).toBe('disagrees');
  });

  it('refuses when the file a claim points at is gone', () => {
    const gone = CLAIMS.map((c) =>
      c.id === 'source.repo'
        ? { ...c, source: { kind: 'tree' as const, where: 'infra/estate/no-such-file.toml', pointer: 'mirror_url' } }
        : c,
    );
    const r = checkClaims(html, resolveClaims(gone, READERS), null, gone);
    expect(refuses(r)).toBe(true);
    expect(r.findings.find((f) => f.id === 'source.repo')?.kind).toBe('unreadable');
  });

  it('refuses when the route the page links is not served', () => {
    // Same registry, same page — only the router stops serving the
    // path, which is the drift a deleted route would be.
    const r = checkClaims(html, resolveClaims(CLAIMS, { ...READERS, serves: () => false }), null);
    expect(refuses(r)).toBe(true);
    expect(r.findings.find((f) => f.id === 'cta.signin')?.kind).toBe('disagrees');
  });
});
