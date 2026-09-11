/**
 * Format an actor id — the audit-log `_actor` / event `actor_id` — into a
 * human-readable label.
 *
 * Every transition is fired by exactly one of three kinds of CPU (the
 * `ActorId` union in `crates/core/boss-core/src/actor.rs`):
 *   - a **human**, a bare employee id (`emp-032`); resolved to a name when
 *     the caller passes an `empNames` map, otherwise shown as the id.
 *   - a **named automation**, carrying the `automation:` prefix with an
 *     explicit authority — a dispatch rule (`automation:rule:<name>`), the
 *     dispatcher, the simulator, or the emitting service.
 *   - an **agent session**, in either of two spellings — `<mode>:<model>`
 *     (`claude:opus-5[1m]`, what `agent_runs.actor_id` carries) or the
 *     ADDRESS the session signs its jobs-API calls with
 *     (`claude@algedonic.dev`, what `steps.assignee_id` carries). Both
 *     name an LLM CPU; `claude` is the mode for an interactive Claude
 *     session.
 *
 * The branch order in `formatActor` mirrors `ActorId::from_str` and is
 * load-bearing: `automation:` is claimed first because its slug may itself
 * carry colons (`automation:rule:bill-approve`), then any remaining
 * colon-bearing id is an agent, and a colon-free id falls through to the
 * employee lookup. Get that order wrong and an agent renders as a missing
 * employee — which is what it used to do. An address-form agent id has no
 * branch of its own there *deliberately*: the lookup misses and the
 * address reads out whole, which is what operators already read on the
 * incidents strip. Only the PREDICATE needs to know it is a machine.
 *
 * There is deliberately no anonymous "system" actor (removed in v1.1.0). A
 * legacy null/empty reads as the `platform` automation — never a fake human.
 */
export function isHumanActor(actorId: string | null | undefined): boolean {
  // The TS mirror of `ActorId::is_human`, and the one definition of that
  // rule on the client — callers that need "is this the machine?" negate
  // it rather than re-deriving it.
  //
  // THREE machine shapes, because the system writes three:
  //
  //  1. A COLON. Every named automation (`automation:*`), the legacy bare
  //     `rule:*`, and the `<mode>:<model>` agent form carry one; no
  //     employee id does (`emp-032`, `emp-bootstrap-admin`, `emp-aa-185`).
  //
  //  2. An `@`. An actor id shaped like an address is a SESSION LOGIN, and
  //     a login is not a person. BOSS resolves a human's login to an
  //     employee id before any write is signed — `boss-gateway/src/oidc.rs`
  //     takes the IdP's email through `bootstrap_email` and puts
  //     `session.employee_id` (an `emp-*` id) on the session, failing
  //     closed when no employee matches. So an id that is still an address
  //     where a step records it was never resolved to a person: it is a
  //     session identity, and on this deployment that means an agent. The
  //     address form is what `BOSS_ACTOR` carries on the dev pod, so every
  //     `boss` verb the agent runs signs with it.
  //
  //  3. The literal `system`. `ActorId::from_str` maps it to the named
  //     `platform` automation rather than to a fake human ("'No one did
  //     it' is not a representable state, and neither is 'the system did
  //     it'"), and this mirror had drifted from that: it has no colon and
  //     no `@`, so it read as an employee. Nothing live carries it
  //     (`GET /api/jobs?owner_id=system-sim` → `total: 0`, on a connection
  //     whose other reads answer nonzero), so this pins the mirror rather
  //     than changing any rendering today.
  //
  // WHY THIS IS HERE (backlog a6b10413). The colon alone was the whole
  // test, which was true while `automation:` and `emp-` were the only
  // prefixes and stopped being true when an agent began being assigned
  // steps by address. Measured on the system of record 2026-09-11:
  // `claude@algedonic.dev` held 142 step assignments across the 69 open
  // packets and signed 12 completions — so the one id doing most of the
  // department's work read as staff on every surface asking this question,
  // and any census of the human/machine boundary counted it as a person.
  //
  // THE HONEST LIMIT: this is a fix to the PREDICATE, not to the
  // vocabulary. One actor still has two spellings — the address here and
  // `claude:opus-5[1m]` in `agent_runs` — so "what did this actor build,
  // and what did it cost" is still a join nothing can make. That is one
  // spelling per actor (§9a), it touches live data and the `ActorId`
  // union, and it is filed separately rather than half-done here.
  return !!actorId && !actorId.includes(':') && !actorId.includes('@') && actorId !== 'system';
}

/** @see the module docstring — this is the label side of {@link isHumanActor}. */
export function formatActor(
  actorId: string | null | undefined,
  empNames?: ReadonlyMap<string, string>,
): string {
  if (!actorId) return 'Platform';

  if (actorId.startsWith('automation:')) {
    const authority = actorId.slice('automation:'.length);
    // A dispatch rule names the rule that fired the side-effect.
    if (authority.startsWith('rule:')) {
      return `Rule · ${authority.slice('rule:'.length)}`;
    }
    const KNOWN: Readonly<Record<string, string>> = {
      dispatcher: 'Dispatcher',
      sim: 'Simulator',
      platform: 'Platform',
    };
    // Otherwise it's a service slug (`automation:account-provisioning`); title-case it.
    return KNOWN[authority] ?? titleCase(authority);
  }

  const split = actorId.indexOf(':');
  if (split > -1) {
    // Agent — `<mode> · <model>`, same separator the rule chip uses. The
    // model half stays verbatim: it is a vendor string, and title-casing
    // `opus-5` into `Opus 5` would name a model that does not exist.
    return `${titleCase(actorId.slice(0, split))} · ${actorId.slice(split + 1)}`;
  }

  // Human — an employee id. Use a friendly name when we have one.
  return empNames?.get(actorId) ?? actorId;
}

function titleCase(slug: string): string {
  return slug
    .split('-')
    .map((w) => (w ? w[0]!.toUpperCase() + w.slice(1) : w))
    .join(' ');
}
