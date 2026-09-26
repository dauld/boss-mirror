// The reporting chain says where it STOPS and why (backlog 1a83fe98,
// found by the builder of 03c88448, 2026-09-24). EmployeePage walked
// manager_id through the roster map, and a manager_id the roster did not
// hold answered `undefined` exactly as a null one did — so an employee
// whose record NAMES a manager printed "No manager — reports to board.",
// and a chain whose ancestor pointed outside the roster simply ended,
// reading as though the last person shown reports to the board. These
// cases hold the three ways a walk ends apart, and the fourth — a
// manager_id cycle — which used to loop forever.

import { describe, expect, it } from 'bun:test';
import type { Employee } from './types';
import { reportingChain } from './utils';

function emp(id: string, manager_id: string | null): Employee {
  return {
    id, name: id, email: null, role: null, department: null, skill_level: null,
    skills: [], hire_date: null, location: null, manager_id,
    employment_type: null, status: 'active', certifications: [],
  };
}

const byId = (rows: ReadonlyArray<Employee>): ReadonlyMap<string, Employee> =>
  new Map(rows.map((e) => [e.id, e]));

describe('reportingChain', () => {
  it('ends at the board only when the last manager has no manager_id', () => {
    const top = emp('emp-001', null);
    const mid = emp('emp-002', 'emp-001');
    const self = emp('emp-003', 'emp-002');
    const walk = reportingChain(self, byId([top, mid, self]));
    expect(walk.chain.map((e) => e.id)).toEqual(['emp-002', 'emp-001']);
    expect(walk.end).toEqual({ kind: 'board' });
  });

  it('an employee with no manager_id reports to the board with an empty chain', () => {
    const self = emp('emp-001', null);
    expect(reportingChain(self, byId([self]))).toEqual({ chain: [], end: { kind: 'board' } });
  });

  it('a direct manager the roster does not hold is a named gap, not the board', () => {
    const self = emp('emp-003', 'emp-gone');
    const walk = reportingChain(self, byId([self]));
    expect(walk.chain).toEqual([]);
    expect(walk.end).toEqual({ kind: 'unresolved', managerId: 'emp-gone', of: 'emp-003' });
  });

  it('a gap part-way up names the id and whose manager it is', () => {
    const mid = emp('emp-002', 'emp-gone');
    const self = emp('emp-003', 'emp-002');
    const walk = reportingChain(self, byId([mid, self]));
    expect(walk.chain.map((e) => e.id)).toEqual(['emp-002']);
    expect(walk.end).toEqual({ kind: 'unresolved', managerId: 'emp-gone', of: 'emp-002' });
  });

  it('a manager_id cycle stops at the first repeat and says so', () => {
    const a = emp('emp-a', 'emp-b');
    const b = emp('emp-b', 'emp-a');
    const self = emp('emp-self', 'emp-a');
    const walk = reportingChain(self, byId([a, b, self]));
    expect(walk.chain.map((e) => e.id)).toEqual(['emp-a', 'emp-b']);
    expect(walk.end).toEqual({ kind: 'cycle', managerId: 'emp-a', of: 'emp-b' });
  });

  it('an employee who is their own manager is a cycle, not an endless chain', () => {
    const self = emp('emp-self', 'emp-self');
    const walk = reportingChain(self, byId([self]));
    expect(walk.chain).toEqual([]);
    expect(walk.end).toEqual({ kind: 'cycle', managerId: 'emp-self', of: 'emp-self' });
  });
});
