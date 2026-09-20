// THE CORRECT HALF, PINNED (design 59a776c5). What a marketing page
// says has to keep matching what the record holds, and the failure
// modes that matter here are the QUIET ones: a claim nobody registered,
// a row nobody marked, a value nobody could read, and — the one the
// chosen mechanism cannot see directly — a claim nobody marked at all.

import { describe, expect, it } from 'bun:test';

import { claimIds, type Claim } from './claims';
import {
  checkClaims,
  fingerprintOf,
  markedClaims,
  proseOf,
  refuses,
  reportLines,
  shownFor,
  type Resolved,
} from './check-claims';

const REGISTRY: ReadonlyArray<Claim> = [
  { id: 'tenant.name', asserts: 'the demo tenant is named this', reads: 'text', source: { kind: 'tree', where: 'seeds/tenant.toml', pointer: 'meta.display_name' } },
  { id: 'source.repo', asserts: 'the public source lives here', reads: 'href', source: { kind: 'tree', where: 'fixture.toml', pointer: 'repo.url' } },
];

const page = (tenant: string, extra = '') => `
  <div class="landing">
    <h1>BOSS</h1>
    <p>a live window into <strong data-claim="tenant.name">${tenant}</strong>, the demo tenant</p>
    <a data-claim="source.repo" href="https://github.com/algedonic-dev/boss">Source on GitHub</a>
    ${extra}
    <style>.landing { color: red; }</style>
  </div>`;

const read = (pairs: Record<string, Resolved>) => new Map(Object.entries(pairs));
const AGREES = read({
  'tenant.name': { kind: 'read', value: 'Example Tenant' },
  'source.repo': { kind: 'read', value: 'https://github.com/algedonic-dev/boss' },
});

describe('reading the page', () => {
  it('finds every mark, in source order', () => {
    expect(markedClaims(page('Example Tenant'))).toEqual(['tenant.name', 'source.repo']);
  });

  it('takes the claim value from the element carrying the mark', () => {
    expect(shownFor(page('Example Tenant'), 'tenant.name')).toBe('Example Tenant');
    expect(shownFor(page('Example Tenant'), 'nope')).toBeUndefined();
  });

  it('reads the prose a visitor sees, not the markup or the styles', () => {
    const prose = proseOf(page('Example Tenant'));
    expect(prose).toContain('a live window into Example Tenant');
    expect(prose).not.toContain('color: red');
    expect(prose).not.toContain('<strong');
  });
});

describe('judging the page against the record', () => {
  it('agrees when the page and the record say the same thing', () => {
    const r = checkClaims(page('Example Tenant'), AGREES, null, REGISTRY);
    expect(r.findings.every((f) => f.kind === 'agrees')).toBe(true);
    expect(r.marked).toBe(2);
    expect(refuses(r)).toBe(false);
  });

  it('REFUSES when the page drifted from the record, and names both values', () => {
    // The whole point: the tenant was renamed in the record and the
    // page still carries the old name.
    const r = checkClaims(page('Example Tenant'), read({
      'tenant.name': { kind: 'read', value: 'Example Tenant Ltd.' },
      'source.repo': { kind: 'read', value: 'https://github.com/algedonic-dev/boss' },
    }), null, REGISTRY);
    expect(refuses(r)).toBe(true);
    const line = reportLines(r).find((l) => l.startsWith('DISAGREES'));
    expect(line).toContain('the page says "Example Tenant"');
    expect(line).toContain('the record says "Example Tenant Ltd."');
  });

  it('REFUSES a claim whose row could not be read — no evidence is not a pass', () => {
    const r = checkClaims(page('Example Tenant'), read({
      'tenant.name': { kind: 'unreadable', why: 'the tenant endpoint answered 503' },
      'source.repo': { kind: 'read', value: 'https://github.com/algedonic-dev/boss' },
    }), null, REGISTRY);
    expect(refuses(r)).toBe(true);
    expect(reportLines(r).find((l) => l.startsWith('UNREADABLE'))).toContain('503');
  });

  it('names a mark no row answers — it looks checked and never was', () => {
    const r = checkClaims(page('Example Tenant', '<span data-claim="pricing.tier1">10%</span>'), AGREES, null, REGISTRY);
    expect(r.findings).toContainEqual({ kind: 'unregistered', id: 'pricing.tier1' });
    expect(reportLines(r).find((l) => l.startsWith('UNREGISTERED'))).toContain('never been checked');
  });

  it('names a row the page no longer marks — it is checking nothing', () => {
    const noRepo = '<div><strong data-claim="tenant.name">Example Tenant</strong></div>';
    const r = checkClaims(noRepo, AGREES, null, REGISTRY);
    expect(r.findings).toContainEqual({ kind: 'unused', id: 'source.repo' });
  });
});

describe('the coverage hole the mechanism leaves', () => {
  // David chose marked claims knowing an UNMARKED claim is invisible.
  // Nothing can compute the true denominator — but "the words moved and
  // the claim set did not" is decidable, and it is the prompt to mark.
  it('says nothing when the page is unchanged', () => {
    const html = page('Example Tenant');
    const first = checkClaims(html, AGREES, null, REGISTRY);
    const again = checkClaims(html, AGREES, first.fingerprint, REGISTRY);
    expect(again.proseChanged).toBe(false);
    expect(reportLines(again).some((l) => l.startsWith('PROSE CHANGED'))).toBe(false);
  });

  it('flags a page that grew an UNMARKED claim, which nothing else could catch', () => {
    const before = page('Example Tenant');
    const first = checkClaims(before, AGREES, null, REGISTRY);
    // New copy, new assertion, nobody marked it. Every marked claim
    // still agrees — so without this, the run is all-green.
    const after = page('Example Tenant', '<p>Hosting is 10% of your first $1M of spend.</p>');
    const r = checkClaims(after, AGREES, first.fingerprint, REGISTRY);
    expect(r.findings.every((f) => f.kind === 'agrees' || f.kind === 'unused')).toBe(true);
    expect(r.proseChanged).toBe(true);
    expect(reportLines(r).find((l) => l.startsWith('PROSE CHANGED'))).toContain('cannot see an unmarked claim');
  });

  it('does NOT refuse on moved prose — a prompt is not a verdict', () => {
    // Keeping these apart is what stops the check becoming the
    // permanently-red alarm nobody reads.
    const first = checkClaims(page('Example Tenant'), AGREES, null, REGISTRY);
    const r = checkClaims(page('Example Tenant', '<p>New words.</p>'), AGREES, first.fingerprint, REGISTRY);
    expect(r.proseChanged).toBe(true);
    expect(refuses(r)).toBe(false);
  });

  it('fingerprints the PROSE, so a markup-only change is not a false prompt', () => {
    const a = '<p>a live window into <strong data-claim="tenant.name">Example Tenant</strong></p>';
    const b = '<div><span>a live window into</span> <em data-claim="tenant.name">Example Tenant</em></div>';
    expect(fingerprintOf(a)).toBe(fingerprintOf(b));
  });
});

describe('which side of the element carries the claim', () => {
  // A link asserts where it GOES, not what it is labelled. Comparing
  // the copy to a route would refuse on every rewording of the CTA,
  // which is the fastest way to teach someone to ignore the check.
  const cta = '<a data-claim="cta.signin" class="x" href="/login">Sign in to operate the demo →</a>';
  const ROUTE: ReadonlyArray<Claim> = [
    { id: 'cta.signin', asserts: 'this route exists', reads: 'href', source: { kind: 'route', path: '/login' } },
  ];

  it('reads the href for a route claim, not the link text', () => {
    const r = checkClaims(cta, read({ 'cta.signin': { kind: 'read', value: '/login' } }), null, ROUTE);
    expect(refuses(r)).toBe(false);
    expect(r.findings).toContainEqual({ kind: 'agrees', id: 'cta.signin', value: '/login' });
  });

  it('still refuses when the link points somewhere the record does not have', () => {
    const moved = cta.replace('href="/login"', 'href="/signin"');
    const r = checkClaims(moved, read({ 'cta.signin': { kind: 'read', value: '/login' } }), null, ROUTE);
    expect(refuses(r)).toBe(true);
    expect(reportLines(r).find((l) => l.startsWith('DISAGREES'))).toContain('/signin');
  });

  it('rewording the CTA copy changes nothing about the claim', () => {
    const reworded = cta.replace('Sign in to operate the demo →', 'Open the demo');
    const r = checkClaims(reworded, read({ 'cta.signin': { kind: 'read', value: '/login' } }), null, ROUTE);
    expect(refuses(r)).toBe(false);
  });
});

describe('the page and the registry cannot drift apart', () => {
  // CLAUDE.md 9a: the marks live in a .svelte file and the rows live in
  // claims.ts, and nothing but this holds them equal.
  it('every mark on the real landing page has a row, and every row is marked', async () => {
    const src = await Bun.file(new URL('../landing/LandingPage.svelte', import.meta.url)).text();
    const marks = markedClaims(src);
    expect(marks.length).toBeGreaterThan(0);
    for (const id of marks) {
      expect(claimIds, `${id} is marked on the page but no row says what answers it`).toContain(id);
    }
    for (const id of claimIds) {
      expect(marks, `${id} is registered but nothing on the page carries it`).toContain(id);
    }
  });
});
