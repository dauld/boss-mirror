// The department tabs come from the Class registry, not a list in this
// file (ce68f137). Run via `bun test` from libs/web-kit — plain
// TypeScript, no runes, so no preload is needed.

import { describe, expect, test } from 'bun:test';
import { departmentsFromRegistry } from './nav';

function row(
  code: string,
  display_name: string,
  sort_order: number,
  extra: Partial<{ member_attribute: string; retired_at: string | null }> = {},
) {
  return {
    subject_kind: 'employee',
    code,
    display_name,
    parent_code: null,
    member_attribute: 'department',
    metadata: {},
    sort_order,
    retired_at: null,
    ...extra,
  };
}

describe('departmentsFromRegistry — the tab vocabulary is the registry', () => {
  test('one department per active department row, in sort order', () => {
    const got = departmentsFromRegistry([
      row('operations', 'Operations', 20),
      row('it', 'IT', 1),
      row('executive', 'Executive', 10),
    ]);
    expect(got.map((d) => d.code)).toEqual(['it', 'executive', 'operations']);
    expect(got.map((d) => d.label)).toEqual(['IT', 'Executive', 'Operations']);
  });

  test('an empty registry yields no department tabs, not a crash', () => {
    expect(departmentsFromRegistry([])).toEqual([]);
  });

  test('retired rows and other member attributes are not departments', () => {
    const got = departmentsFromRegistry([
      row('sales', 'Sales', 20),
      row('refurb', 'Refurb', 40, { retired_at: '2026-09-01T00:00:00Z' }),
      row('ceo', 'CEO', 1, { member_attribute: 'role' }),
    ]);
    expect(got.map((d) => d.code)).toEqual(['sales']);
  });
});
