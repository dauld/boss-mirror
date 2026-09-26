// Pure session logic, deliberately free of runes.
//
// classifyProbe and guestEmployee are ordinary functions: given a
// gateway probe body and the read of the viewer's own people row, they
// decide who you are. They used
// to live in session.svelte.ts beside `export const session =
// $state(...)`, and a module-level rune makes the whole module
// unloadable outside the Svelte compiler - so the seven test files in
// this package could not be run by `bun test` at all, and were wired
// into no CI job as a result. Splitting the pure half out is what
// makes them testable; session.svelte.ts re-exports everything here
// so its 26 importers are untouched.

export type Certification = {
  name: string;
  issuing_body: string;
  issued_on: string;
  expires_on: string | null;
};

export type Employee = {
  id: string;
  name: string;
  email: string;
  role: string;
  department: string;
  hire_date: string;
  status: string;
  location: string;
  employment_type: string;
  skill_level?: number | null;
  skills: string[];
  certifications: Certification[];
  manager_id?: string | null;
};

export type SessionState =
  | { kind: 'loading' }
  | { kind: 'ready'; user: Employee }
  | { kind: 'unauthenticated' }
  | { kind: 'unrecognized'; username: string }
  /// Signed in as an employee whose people row could not be READ — a
  /// refusal or a network error, not an answer. Distinct from
  /// `unrecognized` (the people service answered: no such employee),
  /// because a blink of /api/people used to render a real operator as
  /// an unrecognized login with nothing on screen saying a read had
  /// failed (backlog b4f68a65). `error` names the read, in the
  /// `<url>: HTTP <status>` shape every page's failed-read line uses.
  | { kind: 'unresolved'; username: string; error: string };

export type SessionEnvelope = {
  value: SessionState;
  fromGateway: boolean;
  /// True for the read-only guest: every read surface renders,
  /// and surfaces that offer writes may hide or soften them.
  readonly: boolean;
};


/// The roles an anonymous guest session can carry — the read-only floor,
/// `boss_core::roles::READ_ONLY_FLOOR_ROLES`, held equal to it by
/// crates/core/boss-testing/tests/the_web_guest_roles_are_the_read_only_floor.rs.
/// `visitor` is the OSS basic guest; `audit-readonly` the system-audit
/// read an instance opts in to (design 2830b6b7, 2026-09-25). Keep the
/// declaration on ONE line: the pin reads its quoted literals.
export const READ_ONLY_GUEST_ROLES: ReadonlyArray<string> = ['audit-readonly', 'visitor'];

export function guestEmployee(username: string, role: string = 'audit-readonly'): Employee {
  return {
    id: username,
    name: 'Guest',
    email: username,
    role,
    department: 'visitor',
    hire_date: new Date().toISOString().slice(0, 10),
    status: 'active',
    location: '—',
    employment_type: 'guest',
    skills: [],
    certifications: [],
  };
}

/// The role a break-glass session carries. Mirrors
/// `boss_core::roles::BREAK_GLASS_ROLE` — the same string the gateway
/// signs into the cookie in `break_glass::mint_session`.
export const BREAK_GLASS_ROLE = 'break-glass';

/// The renderable identity of a break-glass session.
///
/// A break-glass session is deliberately NOT an employee (Q4 of
/// docs/design/break-glass-is-a-key-you-hold.md): the door mints a
/// narrow role with no employee id, because resolving one would make
/// the emergency path depend on boss-people being up. It still has an
/// identity — a hardware key someone is holding — so the chrome gets
/// one to render, the same way the guest does. It is not read-only:
/// the role exists in order to act (deploy rollback, merge approval,
/// auth administration), and claiming otherwise would make the chrome
/// disagree with what the server grants.
export function breakGlassOperator(username: string): Employee {
  return {
    id: username,
    name: 'Break-glass operator',
    email: username,
    role: BREAK_GLASS_ROLE,
    department: 'emergency',
    hire_date: '',
    status: 'active',
    location: '—',
    employment_type: 'break-glass',
    skills: [],
    certifications: [],
  };
}

export type ProbeBody = {
  username?: string;
  employee_id?: string;
  role?: string;
};

/// What reading the viewer's own people row answered. `absent` is an
/// ANSWER — no employee id to ask about, or the people service said
/// 404 "no employee with ID" — and `failed` is the absence of one.
export type ViewerRead =
  | { kind: 'found'; employee: Employee }
  | { kind: 'absent' }
  | { kind: 'failed'; error: string };

type Fetch = (url: string) => Promise<Pick<Response, 'ok' | 'status' | 'json'>>;

function isEmployeeRow(body: unknown, id: string): body is Employee {
  return typeof body === 'object' && body !== null && !Array.isArray(body)
    && (body as { id?: unknown }).id === id;
}

/// Read ONE people row — the viewer's, or a persona's. The shell used
/// to fetch the whole /api/people roster on every load to find this one
/// row, and dropped a failed read on the floor (backlog b4f68a65,
/// measured 2026-09-26): every page that names people read the roster
/// again, and a failed shell read left an empty map that classified a
/// signed-in operator as `unrecognized`. One row, and a failure that
/// says so. The error text is the `<url>: HTTP <status>` /
/// `<url>: <message>` shape of apps/web's readState, so the line the
/// chrome renders reads like every other failed people read.
export async function readPeopleRow(
  id: string,
  fetchFn: Fetch = (url) => fetch(url),
): Promise<ViewerRead> {
  const url = `/api/people/${encodeURIComponent(id)}`;
  try {
    const resp = await fetchFn(url);
    if (resp.status === 404) return { kind: 'absent' };
    if (!resp.ok) return { kind: 'failed', error: `${url}: HTTP ${resp.status}` };
    const body = (await resp.json()) as unknown;
    // A 200 that is not a row — a proxy's `[]`, an error envelope — is
    // not the viewer, and rendering it as one crashes the chrome.
    if (!isEmployeeRow(body, id)) return { kind: 'failed', error: `${url}: not an employee row` };
    return { kind: 'found', employee: body };
  } catch (e) {
    return { kind: 'failed', error: `${url}: ${e instanceof Error ? e.message : String(e)}` };
  }
}

/// Pure classification of the gateway probe — extracted so the
/// guest/unrecognized boundary is a tested decision, not a branch
/// buried in a fetch handler.
export function classifyProbe(
  body: ProbeBody,
  viewer: ViewerRead,
): { value: SessionState; readonly: boolean } | null {
  const username = body.username ?? '';
  if (viewer.kind === 'found') {
    return { value: { kind: 'ready', user: viewer.employee }, readonly: false };
  }
  // The row could not be read, so who this is is UNKNOWN — not
  // unknown-to-the-people-service. Saying `unrecognized` here is the
  // defect b4f68a65 names.
  if (viewer.kind === 'failed') {
    return {
      value: { kind: 'unresolved', username: username || (body.employee_id ?? ''), error: viewer.error },
      readonly: false,
    };
  }
  // A session with no employee and a read-only-floor role is the
  // guest — a first-class read-only persona, not a broken login.
  if (username && body.role && READ_ONLY_GUEST_ROLES.includes(body.role)) {
    return {
      value: { kind: 'ready', user: guestEmployee(username, body.role) },
      readonly: true,
    };
  }
  // A session with no employee and the break-glass role is the
  // emergency operator — a session that is a KEY, not a person. It
  // used to fall through to `unrecognized`, so the one door that only
  // opens when everything else has failed answered with an error the
  // moment it worked (packet 2ef7726b).
  if (username && body.role === BREAK_GLASS_ROLE) {
    return {
      value: { kind: 'ready', user: breakGlassOperator(username) },
      readonly: false,
    };
  }
  if (username) return { value: { kind: 'unrecognized', username }, readonly: false };
  return null;
}
