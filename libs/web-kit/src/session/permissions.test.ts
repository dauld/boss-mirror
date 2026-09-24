// Route visibility — the role's Class row says what a role sees; the
// SPA carries no org chart (backlog 18d6a6c9, 2026-09-17). Run via
// `bun test`.

import { describe, expect, test } from 'bun:test';
import { canSeeRoute, declaredSurfaces, DEFAULT_WORK, ROUTES, workFor, type RouteName } from './permissions';

const row = (code: string, metadata: Record<string, unknown>) => ({ code, metadata });

describe('canSeeRoute — a role with no declared surfaces sees every surface', () => {
  // Measured on prod (2026-09-17): the matrix this replaced knew 33
  // brewery roles and 26 device-shop roles, and a role it did not know
  // — every role of the company's own tenant — saw only the six
  // ungated routes. The modules are the tenant's statement of what
  // exists (missing = off since ce68f137) and policy is enforced at
  // the endpoints, so an undeclared role's sidebar is the tenant's
  // modules, not nothing.
  const surfaces: RouteName[] = [
    'workflows', 'policy', 'system-kb', 'system-design', 'system-step-plugins',
    'people', 'catalog', 'accounts', 'finance', 'exec', 'auth-admin',
  ];
  for (const r of surfaces) {
    test(`platform-admin (no surfaces declared) can see "${r}"`, () => {
      expect(canSeeRoute('platform-admin', r, row('platform-admin', { is_system_role: true }))).toBe(true);
    });
  }
  test('a role the registry has not answered for yet sees everything, not nothing', () => {
    expect(canSeeRoute('founder', 'exec', undefined)).toBe(true);
    expect(canSeeRoute('founder', 'finance', undefined)).toBe(true);
  });
  test('a row with metadata that says nothing about surfaces is the same', () => {
    expect(canSeeRoute('sales-rep', 'finance', row('sales-rep', { department: 'sales' }))).toBe(true);
  });
});

describe('canSeeRoute — a role whose Class row declares surfaces sees exactly those', () => {
  // The brewery's classes.json carries each role's former matrix entry
  // as `metadata.surfaces`, so the playground renders as before — from
  // the tenant's own data.
  const brewer = row('brewer', { surfaces: ['jobs', 'parts', 'products', 'schedule'] });
  test('a declared surface is visible', () => {
    expect(canSeeRoute('brewer', 'parts', brewer)).toBe(true);
    expect(canSeeRoute('brewer', 'schedule', brewer)).toBe(true);
  });
  test('an undeclared surface is not', () => {
    expect(canSeeRoute('brewer', 'exec', brewer)).toBe(false);
    expect(canSeeRoute('brewer', 'finance', brewer)).toBe(false);
    expect(canSeeRoute('brewer', 'policy', brewer)).toBe(false);
  });
  test('the always-on routes stay visible regardless — what they READ is policed by the endpoints', () => {
    for (const r of ['shop', 'inbox', 'workflows', 'system-experiments', 'views', 'system-feedback'] as RouteName[]) {
      expect(canSeeRoute('brewer', r, brewer)).toBe(true);
    }
  });
  test('an empty declaration is a declaration: only the always-on routes', () => {
    const none = row('bartender', { surfaces: [] });
    expect(canSeeRoute('bartender', 'calendar', none)).toBe(false);
    expect(canSeeRoute('bartender', 'inbox', none)).toBe(true);
  });
});

describe('declaredSurfaces — reads metadata.surfaces defensively', () => {
  test('absent row, absent key, or a non-list is "nothing declared"', () => {
    expect(declaredSurfaces(undefined)).toBeUndefined();
    expect(declaredSurfaces(row('x', {}))).toBeUndefined();
    expect(declaredSurfaces(row('x', { surfaces: 'jobs' }))).toBeUndefined();
    expect(declaredSurfaces(row('x', { surfaces: null }))).toBeUndefined();
  });
  test('non-string entries are dropped, not crashed on', () => {
    expect(declaredSurfaces(row('x', { surfaces: ['jobs', 3, null, 'exec'] }))).toEqual(['jobs', 'exec']);
  });
  test('ROUTES is every gated RouteName once — the vocabulary a declaration is checked against', () => {
    expect(new Set(ROUTES).size).toBe(ROUTES.length);
    expect(ROUTES).toContain('finance');
    expect(ROUTES).not.toContain('inbox');
  });
});

describe('the brewery seed declares its surfaces in the vocabulary the SPA reads', () => {
  // The matrix moved verbatim into examples/brewery/seeds/classes.json;
  // a route renamed in libs/web-kit without the seed following is a
  // role that silently loses a sidebar entry, so the seed is checked
  // against ROUTES here rather than trusted.
  test('every metadata.surfaces entry is a gated RouteName', async () => {
    const path = new URL('../../../../examples/brewery/seeds/classes.json', import.meta.url);
    const rows = (await Bun.file(path).json()) as ReadonlyArray<{
      subject_kind: string; code: string; member_attribute?: string; metadata?: Record<string, unknown>;
    }>;
    const roles = rows.filter((r) => r.subject_kind === 'employee' && r.member_attribute === 'role');
    const declared = roles.filter((r) => declaredSurfaces({ code: r.code, metadata: r.metadata ?? {} }) !== undefined);
    expect(declared.length).toBeGreaterThan(30);
    const known = new Set<string>(ROUTES);
    const bad = declared.flatMap((r) =>
      (declaredSurfaces({ code: r.code, metadata: r.metadata ?? {} }) ?? [])
        .filter((s) => !known.has(s))
        .map((s) => `${r.code}: ${s}`),
    );
    expect(bad).toEqual([]);
  });
});

describe('workFor — a role\'s Work list is its Class row\'s metadata.work', () => {
  // Until 2026-09-24 (backlog 6a3b93eb) libs/web-kit carried
  // WORK_BY_ROLE: a closed map of 66 brewery, device-shop and platform
  // role codes, and every live role of the company's own tenant fell
  // through it to the default. The lists moved verbatim onto the
  // example tenants' role rows as `metadata.work`, beside `surfaces`.
  test('a role the registry has not answered for, or whose row declares none, gets the default', () => {
    expect(workFor(undefined)).toEqual(DEFAULT_WORK);
    expect(workFor(row('platform-admin', { is_system_role: true }))).toEqual(DEFAULT_WORK);
    expect(DEFAULT_WORK).toEqual(['jobs']);
  });
  test('a declared list is the Work list, in its declared order', () => {
    expect(workFor(row('sales-rep', { work: ['sales', 'accounts'] }))).toEqual(['sales', 'accounts']);
  });
  test('an empty declaration is a declaration: no Work rows', () => {
    expect(workFor(row('content-writer', { work: [] }))).toEqual([]);
  });
  test('a non-list reads as nothing declared, never a crash', () => {
    expect(workFor(row('x', { work: 'jobs' }))).toEqual(DEFAULT_WORK);
    expect(workFor(row('x', { work: null }))).toEqual(DEFAULT_WORK);
  });
  test('an entry that is not a gated RouteName is dropped — the shell looks each one up in its catalog', () => {
    expect(workFor(row('x', { work: ['jobs', 3, 'refurb', 'qa'] }))).toEqual(['jobs', 'qa']);
  });
});

describe('the example tenants declare Work in the vocabulary the SPA reads', () => {
  // A route renamed in libs/web-kit without the seed following is a
  // role that silently loses a Work row, so each seed is checked
  // against ROUTES here rather than trusted.
  type Seeded = { subject_kind: string; code: string; member_attribute?: string; metadata?: Record<string, unknown> };
  const seeds: ReadonlyArray<[string, () => Promise<ReadonlyArray<Seeded>>]> = [
    ['brewery', async () =>
      (await Bun.file(new URL('../../../../examples/brewery/seeds/classes.json', import.meta.url)).json()) as Seeded[]],
    ['used-device-shop', async () =>
      (Bun.TOML.parse(await Bun.file(new URL('../../../../examples/used-device-shop/seeds/classes.toml', import.meta.url)).text()) as { class: Seeded[] }).class],
  ];
  for (const [tenant, load] of seeds) {
    test(`${tenant}: every metadata.work entry is a gated RouteName, and none is schedule`, async () => {
      const roles = (await load()).filter((r) => r.subject_kind === 'employee' && r.member_attribute === 'role');
      const declared = roles.filter((r) => Array.isArray(r.metadata?.['work']));
      expect(declared.length).toBeGreaterThan(20);
      const known = new Set<string>(ROUTES);
      const bad = declared.flatMap((r) =>
        (r.metadata?.['work'] as unknown[])
          .filter((s) => typeof s !== 'string' || !known.has(s))
          .map((s) => `${r.code}: ${String(s)}`),
      );
      expect(bad).toEqual([]);
      // "My schedule" is a Home row for every role, gated only by the
      // row's `surfaces` — listing it in Work showed it twice (backlog
      // c88fa303, page audit 0e4fef17).
      const twice = declared.filter((r) => (r.metadata?.['work'] as unknown[]).includes('schedule')).map((r) => r.code);
      expect(twice).toEqual([]);
    });
  }
});
