// Reading a queue's fork out of the Workflow registry, and finding the
// step that carries it on a given Job.
//
// This lives in one module because it has already drifted once. The
// terminal queue reader found the triage step by matching a step KIND
// while the board had moved to finding it by its authority gate, and
// the two disagreed the same day — the reader reported a freshly filed
// item as already triaged. CLAUDE.md §9a: a fact that lives twice gets
// an equality test, or it gets collapsed. This is the collapse.
//
// Everything here derives from registry data. Nothing hardcodes a
// disposition, a step kind, or a field name: add a disposition to the
// Workflow and the callers pick it up with no change.

import type { Job, Step } from './types';

export type ForkOption = Readonly<{ value: string; label: string }>;

/// A queue's fork: the field a triage step sets, and the routes its
/// values open.
export type Fork = Readonly<{ field: string; options: ReadonlyArray<ForkOption> }>;

/// The step a Job is parked on: the one gated on human authority.
/// Found by that property rather than by matching a step kind — kinds
/// are registry data and a kind is a bundle of properties, so matching
/// one would pin today's spelling of a spec the registry is free to
/// re-author.
export function gatedStep(j: Job): Step | undefined {
  return j.steps?.find(
    (s) => (s.metadata as Record<string, unknown> | undefined)?.['authority_role'],
  );
}

/// The fork step ON A JOB — the gated step that asks for a
/// disposition. Identified by carrying the enum field, so it stays
/// correct if the spec renames or reorders steps.
///
/// Falls back to the gated step when the Job has no field-bearing one.
/// That is not defensive padding: a Job materialises its steps at open
/// and keeps them, so Jobs opened before the fork existed carry the old
/// shape forever. Without the fallback their cards render but every
/// control silently does nothing, which is the worst failure available
/// — the surface would look fine and refuse to work.
export function forkStep(j: Job, fork: Fork | null): Step | undefined {
  const field = fork?.field;
  const byField = field
    ? j.steps?.find((s) => s.fields?.some((f) => f.name === field))
    : undefined;
  return byField ?? gatedStep(j);
}

/// The disposition chosen on a Job, or null if it has not been routed.
/// Reads the value off the fork step rather than anywhere else: the
/// step's metadata is where the decision is recorded, and a Job that
/// has not reached the fork simply has none.
export function disposition(j: Job, fork: Fork | null): string | null {
  if (!fork) return null;
  const v = forkStep(j, fork)?.metadata?.[fork.field];
  return typeof v === 'string' && v.length > 0 ? v : null;
}

/// The shape of a registry step this module reads: its slug (`title`),
/// its predicate, and the human name a successor lends to the route
/// that opens it.
export type SpecStep = Readonly<{
  title?: string;
  ready_when?: string;
  title_template?: string | null;
}>;

/// The step that `steps.<slug>.metadata.<field> = "<value>"` opens —
/// the ROUTE that answer takes — or null when no predicate reads it.
///
/// Anchored to the slug: two steps can fork on a field spelled the
/// same way (user-feedback's `triage` and `investigate` both set
/// `disposition`), and a match on the bare `field = "value"` would
/// hand `investigate`'s options `triage`'s successors. One reader for
/// the queue boards (readFork) and the step surface (stepAsk, feedback
/// 26ae4d44), so the two cannot disagree about which step an answer
/// opens (CLAUDE.md 9a).
export function routeFor(
  steps: ReadonlyArray<SpecStep>,
  slug: string,
  field: string,
  value: string,
): string | null {
  const re = new RegExp(
    `steps\\.${escapeRe(slug)}\\.metadata\\.${escapeRe(field)}\\s*=\\s*"${escapeRe(value)}"`,
  );
  const successor = steps.find((s) => re.test(s.ready_when ?? ''));
  return successor ? successor.title_template || successor.title || null : null;
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/// Read the queue's fork out of the Workflow registry: the step with a
/// required pipe-shaped field is the fork, its values are the
/// dispositions, and each successor's `title_template` is that route's
/// human name. Deriving the label from the successor rather than
/// humanising the slug means a surface says what the next step IS —
/// "Reproduce and investigate", not "Reproduce".
export function readFork(spec: unknown): Fork | null {
  const steps = (spec as { steps?: unknown[] })?.steps;
  if (!Array.isArray(steps)) return null;

  for (const step of steps as SpecStep[]) {
    const fields = (step as { fields?: unknown[] }).fields ?? [];
    for (const f of fields) {
      const field = f as { name?: string; field_type?: string; required?: boolean };
      if (!field.required || !field.name || !field.field_type?.includes('|')) continue;
      const name = field.name;
      const options = field.field_type.split('|').map((value) => ({
        value,
        label: routeFor(steps as SpecStep[], step.title ?? '', name, value) ?? value,
      }));
      return { field: name, options };
    }
  }
  return null;
}
