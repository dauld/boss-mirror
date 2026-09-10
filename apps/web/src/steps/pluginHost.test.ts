// The one-fetch downgrade (packet cc9d7fc6): a single failed
// /api/jobs/step-plugins fetch used to be cached as "no plugins" for
// the whole session, permanently downgrading every plugin-backed step
// to the generic surface. Failures must be reported as failures and
// must NOT be cached — the next probe retries.

import { afterEach, describe, expect, test } from 'bun:test';
import { _resetDeadSessionForTests } from '@boss/web-kit/session/deadSession';
import { _resetPluginRegistryForTests, getStepPluginMount, hasActivePluginFor, pluginLoadFailure, probeActivePlugin } from './pluginHost';

const realFetch = globalThis.fetch;
const hadWindow = 'window' in globalThis;
afterEach(() => {
  globalThis.fetch = realFetch;
  _resetPluginRegistryForTests();
  _resetDeadSessionForTests();
  if (!hadWindow) delete (globalThis as Record<string, unknown>).window;
});

type FakeWindow = { location: { pathname: string; search: string; href: string } };

function stubWindow(pathname: string, search = ''): FakeWindow {
  const win: FakeWindow = { location: { pathname, search, href: `${pathname}${search}` } };
  (globalThis as Record<string, unknown>).window = win;
  return win;
}

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
  test('a 403 bundle records the status and resolves null', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      if (calls === 1) return new Response(JSON.stringify([SPEC]), { status: 200 });
      return new Response('denied', { status: 403 });
    });
    _resetPluginRegistryForTests();

    const mount = await getStepPluginMount('review-design');
    expect(mount).toBeNull();
    expect(pluginLoadFailure('review-design')).toContain('403');
    expect(pluginLoadFailure('review-design')).toContain('/plugins/review-design.js');
  });

  test('a 404 bundle records the status and resolves null', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      if (calls === 1) return new Response(JSON.stringify([SPEC]), { status: 200 });
      return new Response('nope', { status: 404 });
    });
    _resetPluginRegistryForTests();

    expect(await getStepPluginMount('review-design')).toBeNull();
    expect(pluginLoadFailure('review-design')).toContain('404');
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

  test('a 401 bundle is a dead session: it goes to the login page, not a Retry button', async () => {
    // David, 2026-09-10, after switching computers with an expired
    // session. The interceptor in App.svelte only watched /api/*, so
    // the plugin preflight's 401 surfaced as "plugin failed to load —
    // Retry", and Retry could never succeed.
    const win = stubWindow('/jobs/abc', '?tab=steps');
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      if (calls === 1) return new Response(JSON.stringify([SPEC]), { status: 200 });
      return new Response('expired', { status: 401 });
    });
    _resetPluginRegistryForTests();

    expect(await getStepPluginMount('review-design')).toBeNull();
    expect(win.location.href).toBe('/login?next=%2Fjobs%2Fabc%3Ftab%3Dsteps');
    // And it does NOT masquerade as a broken bundle.
    expect(pluginLoadFailure('review-design')).toBeNull();
  });

  test('a 401 while already on /login does not loop — it says what it knows instead', async () => {
    const win = stubWindow('/login', '?next=%2Fjobs%2Fabc');
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      if (calls === 1) return new Response(JSON.stringify([SPEC]), { status: 200 });
      return new Response('expired', { status: 401 });
    });
    _resetPluginRegistryForTests();

    expect(await getStepPluginMount('review-design')).toBeNull();
    expect(win.location.href).toBe('/login?next=%2Fjobs%2Fabc');
    expect(pluginLoadFailure('review-design')).toContain('401');
  });

  test('a 403 does NOT redirect — authenticated-but-denied keeps its named failure', async () => {
    // Bouncing a policy denial to a login page tells the operator a
    // lie and loses the diagnosis.
    const win = stubWindow('/jobs/abc', '');
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      if (calls === 1) return new Response(JSON.stringify([SPEC]), { status: 200 });
      return new Response('denied', { status: 403 });
    });
    _resetPluginRegistryForTests();

    expect(await getStepPluginMount('review-design')).toBeNull();
    expect(win.location.href).toBe('/jobs/abc');
    expect(pluginLoadFailure('review-design')).toContain('403');
  });

  test('a network error does NOT redirect — it keeps its unreachable reason', async () => {
    const win = stubWindow('/jobs/abc', '');
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      if (calls === 1) return new Response(JSON.stringify([SPEC]), { status: 200 });
      throw new Error('network down');
    });
    _resetPluginRegistryForTests();

    expect(await getStepPluginMount('review-design')).toBeNull();
    expect(win.location.href).toBe('/jobs/abc');
    expect(pluginLoadFailure('review-design')).toContain('network down');
  });

  test('failures are not cached — a later successful preflight clears the reason', async () => {
    let calls = 0;
    stubFetch(async () => {
      calls += 1;
      if (calls === 1) return new Response(JSON.stringify([SPEC]), { status: 200 });
      if (calls === 2) return new Response('denied', { status: 403 });
      return new Response('// js', { status: 200 });
    });
    _resetPluginRegistryForTests();

    expect(await getStepPluginMount('review-design')).toBeNull();
    expect(pluginLoadFailure('review-design')).toContain('403');

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
