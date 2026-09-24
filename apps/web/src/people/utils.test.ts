// The roster's Status buttons are built from the (employee, status)
// Classes, not from a hand-written pair (backlog 01c268d6; page audit
// 0c0265a3 GAP 7, 2026-09-23). The hand-written group offered Active /
// On leave / All while `terminated` is one of the three live status
// Classes, so terminated rows — and null-status rows, which render as
// "unknown" — were reachable only under All and no button counted
// them. These cases hold the rule the fix states: every row the roster
// holds is in exactly one bucket, and every status Class has a button.

import { describe, expect, it } from 'bun:test';
import type { Employee, EmploymentStatus } from './types';
import { departmentBuckets, statusBuckets } from './utils';

function emp(
  id: string,
  status: EmploymentStatus | null,
  department: string | null = null,
): Employee {
  return {
    id, name: id, email: null, role: null, department, skill_level: null,
    skills: [], hire_date: null, location: null, manager_id: null,
    employment_type: null, status, certifications: [],
  };
}

/// The three live status Classes, as 01-registries.sql seeds them.
const STATUS_CLASSES = [
  { code: 'active', display_name: 'Active' },
  { code: 'on-leave', display_name: 'On Leave' },
  { code: 'terminated', display_name: 'Terminated' },
];

describe('statusBuckets', () => {
  it('gives every status Class a button, terminated included, counted', () => {
    const roster = [emp('a', 'active'), emp('b', 'active'), emp('t', 'terminated')];
    expect(statusBuckets(roster, STATUS_CLASSES)).toEqual([
      { code: 'active', label: 'Active', count: 2 },
      { code: 'on-leave', label: 'On Leave', count: 0 },
      { code: 'terminated', label: 'Terminated', count: 1 },
    ]);
  });

  it('adds an Unknown bucket only when a null-status row exists', () => {
    const withNull = statusBuckets([emp('a', 'active'), emp('n', null)], STATUS_CLASSES);
    expect(withNull.at(-1)).toEqual({ code: null, label: 'Unknown', count: 1 });
    const without = statusBuckets([emp('a', 'active')], STATUS_CLASSES);
    expect(without.some((b) => b.code === null)).toBe(false);
  });

  it('never leaves a row reachable only under All, even before the registry loads', () => {
    // classesFor answers [] while loading or on error; a status the
    // roster holds still gets its bucket, labelled from its code.
    const roster = [emp('a', 'active'), emp('t', 'terminated'), emp('n', null)];
    const buckets = statusBuckets(roster, []);
    expect(buckets).toEqual([
      { code: 'active', label: 'Active', count: 1 },
      { code: 'terminated', label: 'Terminated', count: 1 },
      { code: null, label: 'Unknown', count: 1 },
    ]);
    expect(buckets.reduce((n, b) => n + b.count, 0)).toBe(roster.length);
  });
});

// The Department buttons are built from the rows the Status selection
// admits, not from the active rows (backlog 1410f145; page audit
// 0c0265a3 GAP 6, 2026-09-23). Built from active rows, the counts
// contradicted the table under On leave or All, a department whose
// people were all on leave or terminated had no button, and a row with
// no department could not be selected by department at all.
describe('departmentBuckets', () => {
  const ALL = { kind: 'all' } as const;

  it('counts the rows it is given, not the active ones', () => {
    const onLeave = [emp('a', 'on-leave', 'it'), emp('b', 'on-leave', 'it'), emp('c', 'on-leave', 'ops')];
    expect(departmentBuckets(onLeave, ALL)).toEqual([
      { code: 'it', label: 'IT', count: 2 },
      { code: 'ops', label: 'Ops', count: 1 },
    ]);
  });

  it('gives rows with no department a selectable bucket, last', () => {
    const buckets = departmentBuckets([emp('a', 'active', 'it'), emp('n', 'active', null)], ALL);
    expect(buckets.at(-1)).toEqual({ code: null, label: 'No department', count: 1 });
    expect(departmentBuckets([emp('a', 'active', 'it')], ALL).some((b) => b.code === null)).toBe(false);
  });

  it('puts every row in exactly one bucket', () => {
    const rows = [emp('a', 'active', 'sales'), emp('b', 'terminated', 'it'), emp('n', null, null)];
    const buckets = departmentBuckets(rows, ALL);
    expect(buckets.map((b) => b.code)).toEqual(['it', 'sales', null]);
    expect(buckets.reduce((n, b) => n + b.count, 0)).toBe(rows.length);
  });

  it('keeps the selected department as a zero bucket when the rows hold none of it', () => {
    // Switching Status to one with no one in the selected department
    // must not hide the active button — the table would read empty
    // with no visible filter explaining why.
    const rows = [emp('a', 'on-leave', 'ops')];
    expect(departmentBuckets(rows, { kind: 'code', code: 'it' })).toEqual([
      { code: 'it', label: 'IT', count: 0 },
      { code: 'ops', label: 'Ops', count: 1 },
    ]);
    expect(departmentBuckets(rows, { kind: 'code', code: null }).at(-1)).toEqual({ code: null, label: 'No department', count: 0 });
  });
});
