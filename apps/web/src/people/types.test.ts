// /ux/people labels a department or role from the Class registry's
// display_name, not from its code (backlog 8a331c9b; page audit
// 0c0265a3 GAP 10, 2026-09-23). The roster ran every department and
// role through humanizeClassCode, so the live `operations` department
// printed Operations where its Class says Operations / IT, and the
// `platform-admin` role printed Platform Admin for Platform admin.
// humanizeClassCode stays as the fallback for a code the registry does
// not hold — or holds nothing yet, while it loads or after it fails.

import { describe, expect, it } from 'bun:test';
import { classLabel, employeeRecordRead } from './types';

/// Three live (employee, department) Classes, as the registry answered
/// on 2026-09-24.
const DEPARTMENT_CLASSES = [
  { code: 'it', display_name: 'IT' },
  { code: 'sales', display_name: 'Sales & Sponsorships' },
  { code: 'operations', display_name: 'Operations / IT' },
];

describe('classLabel', () => {
  it("labels a code the registry holds with the registry's display_name", () => {
    expect(classLabel('operations', DEPARTMENT_CLASSES)).toBe('Operations / IT');
    expect(classLabel('sales', DEPARTMENT_CLASSES)).toBe('Sales & Sponsorships');
  });

  it('humanizes a code the registry lacks, and every code before it loads', () => {
    expect(classLabel('head-of-sales', DEPARTMENT_CLASSES)).toBe('Head of Sales');
    expect(classLabel('operations', [])).toBe('Operations');
  });

  it('renders a missing value as a dash, as humanizeClassCode does', () => {
    expect(classLabel(null, DEPARTMENT_CLASSES)).toBe('—');
    expect(classLabel(undefined, DEPARTMENT_CLASSES)).toBe('—');
  });
});

// The employee page read `e.skills.length` off whatever 2xx body the
// detail read returned (backlog 548a1e8d, 2026-09-23): under the mocked
// floor that body is `[]`, so every field is undefined and the page threw
// "Cannot read properties of undefined (reading 'length')" rather than
// saying the read was malformed. boss-people serializes both lists on
// every row (types.rs, no skip_serializing_if), so a row without one is
// not an employee with no skills — it is a read that did not return an
// employee, and the page must say which field it lacked.
describe('employeeRecordRead', () => {
  const READ = '/api/people/emp-001';
  const ROW = {
    id: 'emp-001', name: 'Demo CEO', email: null, role: null, department: null,
    skill_level: null, skills: [], hire_date: null, location: null,
    manager_id: null, employment_type: null, status: null, certifications: [],
  };

  it('accepts a row carrying its id and both lists, however sparse the rest', () => {
    expect(employeeRecordRead(READ, ROW)).toEqual({ kind: 'ok' });
    const { name: _name, email: _email, ...identityOnly } = ROW;
    expect(employeeRecordRead(READ, identityOnly)).toEqual({ kind: 'ok' });
  });

  it('names the list a row omits, rather than letting it read as empty', () => {
    const { skills: _s, ...noSkills } = ROW;
    expect(employeeRecordRead(READ, noSkills)).toEqual({
      kind: 'failed',
      error: `${READ}: the record carries no skills list`,
    });
    const { certifications: _c, ...noCerts } = ROW;
    expect(employeeRecordRead(READ, noCerts)).toEqual({
      kind: 'failed',
      error: `${READ}: the record carries no certifications list`,
    });
    expect(employeeRecordRead(READ, { ...ROW, skills: null })).toEqual({
      kind: 'failed',
      error: `${READ}: the record carries no skills list`,
    });
  });

  it('refuses a body that is not a record at all, which the mocked floor answers', () => {
    for (const body of [[], null, 'not found', 7]) {
      expect(employeeRecordRead(READ, body)).toEqual({
        kind: 'failed',
        error: `${READ}: the answer is not an employee record`,
      });
    }
    const { id: _id, ...noId } = ROW;
    expect(employeeRecordRead(READ, noId)).toEqual({
      kind: 'failed',
      error: `${READ}: the answer is not an employee record`,
    });
  });
});
