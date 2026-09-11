// People domain — Boss employees. Port of apps/web/src/people/types.ts.

export type EmployeeId = string;

/// Class registry code under
/// `(subject_kind='employee', member_attribute='department')`.
/// Open string — tenants extend the taxonomy via the registry.
/// Display labels are looked up from the registry's `display_name`
/// field; the SPA's local helper falls back to a kebab→Title-Case
/// transform when the registry isn't loaded.
export type Department = string;

/// Class registry code under
/// `(subject_kind='employee', member_attribute='role')`. Same
/// open-string shape as Department.
export type Role = string;

// Abbreviations that should render fully uppercase even when the
// registry's display_name isn't loaded (`coo` → `COO`, not `Coo`).
const UPPER_ABBREVIATIONS = new Set([
  'ceo', 'coo', 'cfo', 'cto', 'cmo', 'cio', 'cpo', 'cso',
  'vp', 'svp', 'evp', 'hr', 'it', 'qa', 'qc', 'pm', 'pr',
]);

/// Pretty-print a kebab-case Class code (`'sales-mgr'` → `'Sales Mgr'`,
/// `'coo'` → `'COO'`, `'head-of-sales'` → `'Head of Sales'`). Used as
/// a fallback when the registry's display_name isn't loaded.
export function humanizeClassCode(code: string | null | undefined): string {
  if (!code) return '—';
  // Words that should stay lowercase mid-phrase (English connectives).
  const lower = new Set(['of', 'and', 'the', 'for', 'in', 'on']);
  return code
    .split('-')
    .map((part, i) => {
      if (part.length === 0) return part;
      if (UPPER_ABBREVIATIONS.has(part)) return part.toUpperCase();
      if (i !== 0 && lower.has(part)) return part;
      return part.charAt(0).toUpperCase() + part.slice(1);
    })
    .join(' ');
}

export type EmploymentStatus = 'active' | 'on-leave' | 'terminated';

/// Tone for the web-kit StatusChip; mirrors the retired chip-emp-*
/// CSS colors (active was --ok, on-leave --warn, terminated muted).
/// `null` is identity-first "not yet onboarded into a status" —
/// muted, and call sites render it as "unknown".
export function employmentTone(status: EmploymentStatus | null): 'ok' | 'warn' | 'muted' {
  if (status === 'active') return 'ok';
  if (status === 'on-leave') return 'warn';
  return 'muted';
}

export type EmploymentType = 'full-time' | 'part-time' | 'contractor';
/// Location id (FK to the Locations registry, e.g. `loc-hq`,
/// `loc-remote-default`). Pretty-name lookup against the locations
/// service is a follow-up; today the SPA renders the id directly.
export type LocationId = string;

export type Certification = {
  name: string;
  issuing_body: string;
  issued_on: string;
  expires_on: string | null;
};

export type Employee = {
  id: EmployeeId;
  // Identity-first: only `id` is guaranteed. Descriptive fields are
  // enriched as onboarding proceeds, so each is nullable until set.
  name: string | null;
  email: string | null;
  role: Role | null;
  department: Department | null;
  skill_level: number | null;
  skills: ReadonlyArray<string>;
  hire_date: string | null;
  location: LocationId | null;
  manager_id: EmployeeId | null;
  employment_type: EmploymentType | null;
  status: EmploymentStatus | null;
  certifications: ReadonlyArray<Certification>;
};
