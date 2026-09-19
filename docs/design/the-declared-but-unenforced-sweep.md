# The declared-but-unenforced sweep

*2026-09-19, backlog a2358e7c. The residue of one hunt, not a standing
process — kept because the METHOD is reusable and the discarded
candidates are worth as much as the findings.*

## The class

Something is written down in a place readers trust — a registry field, a
rules document, a column, a probe's prose, a comment — and no mechanism
makes it true. None of them fails loudly. Each reads as fact to the next
person. It is CLAUDE.md's mostly-sure / absolutely-sure distinction
appearing as a code smell.

Nine instances were found on 2026-09-19 without anyone looking for them
(`e720dd00`, `8d054cb2`, `9843aeb9`, `a92571a6`, `65c9c05a`, `8f1de7bf`,
`ff56cc00`, `0b36dd65`, and this packet's own framing). This document is
the deliberate sweep that followed, and its rule is the packet's: every
instance gets ONE of two verdicts — **enforce it** or **stop claiming
it**. Recording the claim more carefully is not a third route; it is what
makes the class invisible, and §F1 below is that route caught in the act.

## Method — what actually paid

In rough order of yield per minute:

1. **Diff a registry's TOML key set against its serde struct.** Parse
   every bundle file, collect the keys, subtract the struct's fields.
   Anything left is a declaration nothing reads. This found F1 and F2.
2. **Ask a script for its own number and compare it to the prose.**
   `gate.sh --exclusions | wc -l`, `tier-import-audit.sh`'s scanned
   count. Counts in documents are unpinned by construction. F4, F5.
3. **Grep every named mechanism in CLAUDE.md and builder-rules.md for
   its file.** Cheap, and it mostly *cleared* claims — see Discarded.
4. **Columns never written**: extract every column from
   `infra/postgres/schema/` and count referencing files outside it.
   30 columns had ≤2 references; all were either dead residue of a
   deleted system or `DEFAULT NOW()` bookkeeping. Nothing of this class.
   Low yield: a column that asserts nothing cannot assert something
   false.

## Findings

### F1 — `owning_team` in the Workflow bundle was decorative — ENFORCED

64 `[[workflow]]` rows declared `owning_team`. `WorkflowToml`
(`crates/core/boss-jobs/src/seed_loader.rs`) had no such field and no
`deny_unknown_fields`, so serde dropped it, and `workflow_toml_to_spec`
ended `spec.owning_team = default_owner.to_string()` regardless. Five
rows were measurably false: `incident`, `doc-flatten` and
`protocol-retro` said `"it"` while every live row says `"platform"`;
`keg-return` said `"distribution"` and `tasting-panel` said `"brewing"`
while the brewery loader stamps the tenant id.

What makes this the exemplar is that it was already known.
`infra/lint/the-live-protocols-are-the-authored-protocols.sh` took
`owning_team` OFF its comparison list and wrote the reason down: *"the
file's key is decorative … a file claiming something the loader will not
honour, and NO publish could ever clear it."* Correct, well argued, and
the wrong route: the comparator stopped looking and the 64 files went on
claiming. A finding no action can close is supposed to be a reason to
close it, not a reason to stop measuring it.

**Verdict: enforce it.** `owning_team` is now a read field, and a value
the loader will not honour is refused by kind naming both values. The
five disagreeing declarations were corrected or dropped.

### F2 — the Workflow bundle's key set was open — ENFORCED

Every sibling bundle struct in the same file — `StationToml`,
`StepPluginToml`, `CadenceRuleToml`, `DeliveryPolicyToml` — carries
`#[serde(deny_unknown_fields)]`. `WorkflowToml`, `StepToml` and
`TerminalToml` did not. That is the hole F1 came through, and a typo'd
`ready_when`, `authority_role` or `terminal` had the same silence
available to it, in the registry that is supposed to be where a new work
type lands instead of a code path.

**Verdict: enforce it.** All three now close their key set.

### F3 — the dispatcher rule registry's key set is open — OPEN

`RawRule`, `RawSchedule` and `RawDoStep`
(`crates/core/boss-dispatcher/src/rules/registry.rs`) carry no
`deny_unknown_fields`, over 58 rule files. All 58 were scanned: **no
unknown key today**, so this is a latent hole and not a live false
claim — which is why it is a car of its own rather than a line in this
one. Closing it needs `why` added to `RawRule` first: the key is required
of every rule file and is parsed today by a separate `RuleFileMeta`,
passing through `RawRule` only because the set is open.

**Verdict: enforce it.**

### F4 — CLAUDE.md §9a's "four-entry exclusion set" — OPEN

> *"the roster is the directory minus a four-entry exclusion set"*

Measured 2026-09-19: `bash infra/gate.sh --exclusions` prints **five**;
`--roster` prints 83; `ls infra/lint/*.sh` is 88. And the shape the
sentence describes stopped existing on 2026-09-18 — there is no
exclusion set in `gate.sh` any more. Each lint declares its own
`# consist: skip — <reason>` header line and `consist_exclusions()`
reads them, which is a better mechanism than the sentence credits.

Nothing pins a count in a document to the directory it describes, and
nothing ever will; the repair is to stop stating one.

**Verdict: stop claiming it.** The sentence a directory cannot falsify
is "minus the lints that declare `# consist: skip` in their own header".

### F5 — CLAUDE.md's "0 violations across 28 core crates" — OPEN

`bash infra/lint/tier-import-audit.sh` answers *"scanned 29 core
crate(s)"*. `ls -d crates/core/*/` is 28. The lint's `crate_tomls()` is a
bare `find … -name Cargo.toml`, so it counts
`crates/core/boss-expr/fuzz/Cargo.toml` as a core crate.

Two claims, one number, and they already disagree. The scanned figure is
not decoration: `lint_scanned` exists so that a lint which looked at
nothing cannot report clean, so it should count what it says it counts.

**Verdict: both routes, one each.** Drop the count from CLAUDE.md — the
claim worth making is "runs cleanly", which the gate holds on every run.
And exclude nested `*/fuzz/` from `crate_tomls()` so the scanned figure
answers the question it names.

## Discarded — checked, and the claim holds

A wrong finding in a list like this is expensive, because the whole point
is that these read as fact. Each of the following was suspected, measured
and cleared; recording them is what stops the next sweep re-deriving them.

| claim | held by |
|---|---|
| builder-rules 12: "the no-producer-coin pin refuses it" | `a_lint_that_cannot_read_does_not_say_clean.rs::no_shell_under_infra_pipes_a_producer_into_an_early_exiting_reader` — every shell under `infra/`, on lines inside a `pipefail` window, exactly as the rule words it |
| builder-rules 3: "never a fixed /tmp path, never your own repo-root helper (a lint refuses both)" | `infra/lint/a-fixture-path-cannot-be-a-literal.sh` and its test refuse both shapes |
| CLAUDE.md: the four pod doors are "pinned by a shell test" | `boss_api_sh`, `boss_shim_sh`, `wt_cargo_sh`, `wt_web_sh` all exist; all four `/work/tools/bin` entries are symlinks into the main checkout (the checkout's own freshness is `0b36dd65`, still open) |
| CLAUDE.md: `infra/dev/sor-url` "held equal to it by a test" | `the_estate_address_lives_once.rs` renders `--dev-sor-url` from the source and asserts equality |
| CLAUDE.md: the lint "refuses either IP anywhere else" | both addresses are read FROM the source, every tracked file is scanned, and the allowance is a named set that goes stale loudly |
| CLAUDE.md §9a: the rule count is derived, and `why` is required | `dispatcher-rules-ratchet.sh` derives the count from the directory and refuses an empty `why`; `parse_raw_dir` refuses one too |
| CLAUDE.md §9a: migrations are `schema/*.sql` by numeric prefix | no `manifest.txt` exists; `a-migration-prefix-carries-seconds.sh` holds the width against a closed set of six named legacy files |
| CLAUDE.md §Doors: `regate_receipt`, `is_train_gate`, `operator:unidentified`, `automation:train-conductor` | each is a real constant or predicate with callers in every surface the paragraph names |
| the dispatch prompt's "the claim door reserved this much of the agent's hourly budget" | `boss-jobs::agent_budget` reserves before the CAS: `spent + budget <= cap`, refused with all three numbers |

Two near-misses worth naming so they are not re-found as this class:
`node_roles.declared_at` is a `DEFAULT NOW()` column nothing reads, and
`infra/step-plugins/review-design.js` still has defensive reads of
`design_docs.content_html` / `word_count` / `body_md` for tables dropped
by migration on 2026-09-10. Both are residue. Neither asserts anything,
and a column that asserts nothing cannot assert something false — that
is the line between this class and ordinary dead code.
