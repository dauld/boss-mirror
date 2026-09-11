//! `boss-leaked-policy` — the AST half of CLAUDE.md §9.
//!
//! `infra/codebase-metrics.sh` counts the REGISTRY half of §9 exactly
//! (239 rows across five registries on 2026-09-11) and recorded the
//! other half as `null` with a written reason, because its builder
//! judged a regex count dishonest:
//!
//! > A string-literal `match` arm is equally a leaked workflow policy,
//! > the dispatcher's own handler table (which EXECUTES the registry), a
//! > serde round-trip, or an HTTP route — and arms span lines and nest.
//! > Any regex number would move for reasons unrelated to the goal and
//! > then get quoted.
//!
//! This is the pass that replaces the `null`. THE CLASSIFICATION RULE IS
//! THE DELIVERABLE; the integer is a consequence of it. So the cases
//! below are not invented shapes — every one is COPIED from the tree it
//! measures, file and line named, and the assertion is the reading a
//! reviewer gave that code by hand. A rule that disagrees with a
//! reviewer is wrong about the rule, not about the code, and this file
//! is where that disagreement has to show up.
//!
//! The two gates, in the order that defines them (`leaked_policy.rs`
//! states the full ladder):
//!
//!   1. the scrutinee NAMES A KIND — the trailing identifier is `kind`
//!      or ends `_kind`. That is literally §9's `match kind { … }`.
//!   2. at least one arm literal IS a registry-declared kind, read LIVE
//!      from the registry seeds in the measured tree — so the counter
//!      cannot drift from the registries (CLAUDE.md §9a). No hand-kept
//!      vocabulary list anywhere in this code.
//!
//! Gate 1 alone is what excuses the MIME table and the JSON-Schema
//! primitive check; gate 2 alone is what excuses the rebuilders' fold
//! over event kinds. Neither gate alone is enough, which is the whole
//! reason a regex cannot do this.

use boss_testing::leaked_policy::{Class, Vocabulary, classify_source};

/// The vocabulary a fixture matches against. Real runs read this from
/// the measured tree's registry seeds; the tests state it inline so the
/// assertion is about the RULE and not about today's registry contents.
fn vocab() -> Vocabulary {
    Vocabulary::from_kinds([
        "sign-off",
        "workflow-publish",
        "account",
        "employee",
        "refurb-used",
        "sale",
        "html",
        "scheduling",
    ])
}

/// Classify one snippet, asserting it produced exactly one match
/// expression — a fixture that accidentally contains two is a fixture
/// whose assertion means something else.
fn one(src: &str) -> Class {
    let sites = classify_source("fixture.rs", src, &vocab()).expect("the fixture parses");
    assert_eq!(
        sites.len(),
        1,
        "the fixture must hold exactly one string-literal match, got {sites:#?}"
    );
    sites[0].class
}

// ---------------------------------------------------------------------
// 1. ROUND TRIP — a string and a closed Rust enum, the inverse of
//    `as_str`. Copied from crates/core/boss-jobs/src/scheduling/types.rs
//    (the single largest count in the tree: 20 arms in one file, every
//    one of them this shape).
// ---------------------------------------------------------------------
#[test]
fn a_string_to_enum_conversion_is_a_round_trip_not_a_leak() {
    let src = r#"
        impl AvailabilityKind {
            pub fn parse(s: &str) -> Option<Self> {
                Some(match s {
                    "available" => Self::Available,
                    "pto" => Self::Pto,
                    "sick" => Self::Sick,
                    _ => return None,
                })
            }
        }
    "#;
    assert_eq!(one(src), Class::RoundTrip);
}

#[test]
fn a_from_str_wrapping_its_variants_in_ok_is_still_a_round_trip() {
    // crates/core/boss-jobs/src/stations.rs:64 — and note "sign-off"
    // appears in a sibling `FromStr` (policy `Action`) where it is a
    // POLICY ACTION that happens to be spelled like a step kind. Order
    // is what keeps that coincidence out of the leaked count.
    let src = r#"
        impl std::str::FromStr for Action {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    "read" => Ok(Self::Read),
                    "sign-off" => Ok(Self::SignOff),
                    _ => Err(format!("unknown action {s}")),
                }
            }
        }
    "#;
    assert_eq!(one(src), Class::RoundTrip);
}

// ---------------------------------------------------------------------
// 2. HANDLER TABLE — the thing that EXECUTES registry rows. Deleting it
//    would mean the registry does nothing, so it is the opposite of a
//    leak. Copied from
//    crates/core/boss-dispatcher/src/rules/helpers_inventory.rs:77.
// ---------------------------------------------------------------------
#[test]
fn the_resolver_that_executes_registry_rows_is_not_a_leak() {
    let src = r#"
        impl HelperResolver for InventoryHelpers {
            fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
                match name {
                    "open_po_exists" => open_po_exists(self, args),
                    "vendor_for" => vendor_for(self, args),
                    "open_job_exists" => open_job_exists(self, args),
                    _ => Err(EvalError::UnknownHelper(name.to_string())),
                }
            }
        }
    "#;
    assert_eq!(one(src), Class::HandlerTable);
}

// ---------------------------------------------------------------------
// 3. EVENT FOLD — a projection's fold over the log. The scrutinee DOES
//    name a kind (gate 1 passes), and the rebuilder is still right:
//    event kinds are the log's vocabulary, not a registry's. Gate 2 is
//    what separates them. Copied from
//    crates/core/boss-jobs/src/rebuild.rs:70.
// ---------------------------------------------------------------------
#[test]
fn a_rebuilder_folding_over_event_kinds_is_not_a_leak() {
    let src = r#"
        fn apply(ev: &Event, state: &mut State) {
            match ev.kind.as_str() {
                "jobs.job.created" => state.open += 1,
                "jobs.job.closed" => state.open -= 1,
                _ => {}
            }
        }
    "#;
    assert_eq!(one(src), Class::EventFold);
}

// ---------------------------------------------------------------------
// 4. EXTERNAL VOCABULARY — a table over names nothing in BOSS owns.
//    Both of these are why gate 1 exists, and the MIME one is why a
//    literal-membership test alone would be wrong: "html" is BOTH a
//    file extension and a registry-declared kind in this tree.
// ---------------------------------------------------------------------
#[test]
fn a_mime_table_is_an_external_vocabulary_even_when_a_literal_collides() {
    // crates/core/boss-gateway/src/static_files.rs:182.
    let src = r#"
        fn guess_content_type(path: &Path) -> HeaderValue {
            let ct = match ext {
                "html" => "text/html; charset=utf-8",
                "js" => "application/javascript; charset=utf-8",
                "png" => "image/png",
                _ => "application/octet-stream",
            };
            HeaderValue::from_static(ct)
        }
    "#;
    assert_eq!(
        one(src),
        Class::ExternalVocabulary,
        "\"html\" is a registry-declared kind AND a file extension. A count \
         keyed on literal membership alone calls this leaked policy, and a \
         reviewer never would — which is why the scrutinee gate comes first."
    );
}

#[test]
fn a_json_schema_primitive_check_is_an_external_vocabulary() {
    // crates/core/boss-jobs/src/step_registry.rs:344 and its twin in
    // workflow_lint.rs:992 — the two largest non-round-trip counts.
    let src = r#"
        fn validate_field_type(expected: &str, value: &Value) -> bool {
            match expected {
                "string" => value.is_string(),
                "number" => value.is_number(),
                "date" => value.as_str().is_some_and(|s| s.len() == 10),
                _ => true,
            }
        }
    "#;
    assert_eq!(one(src), Class::ExternalVocabulary);
}

// ---------------------------------------------------------------------
// 5. LEAKED POLICY — both gates. These are the number.
// ---------------------------------------------------------------------
#[test]
fn a_branch_on_a_step_kind_is_leaked_policy() {
    // crates/core/boss-jobs/src/bootstrap.rs:406 — core code that must
    // be edited to teach the bootstrap a new step kind.
    let src = r#"
        fn completion_body(step_kind: &str, step: &Value) -> Map<String, Value> {
            match step_kind {
                "sign-off" => {
                    out.insert("authority_role".into(), json!("workflow-approver"));
                    out
                }
                "workflow-publish" => {
                    out.insert("spec_slug".into(), json!(slug));
                    out
                }
                _ => out,
            }
        }
    "#;
    let sites = classify_source("fixture.rs", src, &vocab()).expect("parses");
    assert_eq!(sites[0].class, Class::LeakedPolicy);
    assert_eq!(sites[0].scrutinee, "step_kind");
    assert_eq!(
        sites[0].registry_literals,
        vec!["sign-off".to_string(), "workflow-publish".to_string()],
        "the site names WHICH registry kinds it branched on, so the number \
         can be audited without re-running the pass"
    );
}

#[test]
fn a_branch_on_a_subject_kind_is_leaked_policy() {
    // crates/core/boss-jobs/src/policy_glue.rs:48 — territory scope
    // decided by a subject-kind name in core policy glue.
    let src = r#"
        fn territory_matches(user: &User, subject: &impl Subject) -> bool {
            match subject.kind() {
                "account" | "employee" => {
                    let target = subject.id();
                    user.territory_account_ids.iter().any(|p| p == target)
                }
                _ => false,
            }
        }
    "#;
    let sites = classify_source("fixture.rs", src, &vocab()).expect("parses");
    assert_eq!(sites[0].class, Class::LeakedPolicy);
    assert_eq!(
        sites[0].scrutinee, "kind",
        "`subject.kind()` names a kind as plainly as a `kind` binding does"
    );
}

#[test]
fn a_label_table_keyed_on_a_registry_kind_is_leaked_policy_too() {
    // Not a behaviour branch — a display name. It is still core code
    // that must be edited to add a registry row, which is the whole of
    // §9's complaint, so it is not given its own softer bucket.
    let src = r#"
        fn label(kind: &str) -> &'static str {
            match kind {
                "refurb-used" => "Refurbish (used)",
                "sale" => "Sale",
                _ => "Job",
            }
        }
    "#;
    assert_eq!(one(src), Class::LeakedPolicy);
}

// ---------------------------------------------------------------------
// 6. UNCLASSIFIED — reported as its own number with its own reason,
//    never forced into a bucket. A count with `unclassified: 7` is
//    usable; a count that silently guesses is the thing this pass
//    exists to avoid.
// ---------------------------------------------------------------------
#[test]
fn a_kind_scrutinee_over_an_unknown_vocabulary_is_unclassified() {
    let src = r#"
        fn route(kind: &str) -> &'static str {
            match kind {
                "something-no-registry-declares" => "a",
                _ => "b",
            }
        }
    "#;
    assert_eq!(one(src), Class::UnclassifiedKindUnknownVocabulary);
}

#[test]
fn a_registry_kind_under_a_scrutinee_that_does_not_name_one_is_unclassified() {
    // A leak can hide behind a variable name. Saying so is cheaper than
    // either guessing, and it is the bucket to go read by hand.
    let src = r#"
        fn pick(s: &str) -> u8 {
            match s {
                "refurb-used" => {
                    do_one_thing();
                    1
                }
                _ => 0,
            }
        }
    "#;
    assert_eq!(one(src), Class::UnclassifiedRegistryLiteralOffKind);
}

// ---------------------------------------------------------------------
// Scope, conservation, and the fixture-free run over the real tree.
// ---------------------------------------------------------------------

#[test]
fn test_code_is_not_measured() {
    // `no-step-kind-match.sh` exempts tests for the same reason: a test
    // PINS a fixture, it does not decide behaviour. An inline
    // `#[cfg(test)]` module is most of this repo's Rust tests, so a pass
    // that only skipped `tests/` directories would measure them.
    let src = r#"
        #[cfg(test)]
        mod tests {
            fn helper(step_kind: &str) -> u8 {
                match step_kind {
                    "sign-off" => 1,
                    _ => 0,
                }
            }
        }
    "#;
    let sites = classify_source("fixture.rs", src, &vocab()).expect("parses");
    assert!(
        sites.is_empty(),
        "an inline #[cfg(test)] module is test code: {sites:#?}"
    );
}

#[test]
fn a_match_with_no_string_patterns_is_not_a_site_at_all() {
    let src = r#"
        fn label(kind: AssignmentKind) -> &'static str {
            match kind {
                AssignmentKind::Wo => "Service call",
                AssignmentKind::Travel => "Travel",
            }
        }
    "#;
    let sites = classify_source("fixture.rs", src, &vocab()).expect("parses");
    assert!(
        sites.is_empty(),
        "matching on an ENUM and yielding strings is the as_str direction — \
         there is no string pattern here at all: {sites:#?}"
    );
}

#[test]
fn the_real_tree_classifies_every_site_into_exactly_one_bucket() {
    // CONSERVATION, the property rather than today's integer. A
    // threshold here would red a future car for doing nothing wrong; a
    // conservation law cannot. The integer itself rides on the daily
    // packet, which is where a number that moves belongs.
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves");
    let report = boss_testing::leaked_policy::scan(&repo, &["crates/core"])
        .expect("the real tree is parseable and carries a registry vocabulary");

    assert!(
        report.vocabulary_kinds > 50,
        "the vocabulary is read from the measured tree's registry seeds; {} \
         kinds is not a tree with five registries in it",
        report.vocabulary_kinds
    );
    assert!(
        report.matches > 50,
        "{} string-literal matches",
        report.matches
    );
    assert_eq!(
        report.by_class.values().sum::<usize>(),
        report.matches,
        "every site lands in exactly one bucket: {:#?}",
        report.by_class
    );
    assert_eq!(
        report.unparsable_files, 0,
        "a file the pass could not parse makes the count short by an unknown \
         amount, so the run must refuse rather than report"
    );
    // Every leaked site names the kinds it branched on — the audit trail
    // that makes the number checkable by hand.
    for site in &report.sites {
        if site.class == Class::LeakedPolicy {
            assert!(
                !site.registry_literals.is_empty(),
                "a leaked site with no registry literal cannot have passed gate 2: {site:#?}"
            );
        }
    }
}
