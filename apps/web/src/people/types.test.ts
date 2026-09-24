// /ux/people labels a department or role from the Class registry's
// display_name, not from its code (backlog 8a331c9b; page audit
// 0c0265a3 GAP 10, 2026-09-23). The roster ran every department and
// role through humanizeClassCode, so the live `operations` department
// printed Operations where its Class says Operations / IT, and the
// `platform-admin` role printed Platform Admin for Platform admin.
// humanizeClassCode stays as the fallback for a code the registry does
// not hold — or holds nothing yet, while it loads or after it fails.

import { describe, expect, it } from 'bun:test';
import { classLabel } from './types';

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
