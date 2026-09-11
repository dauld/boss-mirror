import { describe, expect, test } from 'bun:test';
import { classifyProbe, type Employee } from './classify';

const emp: Employee = {
  id: 'emp-1', name: 'Ada', email: 'ada@x', role: 'platform-admin',
  department: 'platform', hire_date: '2020-01-01', status: 'active',
  location: 'hq', employment_type: 'ft', skills: [], certifications: [],
};
const byId = new Map([[emp.id, emp]]);

describe('classifyProbe', () => {
  test('a resolved employee is ready and writable', () => {
    const c = classifyProbe({ username: 'ada@x', employee_id: 'emp-1' }, byId)!;
    expect(c.value.kind).toBe('ready');
    expect(c.readonly).toBe(false);
  });

  test('audit-readonly with no employee is the guest — first-class and read-only', () => {
    const c = classifyProbe(
      { username: 'guest@algedonic.dev', role: 'audit-readonly' },
      byId,
    )!;
    expect(c.value.kind).toBe('ready');
    expect(c.readonly).toBe(true);
    if (c.value.kind === 'ready') {
      expect(c.value.user.name).toBe('Guest');
      expect(c.value.user.role).toBe('audit-readonly');
    }
  });

  test('any other unmatched session stays unrecognized', () => {
    const c = classifyProbe({ username: 'who@x', role: 'service-tech' }, byId)!;
    expect(c.value.kind).toBe('unrecognized');
  });

  test('no session at all falls through', () => {
    expect(classifyProbe({}, byId)).toBeNull();
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
    const c = classifyProbe(probe, byId)!;
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
    expect(classifyProbe(probe, byId)!.readonly).toBe(false);
  });
});
