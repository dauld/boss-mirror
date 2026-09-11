// A dead session goes to the login page — one definition of what
// that means, for every caller that learns a session is dead.
//
// Three places need the same answer, and each used to spell it out:
//   - App.svelte's 401-redirect interceptor, for /api/* reads and
//     writes (identity probes exempt — a 401 on "who am I" is the
//     ANSWER, and redirecting on it made `unauthenticated`
//     unreachable).
//   - steps/pluginHost.ts, for the `/plugins/<bundle>` preflight. The
//     interceptor never covered it, so an expired session rendered a
//     plugin-load error with a Retry button that could not succeed:
//     the surface reported a symptom while the system already held
//     the cause (David, 2026-09-10).
//   - ui/WriteGate.svelte's "sign in to act" href, for the read-only
//     guest — a link rather than a redirect, but the same target and
//     the same ?next=.
//
// The redirect target, the ?next= encoding and the on-/login guard
// are one fact, so they live here once (CLAUDE.md §9a — collapse it
// if you can; a comment asking call sites to agree is not a
// mechanism).

/** The login route, as the router spells it (`router.ts` matches the
 *  literal `/login`, and LoginPage reads `?next=` back off it). */
export const LOGIN_PATH = '/login';

/** Where a dead session should land: the login page with the page the
 *  operator was reading captured as `?next=`, so signing in returns
 *  them there. LoginPage admits in-app paths only, so the encoding
 *  here is the whole contract. Pure — takes the location rather than
 *  reading one. */
export function loginUrlFor(pathname: string, search: string): string {
  return `${LOGIN_PATH}?next=${encodeURIComponent(pathname + search)}`;
}

/** The slice of `window` a redirect needs. Declared locally so tests
 *  can pass a plain object and so nothing here depends on a DOM. */
export type NavWindow = {
  location: { pathname: string; search: string; href: string };
};

// Once-per-page latch. A dead session fails EVERY in-flight request,
// and a step page can have several plugin preflights and several API
// reads outstanding at the same moment; without this, each one
// restates the navigation. The first call captures `next` and wins.
// Module-level because a page's "I have already left" is a property
// of the page, not of any one caller; `_resetDeadSessionForTests`
// clears it the way pluginHost clears its caches.
let alreadyLeaving = false;

function currentWindow(): NavWindow | null {
  return (globalThis as { window?: NavWindow }).window ?? null;
}

/** Send this tab to the login page. Returns whether it issued the
 *  navigation, so a caller can tell "handled, stop explaining this
 *  failure" from "nothing happened, say what you know".
 *
 *  Refuses in two cases, both of which would be a loop or noise:
 *  already on /login (the page that fixes it), and already leaving. */
export function goToLogin(win: NavWindow | null = currentWindow()): boolean {
  if (!win) return false;
  if (win.location.pathname === LOGIN_PATH) return false;
  if (alreadyLeaving) return false;
  alreadyLeaving = true;
  win.location.href = loginUrlFor(win.location.pathname, win.location.search);
  return true;
}

export function _resetDeadSessionForTests(): void {
  alreadyLeaving = false;
}
