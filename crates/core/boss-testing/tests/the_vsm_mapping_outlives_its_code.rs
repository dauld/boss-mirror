//! The VSM mapping must be in the record before the only code that
//! speaks VSM leaves the tree.
//!
//! WHY THIS IS A TEST AND NOT A NOTE ON A PACKET. Backlog 6872efd4
//! states the requirement as an ORDERING: write the mapping BEFORE the
//! retirement cars land, "so there is never a window where the
//! vocabulary is in neither the code nor the record". An ordering
//! between two cars that no mechanism holds is a thing someone has to
//! remember across days and across whoever picks the second car up —
//! and the retirement car is the one with the momentum behind it,
//! since deleting 2,586 lines is the visible win.
//!
//! So the ordering is enforced from the side that can be checked: if
//! `boss-cybernetics` is gone from the tree, the decision record must
//! carry the mapping. The retirement car cannot land alone; it lands
//! either after this one or with it.
//!
//! Stafford Beer is the namesake and CLAUDE.md puts cybernetics first
//! among the three founding lineages. Retiring the only S2
//! implementation without recording what plays S2 is how a founding
//! idea quietly becomes decoration — which is the sharper version of
//! the declared-but-unenforced class the a2358e7c sweep hunts.

use boss_testing::repo_root;

const MAPPING_HEADING: &str = "### The Viable System Model, mapped";

fn decision_record() -> String {
    std::fs::read_to_string(repo_root().join("docs/architecture-decisions.md"))
        .expect("the decision record is readable")
}

#[test]
fn the_record_carries_the_mapping() {
    let text = decision_record();
    assert!(
        text.contains(MAPPING_HEADING),
        "docs/architecture-decisions.md must carry the VSM mapping \
         (heading: {MAPPING_HEADING})"
    );
}

#[test]
fn the_mapping_names_each_level_it_claims_to_map() {
    let text = decision_record();
    let from = text
        .find(MAPPING_HEADING)
        .expect("the mapping section exists");
    // Bounded to the section itself: S1 appears elsewhere in a 2,000
    // line document, so a whole-file search would pass on prose that
    // has nothing to do with this.
    let section = &text[from..];
    let end = section[MAPPING_HEADING.len()..]
        .find("\n## ")
        .map(|i| i + MAPPING_HEADING.len())
        .unwrap_or(section.len());
    let section = &section[..end];
    for level in ["**S1**", "**S2**", "**S3**"] {
        assert!(
            section.contains(level),
            "the mapping must say what plays {level}; a section that \
             names a level without naming its BOSS concept is the \
             decoration this is meant to prevent"
        );
    }
    // The gap is load-bearing and must not be quietly dropped: four of
    // the five responsibilities have successors and the inbox does not.
    assert!(
        section.contains("station"),
        "the mapping must name the station as S2's mechanism and as the \
         successor the durable inbox is waiting on (design 8382bbb2)"
    );
}

/// The ordering rule itself, as a function so both arms can be proved.
/// While the crate exists the vocabulary is in the code and the record
/// is belt-and-braces; once it is gone the record is the only copy.
fn mapping_is_required(vsm_code_exists: bool) -> bool {
    !vsm_code_exists
}

#[test]
fn the_rule_demands_the_record_exactly_when_the_code_is_gone() {
    // Proved both ways, because the arm that matters cannot be
    // exercised while boss-cybernetics is still in the tree — and a
    // latch nobody has seen fire is the class this repo keeps filing.
    assert!(
        mapping_is_required(false),
        "with the VSM code gone, the record must carry the mapping"
    );
    assert!(
        !mapping_is_required(true),
        "while the code carries the vocabulary the record is not the only copy"
    );
}

#[test]
fn the_ordering_holds_against_the_tree_as_it_stands() {
    // THE ORDERING, enforced. This is the live application of the rule
    // above: whatever the tree currently holds, the two must not both
    // be absent.
    let vsm_code = repo_root().join("crates/core/boss-cybernetics");
    if mapping_is_required(vsm_code.exists()) {
        assert!(
            decision_record().contains(MAPPING_HEADING),
            "boss-cybernetics has been retired and the decision record \
             carries no VSM mapping — the vocabulary is now in neither \
             the code nor the record, which is exactly the window \
             backlog 6872efd4 exists to close"
        );
    }
}
