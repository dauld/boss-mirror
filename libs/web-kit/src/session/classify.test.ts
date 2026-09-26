import { describe, expect, test } from 'bun:test';
import { classifyProbe, readPeopleRow, READ_ONLY_GUEST_ROLES, type Employee, type ViewerRead } from './classify';

const emp: Employee = {
  id: 'emp-1', name: 'Ada', email: 'ada@x', role: 'platform-admin',
  department: 'platform', hire_date: '2020-01-01', status: 'active',
  location: 'hq', employment_type: 'ft', skills: [], certifications: [],
};
const absent: ViewerRead = { kind: 'absent' };
const found: ViewerRead = { kind: 'found', employee: emp };

describe('classifyProbe', () => {
  test('a resolved employee is ready and writable', () => {
    const c = classifyProbe({ username: 'ada@x', employee_id: 'emp-1' }, found)!;
    expect(c.value.kind).toBe('ready');
    expect(c.readonly).toBe(false);
  });

  test('audit-readonly with no employee is the guest — first-class and read-only', () => {
    const c = classifyProbe(
      { username: 'guest@algedonic.dev', role: 'audit-readonly' },
      absent,
    )!;
    expect(c.value.kind).toBe('ready');
    expect(c.readonly).toBe(true);
    if (c.value.kind === 'ready') {
      expect(c.value.user.name).toBe('Guest');
      expect(c.value.user.role).toBe('audit-readonly');
    }
  });

  // Design 2830b6b7 (2026-09-25): the OSS default guest carries
  // `visitor`, not `audit-readonly`. Unrecognised, the guest button
  // signed a visitor in and the app answered "no matching employee".
  test('visitor with no employee is the guest too — and carries its own role', () => {
    const c = classifyProbe({ username: 'guest@algedonic.dev', role: 'visitor' }, absent)!;
    expect(c.value.kind).toBe('ready');
    expect(c.readonly).toBe(true);
    if (c.value.kind === 'ready') {
      expect(c.value.user.name).toBe('Guest');
      expect(c.value.user.role).toBe('visitor');
    }
  });

  test('the guest roles are exactly the read-only floor', () => {
    expect([...READ_ONLY_GUEST_ROLES]).toEqual(['audit-readonly', 'visitor']);
  });

  test('any other unmatched session stays unrecognized', () => {
    const c = classifyProbe({ username: 'who@x', role: 'service-tech' }, absent)!;
    expect(c.value.kind).toBe('unrecognized');
  });

  test('no session at all falls through', () => {
    expect(classifyProbe({}, absent)).toBeNull();
  });
});

// A break-glass session is not an employee, on purpose: Q4 of
// docs/design/break-glass-is-a-key-you-hold.md mints a narrow role
// with NO employee id, because resolving one would make the emergency
// door depend on boss-people being up — the dependency the design
// forbids the verifier to have. Classified as `unrecognized`, the
// first live assertion with a hardware key opened the door and the
// app answered "no matching employee in the roster": an error on the
// one session that only exists when everything else has failed
// (packet 2ef7726b, 2026-09-08).
describe('a break-glass session is a first-class operator identity', () => {
  const probe = { username: 'break-glass-operator', role: 'break-glass' };

  test('role break-glass with no employee resolves to an operator', () => {
    const c = classifyProbe(probe, absent)!;
    expect(c.value.kind).toBe('ready');
    if (c.value.kind === 'ready') {
      expect(c.value.user.role).toBe('break-glass');
      expect(c.value.user.name).toBe('Break-glass operator');
      expect(c.value.user.id).toBe('break-glass-operator');
    }
  });

  test('it is not the read-only guest — its levers are writes', () => {
    // deploy rollback, merge approval, auth administration. Marking
    // the session readonly would disable every WriteGate and make the
    // chrome disagree with what the server actually grants.
    expect(classifyProbe(probe, absent)!.readonly).toBe(false);
  });
});

// Backlog b4f68a65 (measured 2026-09-26): the shell read the whole
// /api/people roster to find the viewer, and a failed read left an
// empty map — so a signed-in operator classified as `unrecognized`
// ("no matching employee in the roster") because the people service
// blinked, and nothing said a read had failed. The shell now reads the
// viewer's ONE row, and a read that did not answer is its own state.
describe('the viewer is one people row, and a failed read says so', () => {
  const probe = { username: 'ada@x', employee_id: 'emp-1', role: 'platform-admin' };
  type Resp = Pick<Response, 'ok' | 'status' | 'json'>;
  const answering = (status: number, body: unknown) => {
    const asked: string[] = [];
    const fetchFn = async (url: string): Promise<Resp> => {
      asked.push(url);
      return { ok: status >= 200 && status < 300, status, json: async () => body };
    };
    return { asked, fetchFn };
  };

  test('it reads exactly the viewer’s own row, by id', async () => {
    const { asked, fetchFn } = answering(200, emp);
    const read = await readPeopleRow('emp-1', fetchFn);
    expect(asked).toEqual(['/api/people/emp-1']);
    expect(read).toEqual({ kind: 'found', employee: emp });
  });

  test('a refused read is unresolved, naming the read — never unrecognized', async () => {
    const { fetchFn } = answering(503, 'people down');
    const c = classifyProbe(probe, await readPeopleRow('emp-1', fetchFn))!;
    expect(c.value).toEqual({
      kind: 'unresolved',
      username: 'ada@x',
      error: '/api/people/emp-1: HTTP 503',
    });
  });

  test('a network error is unresolved too, carrying the message', async () => {
    const fetchFn = async (): Promise<Resp> => {
      throw new Error('connection reset');
    };
    const c = classifyProbe(probe, await readPeopleRow('emp-1', fetchFn))!;
    expect(c.value.kind).toBe('unresolved');
    if (c.value.kind === 'unresolved') {
      expect(c.value.error).toBe('/api/people/emp-1: connection reset');
    }
  });

  test('a 404 is an answer: the people service holds no such employee', async () => {
    const { fetchFn } = answering(404, 'no employee with ID emp-1');
    const c = classifyProbe(probe, await readPeopleRow('emp-1', fetchFn))!;
    expect(c.value.kind).toBe('unrecognized');
  });

  test('a 200 that is not the viewer’s row is a failed read, not a user', async () => {
    for (const body of [[], null, { error: 'nope' }, { ...emp, id: 'emp-2' }]) {
      const { fetchFn } = answering(200, body);
      expect(await readPeopleRow('emp-1', fetchFn)).toEqual({
        kind: 'failed',
        error: '/api/people/emp-1: not an employee row',
      });
    }
  });

  test('a signed-in employee is never unrecognized on any people-service failure', async () => {
    for (const status of [401, 403, 500, 502, 503, 504]) {
      const { fetchFn } = answering(status, null);
      const c = classifyProbe(probe, await readPeopleRow('emp-1', fetchFn))!;
      expect(c.value.kind).toBe('unresolved');
    }
  });
});
