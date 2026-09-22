//! Prose about `boss-expr` lives in places the evaluator does not
//! compile, and two of them were wrong at once.
//!
//! MEASURED (backlog dd61e914, filed by the builder of 151d1e04 on
//! 2026-09-19). 151d1e04 gave AND and OR a short-circuit, so a cheap
//! conjunct now buys the helper on its right. The reasoning block in
//! `infra/dispatcher/rules/refresh-publish-drift-daily.toml` still
//! said the expression language has no short-circuit, and that
//! sentence is the one a rule author reads while deciding whether a
//! guard is affordable. It is what the packet read: it asked for the
//! conjuncts of a rule that already carries them, and for a guard on
//! a SCHEDULED rule that `boss-dispatcher`'s own
//! `the_measurement_carries_no_dedup_guard` refuses by design.
//!
//! The module header had drifted the other way — it said "Two
//! consumers share this DSL today" while four crates depended on it
//! (`boss-jobs`, `boss-dispatcher`, `boss-views`, and
//! `boss-dispatcher-handlers` for `ops.judge`).
//!
//! CLAUDE.md §9a: collapse if you can, pin it if you cannot. The count
//! word is collapsed away — a list is its own count — and the list of
//! names is pinned here against the manifests that decide it, which is
//! the copy that cannot be edited into agreement by hand.

use boss_testing::repo_root;

/// The leading `//!` block of the DSL's own module docs — the text a
/// reader lands in from any of its consumers.
fn expr_header() -> String {
    let src = std::fs::read_to_string(repo_root().join("crates/core/boss-expr/src/lib.rs"))
        .expect("the boss-expr module source is readable");
    src.lines()
        .take_while(|l| l.starts_with("//!"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every workspace crate whose manifest takes a `boss-expr`
/// dependency, read from `crates/<tier>/<name>/Cargo.toml`.
///
/// The walk is deliberately two levels deep, so `boss-expr`'s own
/// nested `fuzz` crate is not counted: a fuzz harness for the DSL is
/// not a consumer of it.
fn crates_that_depend_on_expr() -> Vec<String> {
    let root = repo_root().join("crates");
    let mut found = Vec::new();
    for tier in std::fs::read_dir(&root).expect("the crates tree is readable") {
        let tier = tier.expect("a tier entry is readable").path();
        if !tier.is_dir() {
            continue;
        }
        for krate in std::fs::read_dir(&tier).expect("a tier is readable") {
            let krate = krate.expect("a crate entry is readable").path();
            let manifest = krate.join("Cargo.toml");
            if !manifest.is_file() {
                continue;
            }
            let name = krate
                .file_name()
                .expect("a crate directory has a name")
                .to_string_lossy()
                .to_string();
            if name == "boss-expr" {
                continue;
            }
            let text = std::fs::read_to_string(&manifest).expect("a crate manifest is readable");
            if text
                .lines()
                .any(|l| l.trim_start().starts_with("boss-expr ="))
            {
                found.push(name);
            }
        }
    }
    found.sort();
    found
}

#[test]
fn the_header_names_every_crate_that_depends_on_the_dsl() {
    let header = expr_header();
    let deps = crates_that_depend_on_expr();
    assert!(
        deps.len() >= 2,
        "the manifest walk found {deps:?} — it stopped finding consumers, so this test is \
         no longer measuring anything"
    );
    for krate in &deps {
        assert!(
            header.contains(krate.as_str()),
            "`{krate}` depends on boss-expr and the module header does not name it; the \
             header is what a reader of the DSL learns its blast radius from"
        );
    }
}

#[test]
fn the_header_does_not_carry_a_count_of_its_own_list() {
    let header = expr_header();
    for word in [
        "One consumer",
        "Two consumers",
        "Three consumers",
        "Four consumers",
        "Five consumers",
        // Not an ordinal but the same second copy: the header said
        // `both consumers` in its terminating-language paragraph while
        // the list below it named four (f1bfc954, 2026-09-22).
        "both consumers",
    ] {
        assert!(
            !header.contains(word),
            "the header says `{word}` — a spelled-out count is a second copy of the list \
             below it, and it is the copy that went stale (it said two while four crates \
             depended on the DSL). Let the list be the count."
        );
    }
}

#[test]
fn no_dispatcher_rule_claims_the_language_has_no_short_circuit() {
    let dir = repo_root().join("infra/dispatcher/rules");
    let mut offenders = Vec::new();
    let mut scanned = 0usize;
    for entry in std::fs::read_dir(&dir).expect("the rules directory is readable") {
        let path = entry.expect("a rule entry is readable").path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        scanned += 1;
        let text = std::fs::read_to_string(&path)
            .expect("a rule file is readable")
            .to_lowercase();
        if text.contains("no short-circuit") || text.contains("not short-circuit") {
            offenders.push(
                path.file_name()
                    .expect("a rule file has a name")
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }
    assert!(
        scanned > 20,
        "only {scanned} rule files were scanned — the directory moved and this test is \
         measuring nothing"
    );
    assert!(
        offenders.is_empty(),
        "these rules argue from a DSL that has no short-circuit: {offenders:?}. AND and OR \
         have short-circuited since 151d1e04, so a cheap conjunct now buys the helper on its \
         right — and this is the prose a rule author reads while deciding whether a guard is \
         affordable (backlog dd61e914)"
    );
}
