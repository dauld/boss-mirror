// A dead session goes to the login page — from ANY fetch, not only
// /api/*.
//
// The 401-redirect interceptor in App.svelte only ever watched
// /api/*, and said so. The non-API fetch that matters is a step
// plugin bundle: `/plugins/<frontend_url>` preflighted in
// pluginHost.loadPlugin. With an expired session that preflight came
// back 401 and the operator got a plugin-load error with a Retry
// button that could never succeed — a surface reporting a symptom
// while the system already held the cause.
//
// These tests pin the ONE definition both call sites now share: the
// redirect target, the ?next= encoding, the on-/login guard, and the
// once-per-page latch that keeps a dead session from restating the
// navigation for every concurrent 401.

import { afterEach, describe, expect, test } from 'bun:test';
import {
  LOGIN_PATH,
  _resetDeadSessionForTests,
  goToLogin,
  loginUrlFor,
} from './deadSession';

afterEach(() => {
  _resetDeadSessionForTests();
});

function fakeWindow(pathname: string, search = '') {
  return { location: { pathname, search, href: `${pathname}${search}` } };
}

describe('loginUrlFor', () => {
  test('captures path + search as an encoded ?next=', () => {
    expect(loginUrlFor('/ux/jobs', '?status=open')).toBe(
      '/login?next=%2Fux%2Fjobs%3Fstatus%3Dopen',
    );
  });

  test('a bare path needs no search', () => {
    expect(loginUrlFor('/me', '')).toBe('/login?next=%2Fme');
  });

  test('the target is the login route the router knows', () => {
    expect(LOGIN_PATH).toBe('/login');
    expect(loginUrlFor('/me', '').startsWith(`${LOGIN_PATH}?`)).toBe(true);
  });
});

describe('goToLogin', () => {
  test('navigates to the login page with the current page as ?next=', () => {
    const win = fakeWindow('/jobs/abc', '?tab=steps');
    expect(goToLogin(win)).toBe(true);
    expect(win.location.href).toBe('/login?next=%2Fjobs%2Fabc%3Ftab%3Dsteps');
  });

  test('does NOT redirect when already on /login — that is the loop', () => {
    const win = fakeWindow('/login', '?next=%2Fme');
    expect(goToLogin(win)).toBe(false);
    expect(win.location.href).toBe('/login?next=%2Fme');
  });

  test('redirects once per page: a storm of 401s issues one navigation', () => {
    const win = fakeWindow('/jobs/abc', '');
    expect(goToLogin(win)).toBe(true);
    const after = win.location.href;
    expect(goToLogin(win)).toBe(false);
    expect(goToLogin(win)).toBe(false);
    expect(win.location.href).toBe(after);
  });

  test('with no window to navigate it reports that it did not', () => {
    expect(goToLogin(null)).toBe(false);
  });
});
