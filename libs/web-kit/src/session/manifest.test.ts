// The first paint knows the tenant (5578e42d). Run via `bun test`.

import { describe, expect, test } from 'bun:test';
import {
  documentDescriptionFor,
  documentTitleFor,
  manifestFromInline,
  moduleOn,
  readyFrom,
} from './manifest-inline';

describe('manifestFromInline — the document-carried manifest is ready before first paint', () => {
  test('a manifest object becomes a ready state with its modules and labels', () => {
    const state = manifestFromInline({
      display_name: 'Algedonic Ales',
      tenant_id: 'brewery',
      modules: { hr: false, assets: true },
      labels: { 'assets.entity_singular': 'vessel' },
    });
    expect(state?.kind).toBe('ready');
    if (state?.kind !== 'ready') return;
    expect(state.displayName).toBe('Algedonic Ales');
    expect(state.modules.hr).toBe(false);
    expect(state.labels['assets.entity_singular']).toBe('vessel');
  });

  test('a manifest with no modules or labels is still ready, with empty maps', () => {
    const state = manifestFromInline({});
    expect(state?.kind).toBe('ready');
    if (state?.kind !== 'ready') return;
    expect(Object.keys(state.modules)).toHaveLength(0);
    expect(Object.keys(state.labels)).toHaveLength(0);
  });

  test('anything that is not a manifest object leaves the fetch path in charge', () => {
    expect(manifestFromInline(undefined)).toBeNull();
    expect(manifestFromInline(null)).toBeNull();
    expect(manifestFromInline('{"modules":{}}')).toBeNull();
    expect(manifestFromInline([1, 2])).toBeNull();
    expect(manifestFromInline({ modules: 'hr' })).toBeNull();
    expect(manifestFromInline({ labels: null })).toBeNull();
  });
});

// A module is ON only when the tenant lists it true (ce68f137). The
// previous rule hid a module only on an explicit `false`, so a tenant
// whose `[modules]` was empty — Algedonic's, measured on prod
// 2026-09-17 — got every surface the playground tenant had built.
describe('moduleOn — a module is on only when the tenant lists it true', () => {
  const ready = readyFrom({ modules: { shop: true, sim: false } });

  test('listed true is on', () => {
    expect(moduleOn(ready, 'shop')).toBe(true);
  });

  test('listed false is off', () => {
    expect(moduleOn(ready, 'sim')).toBe(false);
  });

  test('missing is off', () => {
    expect(moduleOn(ready, 'shipping')).toBe(false);
    expect(moduleOn(readyFrom({}), 'shop')).toBe(false);
  });

  test('a manifest that is still loading or unreachable hides nothing', () => {
    // A deployment fault, not a tenant decision: blanking the shell
    // would hide the fault rather than show it.
    expect(moduleOn({ kind: 'loading' }, 'shop')).toBe(true);
    expect(moduleOn({ kind: 'error' }, 'shop')).toBe(true);
  });
});

// The document reads the tenant, not the playground (ce68f137): the
// served shell carried <title>Algedonic Ales</title> on prod.
describe('documentTitleFor — the tab title is the tenant name', () => {
  test('a named tenant titles the document', () => {
    expect(documentTitleFor(readyFrom({ display_name: 'Algedonic, LLC' }))).toBe(
      'Algedonic, LLC',
    );
  });

  test('an unnamed tenant, or no manifest yet, reads BOSS', () => {
    expect(documentTitleFor(readyFrom({}))).toBe('BOSS');
    expect(documentTitleFor({ kind: 'loading' })).toBe('BOSS');
    expect(documentTitleFor({ kind: 'error' })).toBe('BOSS');
  });

  test('the description names the tenant and the product, never the playground', () => {
    const named = documentDescriptionFor(readyFrom({ display_name: 'Algedonic, LLC' }));
    expect(named).toContain('Algedonic, LLC');
    expect(named).toContain('BOSS');
    expect(documentDescriptionFor({ kind: 'loading' })).not.toContain('Ales');
  });
});
