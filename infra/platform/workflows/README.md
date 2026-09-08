# Platform Workflow bundle — protocols as DATA, not Rust literals

One file per protocol: `<kind>.toml` holds exactly one `[[workflow]]`
whose `kind` is the file name. **Adding a protocol is dropping a file
in.** Nothing is appended anywhere; two cars adding kinds touch no
shared line. Pin a kind's decided shape in its own test file,
`crates/core/boss-jobs/tests/platform_bundle_<kind>.rs`, for the same
reason.

This directory was one file (`workflows.toml`) until 2026-09-08, when
two protocol cars parked in one day and the second was left behind on
the tail conflict — the exact shape CLAUDE.md §9a records for
`manifest.txt`, and collapsed the same way `infra/postgres/schema/`
was: the listing is the definition, every reader derives it
independently (`seed_loader::load_workflows` reads the directory in
file-name order; `timers-leave-a-packet.sh` greps `*.toml`; `boss
workflow publish <kind> infra/platform/workflows` or
`.../<kind>.toml` names the same row). Load order carries no meaning:
the seed is insert-if-missing per kind and the viability lint is
per-spec. The loader refuses a file named for a kind it does not
hold, a file holding two, and an empty directory.

David, 2026-08-15: "get those protocols prioritized to be fully moved
into data and registry configurations. That is hunting leakage between
the layers too" — CLAUDE.md's own test: "a protocol that cannot be
replaced without a deploy has leaked into the substrate".

Three of the four registries carrying the operating model already seed
as data (dispatcher_rules, stations, step_plugins). `workflows` did
not: twelve kinds were Rust literals in `platform_workflows()`, and an
API publish of one is REVERTED on the next boot because
bootstrap_reconcile republishes the code default (68331085).

Parsed by `boss_jobs::seed_loader::load_workflows`, the same reader the
brewery's 25-row bundle uses. Authored rather than generated on
purpose: `WorkflowSpec` does NOT serialize to TOML — TOML has no null,
so every `None` field fails with UnsupportedType(unit). The fidelity
guarantee therefore runs the other way, and it is not optional:
`the_platform_bundle_matches_the_specs_it_replaced` (registry.rs)
parses this directory and asserts each converted row equals the spec
it replaces, field for field; `platform_bundle.rs` runs the viability
lint over every file and pins the one-kind-per-file rule.

That test is David's answer to protocols-as-data Q4 made mechanical:
"moving user-feedback v10 from code to bundle must produce a row
identical to the live v10 — not v11. If the loader publishes a new
version instead of recognising the existing one, every in-flight
packet keeps its old spec and the board grows a second lineage."

Seeding is insert-if-missing (`boss-platform-workflow-seed`): a fresh
database gets every kind here at v1; a live one keeps whatever it has,
and a new version goes live through `boss workflow publish`, which
reads the same file — one definition.
