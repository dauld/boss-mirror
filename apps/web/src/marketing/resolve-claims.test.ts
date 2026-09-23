// THE RESOLVER, PINNED (backlog e1524f57, the other half of design
// 59a776c5). `checkClaims` judges a page against resolved values; until
// this file nothing resolved one, so the judgement ran only in its own
// tests. Each source kind resolves to a value or to a reason it could
// not — never to a silent pass.

import { describe, expect, it } from 'bun:test';

import type { Claim } from './claims';
import { resolveClaim, resolveClaims, valueAt, type ClaimReaders } from './resolve-claims';

const TREE: Record<string, unknown> = {
  'seeds/tenant.toml': { meta: { display_name: 'Example Tenant', founded: 1972 } },
  'estate.toml': { mirror_url: 'https://example.test/owner/repo' },
};

const READERS: ClaimReaders = {
  readTree: (where) => {
    const doc = TREE[where];
    if (doc === undefined) throw new Error(`ENOENT: no such file, open '${where}'`);
    return doc;
  },
  serves: (path) => path === '/login',
};

const tree = (id: string, where: string, pointer: string): Claim => ({
  id,
  asserts: 'x',
  reads: 'text',
  source: { kind: 'tree', where, pointer },
});
const route = (id: string, path: string): Claim => ({
  id,
  asserts: 'x',
  reads: 'href',
  source: { kind: 'route', path },
});

describe('a tree claim reads the field its pointer names', () => {
  it('walks a dotted pointer into the parsed file', () => {
    expect(resolveClaim(tree('t', 'seeds/tenant.toml', 'meta.display_name'), READERS)).toEqual({
      kind: 'read',
      value: 'Example Tenant',
    });
    expect(resolveClaim(tree('m', 'estate.toml', 'mirror_url'), READERS)).toEqual({
      kind: 'read',
      value: 'https://example.test/owner/repo',
    });
  });

  it('is unreadable, naming the file, when the file cannot be read', () => {
    const r = resolveClaim(tree('t', 'gone.toml', 'meta.display_name'), READERS);
    expect(r.kind).toBe('unreadable');
    if (r.kind === 'unreadable') expect(r.why).toContain('gone.toml');
  });

  it('is unreadable, naming the pointer, when the field is absent', () => {
    const r = resolveClaim(tree('t', 'seeds/tenant.toml', 'meta.legal_name'), READERS);
    expect(r.kind).toBe('unreadable');
    if (r.kind === 'unreadable') {
      expect(r.why).toContain('meta.legal_name');
      expect(r.why).toContain('seeds/tenant.toml');
    }
  });

  it('is unreadable when the field is not a string, rather than coercing it', () => {
    // A number or a table stringified would "agree" with whatever the
    // page happens to print for it — a guess, not a reading.
    expect(resolveClaim(tree('t', 'seeds/tenant.toml', 'meta.founded'), READERS).kind).toBe('unreadable');
    expect(resolveClaim(tree('t', 'seeds/tenant.toml', 'meta'), READERS).kind).toBe('unreadable');
  });
});

describe('a route claim asks whether this app serves the path', () => {
  it('reads back the path when the app serves it', () => {
    expect(resolveClaim(route('c', '/login'), READERS)).toEqual({ kind: 'read', value: '/login' });
  });

  it('answers a value the page cannot match when the app does not serve it', () => {
    // A route that falls to the catch-all was READ — the record says
    // "not served" — so it is a disagreement, not an unreadable row.
    const r = resolveClaim(route('c', '/signin'), READERS);
    expect(r.kind).toBe('read');
    if (r.kind === 'read') {
      expect(r.value).not.toBe('/signin');
      expect(r.value).toContain('/signin');
    }
  });
});

describe('resolving a registry', () => {
  it('answers every claim, keyed by id', () => {
    const got = resolveClaims([tree('a', 'estate.toml', 'mirror_url'), route('b', '/nope')], READERS);
    expect([...got.keys()]).toEqual(['a', 'b']);
  });
});

describe('valueAt', () => {
  it('descends only through tables', () => {
    expect(valueAt({ a: { b: 'x' } }, 'a.b')).toBe('x');
    expect(valueAt({ a: 'x' }, 'a.b')).toBeUndefined();
    expect(valueAt({ a: ['x'] }, 'a.0')).toBeUndefined();
  });
});
