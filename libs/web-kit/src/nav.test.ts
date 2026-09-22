// The department tabs come from the departments registry — `GET
// /api/departments`, the `departments` table — not a list in this file
// and not the employee Class drawer (ce68f137, then dc5788ba). Run via
// `bun test` from libs/web-kit — plain TypeScript, no runes, so no
// preload is needed.

import { describe, expect, test } from 'bun:test';
import { departmentsFrom } from './nav';

/// One row as `GET /api/departments` serves it: the registry has
/// already dropped retired rows and sorted by `sort_order, id`, so the
/// wire carries neither field.
function row(code: string, display_name: string, fn = 'operations') {
  return { code, display_name, function: fn };
}

function envelope(...rows: ReadonlyArray<unknown>) {
  return { data: rows, total: rows.length };
}

describe('departmentsFrom — the tab vocabulary is the departments registry', () => {
  test('one department per row, in the order the registry answered', () => {
    const got = departmentsFrom(
      envelope(
        row('it', 'IT'),
        row('executive', 'Executive', 'governance'),
        row('sales', 'Sales', 'revenue'),
      ),
    );
    expect(got?.map((d) => d.code)).toEqual(['it', 'executive', 'sales']);
    expect(got?.map((d) => d.label)).toEqual(['IT', 'Executive', 'Sales']);
  });

  test('an empty registry is no department tabs, not a failure', () => {
    expect(departmentsFrom(envelope())).toEqual([]);
  });

  test('a body that is not the envelope is null — the read failed, it is not an empty org chart', () => {
    // The shapes a wrong or unrouted endpoint answers: the bare list
    // the employee Class drawer serves, the SPA fallback's index.html,
    // and nothing at all. Reading any of them as "no departments"
    // would hide the whole org chart silently, which is the failure
    // ce68f137's `{data: [], total: 0}` note already paid for once.
    expect(departmentsFrom([row('it', 'IT')])).toBeNull();
    expect(departmentsFrom('<!doctype html>')).toBeNull();
    expect(departmentsFrom(null)).toBeNull();
    expect(departmentsFrom({ total: 0 })).toBeNull();
  });

  test('a row without a usable code or label is dropped, not rendered as a blank tab', () => {
    const got = departmentsFrom(
      envelope(
        row('sales', 'Sales'),
        { code: '', display_name: 'Nameless' },
        { display_name: 'No code' },
      ),
    );
    expect(got?.map((d) => d.code)).toEqual(['sales']);
  });
});
