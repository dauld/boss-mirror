// Pure session logic, deliberately free of runes.
//
// classifyProbe and guestEmployee are ordinary functions: given a
// gateway probe body and a roster, they decide who you are. They used
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
  | { kind: 'unrecognized'; username: string };

export type SessionEnvelope = {
  value: SessionState;
  roster: ReadonlyArray<Employee>;
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

/// Pure classification of the gateway probe — extracted so the
/// guest/unrecognized boundary is a tested decision, not a branch
/// buried in a fetch handler.
export function classifyProbe(
  body: ProbeBody,
  byId: Map<string, Employee>,
): { value: SessionState; readonly: boolean } | null {
  const username = body.username ?? '';
  const emp = body.employee_id ? (byId.get(body.employee_id) ?? null) : null;
  if (emp) return { value: { kind: 'ready', user: emp }, readonly: false };
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
