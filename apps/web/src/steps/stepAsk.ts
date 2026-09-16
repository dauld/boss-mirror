// The one ask a step with declared fields makes, derived from the
// Workflow's own step graph (user-feedback 26ae4d44).
//
// David, 2026-09-15, on a backlog-item triage step assigned to him,
// carrying a brief that said exactly what to enter (disposition
// `build`, evidence 'option b'): "Wasn't quite sure how to fill in the
// job properly to move to the next step." He gave the decision in chat
// and an agent completed the step for him. The surface had every piece
// — the brief, a select of six bare words, a text box, a button — and
// none of them said which route each word takes or what the button
// does. The words came from `field_type = "verify|design|build|…"`;
// what each one OPENS was already written in the successors'
// `ready_when` predicates, one fetch away, and never read.
//
// Everything here is a pure function of registry data. There is no map
// from a disposition to a meaning: the meaning of `build` IS the step
// it unlocks ("Build the change"), read off the predicate that names
// it. Add a value to the enum and a successor gated on it, and the
// select, the button and the reason line all follow with no change
// here. Pure so it is testable without a DOM; GenericSurface owns the
// fetch and the form.

import type { StepField } from '../jobs/types';
import { routeFor, type SpecStep } from '../jobs/fork';

/// One value of an enum-shaped field and the step it opens, or null
/// when no predicate in the graph reads that value (the Workflow's
/// viability lint means that is a spec the graph does not know, not a
/// value with no consequence).
export type AskRoute = Readonly<{ value: string; route: string | null }>;

/// A pipe-shaped `field_type` is an enum domain — the same shape the
/// Workflow viability lint reads to prove fork coverage. Anything
/// else is free text.
export function optionsFor(f: Pick<StepField, 'field_type'>): ReadonlyArray<string> | null {
  return f.field_type.includes('|') ? f.field_type.split('|') : null;
}

/// Every option of `field` on the step `slug`, each with the step it
/// routes to. A free-text field has no options and so no routes; an
/// enum field on a step whose spec is unknown keeps its options with
/// every route null, so the select still renders its words.
export function askRoutes(
  steps: ReadonlyArray<SpecStep> | null,
  slug: string | null,
  field: Pick<StepField, 'name' | 'field_type'>,
): ReadonlyArray<AskRoute> {
  const options = optionsFor(field) ?? [];
  return options.map((value) => ({
    value,
    route: steps && slug ? routeFor(steps, slug, field.name, value) : null,
  }));
}

/// Which spec step a materialised step is. `spec_slug` is on the wire
/// and wins; older callers and fixtures without it fall back to the
/// spec step whose rendered `title_template` (or slug) is this step's
/// title.
export function specSlugOf(
  step: Readonly<{ spec_slug?: string; title: string }>,
  steps: ReadonlyArray<SpecStep> | null,
): string | null {
  if (step.spec_slug) return step.spec_slug;
  const match = steps?.find((s) => s.title_template === step.title || s.title === step.title);
  return match?.title ?? null;
}

/// The completing control's label: the effect of the answer chosen,
/// when the graph names one. A person reads "Complete — routes to:
/// Build the change" and knows what the click does before making it.
export function completeLabel(routes: ReadonlyArray<AskRoute>, chosen: string): string {
  const route = routes.find((r) => r.value === chosen)?.route;
  return route ? `Complete — routes to: ${route}` : 'Complete';
}

/// The required fields still blank. Whitespace is blank: an empty
/// string satisfies no contract, and the surface never sends one.
export function missingRequired(
  fields: ReadonlyArray<StepField>,
  values: Readonly<Record<string, string>>,
): ReadonlyArray<string> {
  return fields.filter((f) => f.required && !(values[f.name] ?? '').trim()).map((f) => f.name);
}

/// The reason the control is disabled, as a sentence that NAMES the
/// field — not a hover title. Null when nothing is missing.
export function needsLine(missing: ReadonlyArray<string>): string | null {
  if (missing.length === 0) return null;
  return `Needs: ${missing.map((n) => n.replace(/_/g, ' ')).join(', ')}`;
}
