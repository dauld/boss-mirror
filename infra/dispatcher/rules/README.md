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

## This directory IS the registry's definition

The **runtime** registry is the append-only versioned `dispatcher_rules`
table, which the dispatcher loads from Postgres at startup and serves at
`/api/dispatcher/rules`. That table is **derived from this directory**:
`boss-dispatcher`'s boot runs `rules::seed::seed_authored_rules` over it
before loading, publishing every rule a file declares and the table does
not have, and retiring every enforced rule no file here names. So:

- **Adding a rule is dropping a file in.** It is live one deploy later.
- **Changing a rule is bumping its `version`.** The seed writes the row at
  the version the file declares and retires the incumbent — append-only,
  the same move `publish` makes.
- **Retiring a rule is deleting its file.** The next converge retires the
  row.
- **No migration writes rules.** The thirty-one `INSERT INTO
  dispatcher_rules` migrations under `infra/postgres/schema/` are applied
  history: they are never edited (the checksum guard refuses it, which is
  the 2026-08-13 outage) and no new one is ever needed.

**It was two homes until 2026-09-11** (backlog 41ba00cd). A rule was
declared twice, in two languages, with nothing deriving one from the
other: a file here, and an `INSERT` in a migration. Only the second one
ran. `dispatcher_rules_seed_matches_toml` compared them — a §9a **pin**,
which CLAUDE.md is explicit is a holding action and not a destination,
and this is the worst shape of the problem it describes, because one copy
lived in the production database and no test could reach it. The evidence
it had already bitten: on 2026-09-09, establishing whether
`cadence-silence-sweep-daily` was actually firing, a session could
confirm only that its file and its migration were both on main; whether
the row had reached the live registry was unanswerable from the pod.

**The direction was chosen on three measurements**, not on taste:
`bootstrap_reconcile` does nothing to `dispatcher_rules` (it is a
`WorkflowRegistry` / `PolicyRepository` method over `workflows` and
`policy_rules`), so the boot-republish hazard that forced the Workflow
move did not decide it; the `why` below cannot live in a table that has
no column for it and no diff for a reviewer to read; and only the tree can
supply a fresh database, because the row does not exist until something in
the tree writes it. The same shape `boss-platform-workflow-seed` gave
`infra/platform/workflows/` the same week, where `platform_workflows()`
is now `vec![]` and its comment says EMPTY, AND THAT IS THE DESTINATION.

**What the collapse does NOT change: rules are still registry data.**
`POST /api/dispatcher/rules` + publish still authors a rule live, with no
deploy, and the seed never walks a version back — a live version ahead of
this directory is reported in the dispatcher's journal and left alone. A
row already at the authored version is left exactly as it is, whatever its
status, so an operator who retires a misbehaving rule in an incident keeps
it off. What the tree owns is which rules EXIST and what each one says.

**Publishing live and stopping there is still the defect it was.** The
same change owes a file here, or the next converge retires the rule and
the next reader cannot ask why the system was doing what it did. Measured
2026-09-10, before the seed: sixty-four enforced rules, sixty files — the
`why` guard was covering sixty of sixty-four and reporting "OK (60 rules,
each saying why it exists)", true about this directory and wrong about the
system. The four it found (`auto-park-on-gate-green`,
`estate-alarm-on-comparison`, `spawn-keg-return-on-delivery`,
`spawn-tasting-panel-on-brew-close`, backlog 8d471ec5) were two-phase
flips — ship the handler inert, turn it on live — whose phase two never
became a file. Each is why its file narrates where its `why` came from: a
justification invented to satisfy a lint is worse than a named gap, so the
builder who found them listed them instead of backfilling.

Two checks keep that honest, one per question:

- **`GET /api/dispatcher/rules` answers what AND why.** Per enforced rule:
  `name`, `version`, `status`, its trigger (`on_event` or `schedule`),
  `when`, `do`, `delay`, the `why` this directory records, and `authored`
  — false for a rule no file here records. `authored_registry` names the
  directory the whys were read from (`BOSS_DISPATCHER_RULES`, which the
  image carries at `/opt/boss/infra/dispatcher/rules`) and reports any
  error reading it, so `why: null` everywhere never silently means
  "no rule records a why" when it means "I could not read the source".
- **`infra/lint/the-live-rules-are-the-authored-rules.sh`** asks the
  running system whether the derivation is holding: it fails on a rule the
  deployment enforces that the deployment's OWN image does not author, and
  on a deployment that cannot read its authored registry at all. Both
  halves come from the same deployment, so there is no merge-to-converge
  window to reason about — differences against YOUR tree are reported, not
  failed, in both directions. It skips loudly where the read surface is
  unreachable (the forge gate host has no route), and a 200 with no rules
  — or zero rules — is a failure, not a pass.

This directory was one file (`rules.toml`) until 2026-09-09, when two
rule cars parked in one window and the second was left behind on
`conflict: infra/dispatcher/rules.toml, infra/lint/dispatcher-rules-ratchet.sh`
— the exact shape CLAUDE.md §9a records for `manifest.txt`, collapsed
the same way `infra/postgres/schema/` and `infra/platform/workflows/`
were: the listing IS the definition, and every reader derives it
independently (`registry::parse_raw_path` reads the directory in
file-name order; the ratchet lint and `timers-leave-a-packet.sh` glob
`*.toml`). Load order carries no meaning — matching is per event, and
the seed publishes by name.

## Adding a rule touches TWO places

1. **A FILE HERE** — `<rule-name>.toml`, one `[[rule]]`, named for the
   file, carrying its `why` (below). The loader refuses a file named for
   a rule it does not hold, a file holding two, and a rule with no `why`.
2. **`handler_emits()` in `crates/core/boss-dispatcher/src/cascade.rs`**,
   IF your `do` names a handler not already listed there. Its guard
   (`cascade_handlers_match_rules`) names the handler and the rule when
   you miss it; it is checking that every rule's cascade is declared, so
   a chain of rules cannot loop unseen. A Rust registration, not a text
   tail — two cars adding handlers do not conflict on it. This one is not
   a second copy of the rule: it declares what a HANDLER emits, which is
   a property of the handler's code, and no rule row states it.

It used to be three, and the third — a timestamped migration under
`infra/postgres/schema/` inserting the same row so a fresh database has
it — is gone, because the seed gives a fresh database the whole directory.
Twice on 2026-09-08 a builder learned that third place from a red gate
twenty minutes in. **It is now refused rather than merely unnecessary:**
`infra/lint/no-migration-writes-a-dispatcher-rule.sh` fails on any
migration written after the collapse that writes rule rows, because §9a is
explicit that asking the next person not to re-open a second home is not a
mechanism. The thirty-one that predate it are applied history and are left
alone.

The fourth place, before that, was `infra/lint/dispatcher-rules-ratchet.sh`,
whose hand-typed `BASELINE=60` every rule car had to bump. That integer
was the second half of a merge conflict, and it is gone too.
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
