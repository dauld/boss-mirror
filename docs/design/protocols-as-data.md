# Design: protocols as data — closing the last leak into the substrate

**Status**: approved — open questions tracked at `/system/design` (2026-08-16).
**Origin**: David, 2026-08-15: "let's get those protocols prioritized
to be fully moved into data and registry configurations. That is
hunting leakage between the layers too." The phrase is CLAUDE.md's own:
*"a protocol that cannot be replaced without a deploy has leaked into
the substrate, and that leak is the defect to hunt."*
**Related**: [the-three-layers.md](./the-three-layers.md) ·
[design-docs-as-data.md](./design-docs-as-data.md) — the same move for
prose · `68331085` (the silent revert) · `b071994b`

---

## The leak, measured

Four registries carry the operating model. Three of them are seeded as
data and one is not:

| registry | seeded by | rows edited live? |
|---|---|---|
| `dispatcher_rules` | 11 SQL migrations | yes |
| `stations` | 5 SQL migrations | yes |
| `step_plugins` | 3 SQL migrations | yes |
| `workflows` | **`platform_workflows()` in Rust** | **no — reverted on next boot** |

Of 44 live workflows, **32 are already data** — the brewery's
`examples/brewery/seeds/workflows.toml` alone carries 25, loaded
through the public API like every other tenant seed. Only **12** are
code:

`ship-a-change` · `pr-train` · `user-feedback` · `design-doc-review` ·
`workflow-design` · `backlog-item` · `regenerate-deployment` · `sale` ·
`morning-brew` · `maintenance-backup` ·
`maintenance-audit-integrity` · `maintenance-ledger-replay`

So this is not a research project. The pattern is proven in-tree by 32
working examples; twelve rows have not been moved onto it.

## Why it bites, and it is not theoretical

`workflows.created_by` defaults to `'bootstrap'` and the ordinary
create path never binds it, so an API-published workflow is
bootstrap-owned. `bootstrap_reconcile` then republishes the code
default over any bootstrap-owned row whose `steps` differ. Two
protocol edits were silently reverted this way and went unnoticed for
a day (`68331085`).

The cost lands exactly where iteration matters most. Publishing
`approval` v2 took about a minute, because `approval` is not in
`platform_workflows()`. Publishing `user-feedback` v10 took a train
plus two prerequisite lint fixes, because it is. **The protocols we
change most often are the only ones we cannot change cheaply.**

## What "moved to data" means here

Not a new mechanism — the existing one, applied to twelve more rows.
Platform workflows become a seed bundle in-tree
(`infra/platform/workflows/`, beside `operator-baseline/`), loaded
at bootstrap through `POST /api/workflows` by a seed binary, exactly as
`boss-operator-baseline-seed` loads the operator hires and as the
tenant bundles load their 25.

`platform_workflows()` and its 26 `_spec()` builders are then deleted.
`registry.rs` is 5,439 lines and most of it is workflow literals; this
is the codebase shrinking, which is the point rather than a side
effect.

## Sequencing — lowest blast radius first

The twelve are not equal. `ship-a-change` and `pr-train` are what the
pipeline runs on: get those wrong and nothing else can ship, including
the fix. So the order is by traffic and by what breaks:

1. **Prove the path** — `workflow-design` (0 packets), `backlog-item`
   (1), `regenerate-deployment` (0). If the bundle loader is wrong,
   nothing is hurt.
2. **The idle maintenance three** — `maintenance-backup`,
   `maintenance-audit-integrity`, `maintenance-ledger-replay`. Timer-
   driven, no in-flight packets to strand.
3. **Tenant-shaped leftovers** — `sale`, `morning-brew`. These are
   brewery concepts sitting in core; they belong in the tenant bundle
   that already exists, not in a platform one (CLAUDE.md §10).
4. **The high-traffic platform trio** — `user-feedback` (133),
   `design-doc-review` (27), and only then `ship-a-change` (255) and
   `pr-train` (78).

Each step is one car. A car that moves a protocol changes no
behaviour: same spec, different home. The test is that the row reads
identically before and after, and that an operator edit now survives a
boot.

## Open questions

All 5 open questions were resolved 2026-08-18 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q4: What happens to in-flight packets during a conversion? (resolved)

Resolved 2026-08-18 — accept.

**The question was:**

> Proposed: nothing, and this is the part to verify rather than assume.
> Jobs pin the version they were admitted under, so moving `user-feedback`
> v10 from code to bundle must produce a row identical to the live v10 —
> not v11. If the loader publishes a new version instead of recognising
> the existing one, every in-flight packet keeps its old spec and the
> board grows a second lineage. The conversion car's test is a diff of
> the row before and after, byte for byte.
>
>
> Resolved 2026-08-16 — recovered from review Jobs 181ff742 and 68133a77, which recorded identical answers to this question.
>
> nothing, and this is the part to verify rather than assume. Jobs pin the version they were admitted under, so moving `user-feedback` v10 from code to bundle must produce a row identical to the live v10 — not v11. If the loader publishes a new version instead of recognising the existing one, every in-flight packet keeps its old spec and the board grows a second lineage. The conversion car's test is a diff of the row before and after, byte for byte.

nothing, and this is the part to verify rather than assume. Jobs pin the version they were admitted under, so moving `user-feedback` v10 from code to bundle must produce a row identical to the live v10 — not v11. If the loader publishes a new version instead of recognising the existing one, every in-flight packet keeps its old spec and the board grows a second lineage. The conversion car's test is a diff of the row before and after, byte for byte.


### Q1: What replaces `bootstrap_reconcile` for a fresh deployment? (resolved)

Resolved 2026-08-18 — accept.

**The question was:**

> Proposed: the seed binary inserts what is missing and touches nothing
> that exists — the same idempotent posture
> `boss-operator-baseline-seed` already has (409 on duplicate = "already
> there, skip"). Drift-healing goes away deliberately: it is the feature
> that reverts operator edits, and once the spec is data there is no
> "code default" for a row to have drifted from.
>
> The thing genuinely lost is the guarantee that every deployment runs
> the same platform protocols. That was never really true — an operator
> could always publish a new version — and the honest replacement is a
> CHECK rather than a rewrite: a boot-time report of platform kinds
> whose active version differs from the shipped bundle, visible and not
> self-healing.
>
>
> Resolved 2026-08-16 — recovered from review Jobs 181ff742 and 68133a77, which recorded identical answers to this question.
>
> the seed binary inserts what is missing and touches nothing that exists — the same idempotent posture `boss-operator-baseline-seed` already has (409 on duplicate = "already there, skip"). Drift-healing goes away deliberately: it is the feature that reverts operator edits, and once the spec is data there is no "code default" for a row to have drifted from.

the seed binary inserts what is missing and touches nothing that exists — the same idempotent posture `boss-operator-baseline-seed` already has (409 on duplicate = "already there, skip"). Drift-healing goes away deliberately: it is the feature that reverts operator edits, and once the spec is data there is no "code default" for a row to have drifted from.


### Q2: Does the viability lint still run at the right moment? (resolved)

Resolved 2026-08-18 — accept.

**The question was:**

> Proposed: yes, and in more places. `gate_active` already runs on every
> publish path including `publish_authored`, so a bundle loaded through
> the API is linted per row at insert. What is lost is the compile-time
> `every_shipped_platform_seed_is_viable` test. The replacement is a
> test that parses the BUNDLE and lints it — same assertion, one
> indirection, and it catches a malformed TOML the compiler never saw.
>
>
> Resolved 2026-08-16 — recovered from review Jobs 181ff742 and 68133a77, which recorded identical answers to this question.
>
> yes, and in more places. `gate_active` already runs on every publish path including `publish_authored`, so a bundle loaded through the API is linted per row at insert. What is lost is the compile-time `every_shipped_platform_seed_is_viable` test. The replacement is a test that parses the BUNDLE and lints it — same assertion, one indirection, and it catches a malformed TOML the compiler never saw.

yes, and in more places. `gate_active` already runs on every publish path including `publish_authored`, so a bundle loaded through the API is linted per row at insert. What is lost is the compile-time `every_shipped_platform_seed_is_viable` test. The replacement is a test that parses the BUNDLE and lints it — same assertion, one indirection, and it catches a malformed TOML the compiler never saw.


### Q3: TOML bundle, or SQL migrations like the other three registries? (resolved)

Resolved 2026-08-18 — accept.

**The question was:**

> The other three registries seed by migration, which argues for
> consistency. Against it: a workflow spec is a large nested document,
> and a `steps` array as a SQL string literal is unreadable and
> unreviewable — the reason the tenant bundles are TOML.
>
> Proposed: TOML through the API, and accept the inconsistency with the
> other three, because the shape of the data differs by an order of
> magnitude. Worth stating the rule so it is not read as drift: a
> registry whose rows are FLAT seeds by migration; a registry whose rows
> are DOCUMENTS seeds by bundle.
>
>
> Resolved 2026-08-16 — recovered from review Jobs 181ff742 and 68133a77, which recorded identical answers to this question.
>
> TOML through the API, and accept the inconsistency with the other three, because the shape of the data differs by an order of magnitude. Worth stating the rule so it is not read as drift: a registry whose rows are FLAT seeds by migration; a registry whose rows are DOCUMENTS seeds by bundle.

TOML through the API, and accept the inconsistency with the other three, because the shape of the data differs by an order of magnitude. Worth stating the rule so it is not read as drift: a registry whose rows are FLAT seeds by migration; a registry whose rows are DOCUMENTS seeds by bundle.


### Q5: Do `sale` and `morning-brew` move to the tenant, or just to data? (resolved)

Resolved 2026-08-18 — accept.

**The question was:**

> Resolved 2026-08-16 — override. Recovered from review Job 68133a77 and
> confirmed by David: *"Q5 should be B, I was just a little lazy the second
> time."*
>
> These are just data protocols now, but we can put a seed version into tenant if we want. But this shouldn't be important.
>
> David's answer is right, and for a better reason than either recorded
> answer gave: **the leak it worried about is already closed.** Checked
> 2026-08-16 rather than assumed — `platform_workflows()` holds seven
> kinds and every one is platform-shaped (three maintenance chores plus
> design-doc-review, user-feedback, ship-a-change, pr-train). `sale` and
> `morning-brew` appear in core only as strings in test fixtures; the
> brewery kinds seed from `examples/brewery/`, which is where they
> belong. The superseded answer's premise — "they are brewery nouns
> living in core" — was stale.
>
> So there is nothing to move, which is why "this shouldn't be
> important" is the correct call and not merely a relaxed one.
>
> The rule itself is not softened by that. David, 2026-08-16: *"We don't
> want brewery nouns in core no matter what. But most nouns should be
> data anyway, so that was its own class of problem."* Two problems were
> tangled here and separating them is the lasting part. One is a LAYER
> question — tenant nouns in core — which is never acceptable and is
> currently true only by vigilance: nothing asserts that
> `platform_workflows()` stays free of them. The other is a FORM
> question — nouns hardcoded as Rust literals at all — which is what
> this doc is about, and solving it shrinks the first without removing
> it. A kind in a bundle is editable without a deploy wherever the
> bundle lives, so once protocols are data the layer question is about
> which bundle holds a row, not about a deploy.

to the tenant. They are brewery nouns in core, which is the Tier-1/Tier-3 leak CLAUDE.md §10 names, and the brewery bundle that should hold them already exists with 25 siblings. Moving them to a PLATFORM bundle would fix the layer leak this doc is about while leaving a different one in place.
