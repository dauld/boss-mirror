# Dispatcher rule registry — one file per rule

`<rule-name>.toml` holds exactly one `[[rule]]` whose `name` is the file
name. **Adding a rule is dropping a file in.** Nothing is appended
anywhere; two cars adding rules touch no shared line.

Per docs/architecture-decisions.md §Dispatcher — the event router: rules
are the data-driven reactive layer. Each `[[rule]]` declares what to
listen for (`on_event`) or when to fire (`schedule`), an optional
predicate against the payload and system state (`when`), and an ordered
list of side-effect handler invocations to fire when the rule matches
(`do`).

The **runtime** registry is the append-only versioned `dispatcher_rules`
table, which the dispatcher loads from Postgres at startup and serves at
`/api/dispatcher/rules`. This directory is the human-authored source;
`dispatcher_rules_seed_matches_toml` compares the two in BOTH directions,
so a migration that seeds a rule without a file here fails the same way
an unseeded file does. Neither is allowed to be the only one that knows.

**The seed is not the only way in, so the seed guard is not the only
check.** `POST /api/dispatcher/rules` + publish authors a rule live, with
no file and no migration; a TestDb never sees those, so the guard above
stays green while the system enforces rules nobody wrote down. Measured
2026-09-10: sixty-four enforced rules, sixty files — the `why` guard was
covering sixty of sixty-four and reporting "OK (60 rules, each saying why
it exists)", true about this directory and wrong about the system. Two
things close that now:

- **`GET /api/dispatcher/rules` answers what AND why.** Per enforced rule:
  `name`, `version`, `status`, its trigger (`on_event` or `schedule`),
  `when`, `do`, `delay`, the `why` this directory records, and `authored`
  — false for a rule no file here records. `authored_registry` names the
  directory the whys were read from (`BOSS_DISPATCHER_RULES`, which the
  image carries at `/opt/boss/infra/dispatcher/rules`) and reports any
  error reading it, so `why: null` everywhere never silently means
  "no rule records a why" when it means "I could not read the source".
- **`infra/lint/the-live-rules-are-the-authored-rules.sh`** compares the
  live set to this directory and fails on a rule the dispatcher enforces
  that neither a file here records nor a migration retires. It skips
  loudly where the read surface is unreachable (the forge gate host has
  no route), and a 200 with no rules — or zero rules — is a failure, not
  a pass.

  **Retiring a rule is the reverse of adding one, and it leans on the
  same window.** A retirement deletes the file here and retires the row
  in a migration — and a migration runs at converge, which is AFTER the
  consist check that boards the car. So for that window the rule is still
  live with no file, the mirror image of the add case, and the lint reads
  the retirement out of `infra/postgres/schema/` rather than failing on
  it: it replays every `dispatcher_rules` status write in apply order and
  asks whether the tree's last word on that rule AT THAT VERSION is a
  retirement. A version bump (retire v1, insert v2 active) is therefore
  not a retirement, and a rule no migration mentions at all is not one
  either — that is still the drift the check exists for. The first
  version of the lint read only the add direction and so blocked every
  retirement car from a train; `202609101200-the-flush-pipeline-is-deleted.sql`
  is the worked example of a retirement it reads.

This directory was one file (`rules.toml`) until 2026-09-09, when two
rule cars parked in one window and the second was left behind on
`conflict: infra/dispatcher/rules.toml, infra/lint/dispatcher-rules-ratchet.sh`
— the exact shape CLAUDE.md §9a records for `manifest.txt`, collapsed
the same way `infra/postgres/schema/` and `infra/platform/workflows/`
were: the listing IS the definition, and every reader derives it
independently (`registry::parse_raw_path` reads the directory in
file-name order; the ratchet lint and `timers-leave-a-packet.sh` glob
`*.toml`). Load order carries no meaning — matching is per event, and
the seed comparison is by name.

## Adding a rule touches THREE places

Three of them refuse the gate one at a time if you miss them. Twice on
2026-09-08 a builder learned the last one from a red gate twenty minutes
in, which is what this note is for.

1. **A FILE HERE** — `<rule-name>.toml`, one `[[rule]]`, named for the
   file, carrying its `why` (below). The loader refuses a file named for
   a rule it does not hold, a file holding two, and a rule with no `why`.
2. **A timestamped migration under `infra/postgres/schema/`** that
   inserts the same row, so a fresh database has it. Dropping the file
   into that directory is the whole act — it is sorted by its `NNN-`
   prefix and there is no manifest to append to since 2026-08-15.
   `41-dispatcher.sql` is APPLIED history and is never regenerated; a
   rule added since gets its own `NNN-dispatcher-rule-*.sql` with an
   `ON CONFLICT`-safe INSERT (`101-dispatcher-rule-step-assigned.sql` is
   the worked example).
3. **`handler_emits()` in `crates/core/boss-dispatcher/src/cascade.rs`**,
   IF your `do` names a handler not already listed there. Its guard
   (`cascade_handlers_match_rules`) names the handler and the rule when
   you miss it; it is checking that every rule's cascade is declared, so
   a chain of rules cannot loop unseen. A Rust registration, not a text
   tail — two cars adding handlers do not conflict on it.

The fourth place used to be `infra/lint/dispatcher-rules-ratchet.sh`,
whose hand-typed `BASELINE=60` every rule car had to bump. That integer
was the second half of the merge conflict, and it is gone.

## `why` — what the ratchet was actually for

Every rule file carries a `why`, and **a rule file without one does not
load** (`registry::parse_raw_dir`; the lint says the same thing in ~1s
without a compile).

The ratchet was a shrink-only ceiling on the rule count, and its real
purpose was never the number: it was that raising the ceiling cost a
sentence, in a diff a reviewer sees, saying why this reaction could not
be a protocol consequence. The ceiling was a single contended line;
the sentence was the point. So the sentence moved into the rule's own
file, where it is per-rule rather than per-bump, where it covers all 60
rules instead of only the 22 that happened to arrive after the ratchet
was written, and where it cannot be edited by two cars at once.

Under the 3P admission edge, a dispatcher rule is a reaction the
protocol definition could not express (docs/design/protocol-policy-publish.md).
The census classed the 38 rules of 2026-08-12: seven jobs-internal
consequences that move into WorkflowSpec `on` blocks whole, ~22 domain
effects that become admission-staged obligations, and nine external-glue
reactions that stay. A `why` should name which standing exemption the
rule claims:

- **timer** — no packet causes "a day passed"; a sweep's whole point is
  to run when NO event fired.
- **threshold** — the condition is a LEVEL, not a transition.
- **external ingress / external glue** — the cause or the effect lives
  outside the system (a CronJob's observation, the forge's admin API).
- **cross-protocol reactor** — it spans two or more protocols, which no
  single Workflow `on` consequence can express.
- **routing** — the residue the census owes back to the protocol row.
  Legitimate to keep, never legitimate to ADD: the number to watch is
  the 22 `step.done.*` rules, not the total.

If none of those fits, declare the reaction in the Workflow definition
instead.

## Families

Group notes that describe several rules at once live here rather than in
whichever member file happens to sort first.

**Step-completion side effects.** Each `*-on-*-step-done` rule routes a
`step.done.<kind>` event to the handler that runs the step's side
effect. Handler implementations live at
`boss-dispatcher::rules::handlers::*`. This is the routing residue
above.

**External-party callbacks** (`forward-*-to-webhook`). Forward the
events the simulator's CounterpartyEngine reacts to — banks, suppliers,
the keg courier, the tax authority, the operational broadcasts — to its
callback receiver via `webhook.notify`. The simulator never subscribes
to the event stream; the system pushes to a configured webhook,
preserving the sim/system boundary. No-op in any deployment without
`BOSS_EVENT_WEBHOOK_URL` set.

**Delegate-subjob** (Workflow v2, D7) — the spawn → link → resolve loop.
A `delegate-subjob` step spawns a child Job of `metadata.subworkflow`
when it becomes Ready, then pauses; when that child Job closes, the
child's terminal outcome is written back into the parent step's
`subjob_outcome` and the parent step flips to Completed.

1. **SPAWN** (`spawn-subjob-on-delegate-subjob-step-ready`) — on the D6
   ready marker, `jobs.spawn` opens the child Job and sets the parent
   step's `embedded_job` (the forward link). The `step.ready.<kind>`
   payload mirrors `step.done.<kind>`: it carries `job_id` / `step_id` /
   `subject_kind` / `subject_id` / `metadata`, so the args read the child
   kind out of step metadata and pass the parent step id through for
   linkage.
2. **RESOLVE** (`resolve-subjob-on-child-job-closed`) — on the child's
   close, `jobs.subjob_resolve` writes the outcome back and completes the
   parent step. The `when` gate keeps the rule from firing for ordinary
   (non-delegated) Job closes; the JOB_CLOSED marker carries
   `parent_step_id` (null when absent).
