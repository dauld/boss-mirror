// The first paint knows the tenant (5578e42d). Run via `bun test`.

import { describe, expect, test } from 'bun:test';
import { manifestFromInline } from './manifest-inline';

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
