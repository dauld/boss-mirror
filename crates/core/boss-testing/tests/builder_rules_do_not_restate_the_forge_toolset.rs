//! The builder rules must not carry their own copy of "what the forge
//! host has", because there is already an authoritative one and they
//! drifted apart.
//!
//! MEASURED (backlog 785dc91a, found by the builder of a92571a6 on
//! 2026-09-19). `infra/platform/documents/builder-rules.md` — the
//! probe-rules paragraph every builder reads while deciding what their
//! probe may call — said `Not available: boss, kubectl, boss-api,
//! cargo`. `boss` had been available on the forge since 2026-09-18:
//! H8 car 1 (backlog 9f00a805) made that host's own converge install
//! the CLI from the cluster image on every tick, and
//! `infra/forge/host-absent-tools.txt` took `boss` off its list the
//! same day the proof landed, with the retirement recorded in place.
//!
//! So one document said a tool was absent while the file the GATE
//! consults said it was present. A builder reading the rules wrote
//! around a tool that was there; a builder trusting the gate did not.
//!
//! CLAUDE.md §9a: collapse if you can. The absent-tools file is the
//! definition — `boss gate --park-probe` refuses against it, at gate
//! time rather than hours later at arrival — so the rules point at it
//! rather than restating it. This test holds that collapse: the rules
//! may NAME the file, and may not enumerate its contents.
//!
//! The list's OWN contents are asserted in boss-jobs, which
//! `include_str!`s host-absent-tools.txt (probe.rs). A draft of this
//! file asserted them here too and gate.sh's scope self-test refused
//! it by name — `a file a crate include_str!s is that crate's compile
//! input -> [boss-jobs boss-testing], wanted [boss-jobs]` — which is
//! §9a catching a second copy being written into the wrong crate while
//! the first copy was being removed from a document.

use boss_testing::repo_root;

fn rules() -> String {
    std::fs::read_to_string(repo_root().join("infra/platform/documents/builder-rules.md"))
        .expect("the builder rules are readable")
}

#[test]
fn the_rules_name_the_absent_tools_file_rather_than_its_contents() {
    let text = rules();
    assert!(
        text.contains("host-absent-tools.txt"),
        "the probe-rules paragraph must send a builder to the one \
         authoritative list, since that is the file the gate refuses against"
    );
}

#[test]
fn the_rules_do_not_claim_boss_is_absent_from_the_forge() {
    let text = rules();
    // The exact sentence that was wrong, and any re-spelling of it.
    assert!(
        !text.contains("Not available: boss"),
        "boss has been on the forge since 2026-09-18 (host-absent-tools.txt \
         records the retirement); the rules must not say otherwise"
    );
}
