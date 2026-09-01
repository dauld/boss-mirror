// The one-fetch downgrade (packet cc9d7fc6): a single failed
// /api/jobs/step-plugins fetch used to be cached as "no plugins" for
// the whole session, permanently downgrading every plugin-backed step
// to the generic surface. Failures must be reported as failures and
// must NOT be cached — the next probe retries.

import { afterEach, describe, expect, test } from 'bun:test';
import { _resetPluginRegistryForTests, getStepPluginMount, hasActivePluginFor, pluginLoadFailure, probeActivePlugin } from './pluginHost';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
  _resetPluginRegistryForTests();
});

function stubFetch(fn: () => Promise<Response>) {
  globalThis.fetch = fn as unknown as typeof fetch;
}

const SPEC = {
  kind: 'review-design',
  frontend_url: 'review-design.js',
};

describe('probeActivePlugin', () => {
  test('reports failure distinctly from "no plugin registered"', async () => {
    stubFetch(async () => new Response('nope', { status: 500 }));
    _resetPluginRegistryForTests();
    const probe = await probeActivePlugin('review-design');
    expect(probe.kind).toBe('failed');
  });

  test('a failed registry fetch is not cached — the next probe retries', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      if (calls === 1) return new Response('down', { status: 503 });
      return new Response(JSON.stringify([SPEC]), { status: 200 });
    });
    _resetPluginRegistryForTests();

    const first = await probeActivePlugin('review-design');
    expect(first.kind).toBe('failed');

    const second = await probeActivePlugin('review-design');
    expect(second).toEqual({ kind: 'ok', active: true });
    expect(calls).toBe(2);
  });

  test('a successful load IS cached — later probes cost no fetch', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      return new Response(JSON.stringify([SPEC]), { status: 200 });
    });
    _resetPluginRegistryForTests();

    expect(await probeActivePlugin('review-design')).toEqual({ kind: 'ok', active: true });
    expect(await probeActivePlugin('other-kind')).toEqual({ kind: 'ok', active: false });
    expect(calls).toBe(1);
  });
});

describe('hasActivePluginFor', () => {
  test('still answers a plain boolean for callers that only branch', async () => {
    stubFetch(async () => new Response(JSON.stringify([SPEC]), { status: 200 }));
    _resetPluginRegistryForTests();
    expect(await hasActivePluginFor('review-design')).toBe(true);
    expect(await hasActivePluginFor('unknown')).toBe(false);
  });
});

describe('bundle load failure is recorded, not silent', () => {
  // ff87f782: decision steps fell back to the generic surface with no
  // trace — the <script> tag reports only "error", so a 401 at the
  // gateway, a CF redirect and a missing file were indistinguishable.
  // The preflight fetch learns the status and pluginLoadFailure()
  // says it, so a surface can tell "no plugin registered" from "the
  // registered bundle failed to load".
  test('a 401 bundle records the status and resolves null', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      if (calls === 1) return new Response(JSON.stringify([SPEC]), { status: 200 });
      return new Response('denied', { status: 401 });
    });
    _resetPluginRegistryForTests();

    const mount = await getStepPluginMount('review-design');
    expect(mount).toBeNull();
    expect(pluginLoadFailure('review-design')).toContain('401');
    expect(pluginLoadFailure('review-design')).toContain('/plugins/review-design.js');
  });

  test('an unreachable bundle records the reason and resolves null', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      if (calls === 1) return new Response(JSON.stringify([SPEC]), { status: 200 });
      throw new Error('network down');
    });
    _resetPluginRegistryForTests();

    const mount = await getStepPluginMount('review-design');
    expect(mount).toBeNull();
    expect(pluginLoadFailure('review-design')).toContain('network down');
  });

  test('a kind with no registered plugin records NO failure — that is the missing case', async () => {
    stubFetch(async () => new Response(JSON.stringify([SPEC]), { status: 200 }));
    _resetPluginRegistryForTests();

    const mount = await getStepPluginMount('unregistered-kind');
    expect(mount).toBeNull();
    expect(pluginLoadFailure('unregistered-kind')).toBeNull();
  });

  test('failures are not cached — a later successful preflight clears the reason', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      if (calls === 1) return new Response(JSON.stringify([SPEC]), { status: 200 });
      if (calls === 2) return new Response('denied', { status: 401 });
      return new Response('// js', { status: 200 });
    });
    _resetPluginRegistryForTests();

    expect(await getStepPluginMount('review-design')).toBeNull();
    expect(pluginLoadFailure('review-design')).toContain('401');

    // Second attempt: preflight passes, the reason clears, and the
    // load proceeds to script injection (not awaited here — there is
    // no DOM in this runner; the cleared reason is the contract).
    const doc = {
      createElement: () => ({ set src(_v: string) {}, async: false, onerror: null }),
      head: { appendChild: () => {} },
    };
    (globalThis as Record<string, unknown>).document = doc;
    try {
      void getStepPluginMount('review-design');
      await new Promise((r) => setTimeout(r, 10));
      expect(pluginLoadFailure('review-design')).toBeNull();
    } finally {
      delete (globalThis as Record<string, unknown>).document;
    }
  });
});
