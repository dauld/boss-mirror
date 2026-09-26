// Current-user session.
//
// One source: the gateway's `/api/session`, resolved to an Employee
// row. No session means unauthenticated — there is no fallback that
// invents an identity, because the fallback that used to be here is
// what let the chrome disagree with the server about who you were.
//
// `setPersona` survives for `bun run dev`, where there is no gateway
// to issue a session. The dev-server reads the cookie it writes and
// synthesises `x-boss-user` from it. The gateway ignores it entirely.
// (The live smoke suite that was its other reader was deleted
// 2026-09-18, design 0e07ce64; the nightly playground crawl that
// replaced it uses the gateway's guest session, not a persona.)

/// Name of the cookie that tells the dev-server / gateway which
/// persona the user is currently viewing as (demo mode only). The
/// dev-server looks this id up and synthesises
/// x-boss-user from the matched employee's id + role + department
/// so backend policy scoping reflects the selected persona.
///
/// In a real (non-demo) deployment this cookie is ignored —
/// personas are a demo affordance.
const PERSONA_COOKIE = 'boss-persona';

function writePersonaCookie(id: string): void {
  // 30-day cookie scoped to the whole app. `SameSite=Lax` is
  // enough for same-origin fetches; no Secure because the dev
  // server is http.
  document.cookie = `${PERSONA_COOKIE}=${encodeURIComponent(id)}; path=/; max-age=2592000; SameSite=Lax`;
}


// The pure half lives in ./classify so it can be tested without the
// Svelte compiler; re-exported here so existing importers are
// unaffected.
import type { Certification, Employee, SessionState, SessionEnvelope, ProbeBody, ViewerRead } from './classify';
import { guestEmployee, classifyProbe, readPeopleRow } from './classify';
export type { Certification, Employee, SessionState, SessionEnvelope, ProbeBody, ViewerRead };
export { guestEmployee, classifyProbe, readPeopleRow };
export { BREAK_GLASS_ROLE, breakGlassOperator } from './classify';

export const session = $state<SessionEnvelope>({
  value: { kind: 'loading' },
  fromGateway: false,
  readonly: false,
});

/// The honest synthetic identity for a read-only visitor. The old
/// demo-mode sin was dressing a visitor in a REAL employee's name,
/// role and department; the fix is not to strip the visitor of a
/// renderable identity, it is to give them their own: named Guest,
/// carrying the read-only role they actually hold, colliding
/// with no roster id, assignable to nothing.
export async function loadSession(): Promise<void> {
  // The gateway session probe, then the ONE people row it names. The
  // shell used to fetch the whole /api/people roster first, to look
  // this one row up, and a failed roster read was swallowed: every page
  // that names people read the roster a second time, and a signed-in
  // operator whose roster read blinked rendered as an unrecognized
  // login (backlog b4f68a65, 2026-09-26). A failed row read is now its
  // own state, `unresolved`, which the chrome renders as a failed read.
  try {
    const r = await fetch('/api/session', { credentials: 'same-origin' });
    if (r.ok) {
      const body = (await r.json()) as ProbeBody;
      const viewer: ViewerRead = body.employee_id
        ? await readPeopleRow(body.employee_id)
        : { kind: 'absent' };
      const classified = classifyProbe(body, viewer);
      if (classified) {
        session.fromGateway = true;
        session.readonly = classified.readonly;
        session.value = classified.value;
        return;
      }
    }
  } catch {
    // Network failure on the probe → no session.
  }

  // No session, no user. There used to be a demo-mode fallback here
  // that rendered a persona from localStorage — or, failing that, the
  // CEO — for anyone the gateway could not resolve. It is why a
  // read-only visitor saw an executive's name in the chrome while
  // every write returned 403, and it is the last piece of the mode
  // that made "who am I" a different question from "who does the
  // server think I am".
  session.value = { kind: 'unauthenticated' };
}

/// Switch the viewed-as persona. The cookie is the ONLY thing that
/// persists: the dev-server reads it off every proxied request to
/// synthesise `x-boss-user`, so after a reload the SPA learns the
/// persona back from `/api/session` — the server's answer, not a
/// copy the client kept. There used to be a `boss.persona.empId`
/// localStorage write beside this; its reader died with the demo
/// fallback and it was never restored, because a client-side
/// identity that can disagree with the server's is the exact defect
/// the fallback caused.
///
/// The chosen persona is read as its own people row — there is no
/// preloaded roster to search any more (backlog b4f68a65). An id the
/// people service does not hold, or a read that fails, changes nothing.
export async function setPersona(
  id: string,
  fetchFn?: Parameters<typeof readPeopleRow>[1],
): Promise<void> {
  // Write the cookie so the dev-server + gateway can synthesise the
  // right x-boss-user header on API requests. Without this the
  // backend still saw the default (emp-001 CEO) and returned
  // unscoped data.
  try {
    writePersonaCookie(id);
  } catch {
    // document.cookie unavailable (SSR / non-browser) — safe to
    // skip; the UI still updates correctly.
  }
  const read = await readPeopleRow(id, fetchFn);
  if (read.kind === 'found') {
    const emp = read.employee;
    session.fromGateway = false;
    // A persona switch is a full identity change. `readonly` belongs
    // to the guest identity, not to the tab — leaving it set kept a
    // once-guest session rendering GuestHome and inert write surfaces
    // after it became a real operator.
    session.readonly = false;
    session.value = { kind: 'ready', user: emp };
  }
}
