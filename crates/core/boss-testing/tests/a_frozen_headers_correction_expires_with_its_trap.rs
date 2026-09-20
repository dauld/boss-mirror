//! An applied migration's prose cannot be repaired, so the correction
//! lives where the reader lands — and it is pinned to the trap, so it
//! cannot outlive it.
//!
//! WHAT WAS MEASURED (backlog 8c0860ce, 2026-09-20).
//! `infra/postgres/schema/41-dispatcher.sql` opens by telling the next
//! reader that the seed rows are GENERATED from
//! `infra/dispatcher/rules.toml` by `infra/dispatcher/gen-seed.py`, and
//! that they must NOT be hand-edited. Neither file exists: the rule
//! registry collapsed to one file per rule under
//! `infra/dispatcher/rules/` on 2026-09-11 (41ba00cd), which is a
//! CLAUDE.md §9a case study, and the generator went with it. So the
//! header gives a positive instruction that cannot be followed AND
//! forbids the thing that is now correct.
//!
//! WHY THE HEADER IS NOT SIMPLY FIXED, which is the part worth
//! recording. That file is an APPLIED migration, and `migrate.sh`
//! sha256s the whole file — comments included. Editing three comment
//! lines of `111-gateway-audit-events.sql` on 2026-08-13 stopped every
//! deploy that had already applied it and cost BOSS its own system of
//! record for an hour; `infra/lint/migrations-append-only.sh` exists to
//! refuse exactly that edit at gate time, and it would refuse this one.
//! The prose is frozen with the SQL. A trap that cannot be removed is
//! disarmed where the reader lands instead: `infra/dispatcher/rules/`
//! is the registry's definition, its README is the front door, and the
//! README now names the header and says to read it as history.
//!
//! WHY THIS TEST, rather than trusting the paragraph. The packet is the
//! third instance in one day of prose outliving its mechanism, so a
//! correction that is itself unpinned prose would be the same defect
//! one level up. This pins the note to both halves of the fact it
//! describes: the trap still says what it says, the files it names
//! still do not exist, and the README still names them. Any of the
//! three changing fails HERE, by name — including the happy direction,
//! where the trap is gone and the paragraph should go with it.
//!
//! WHAT THIS DOES NOT DO. It judges ONE frozen header, not the class.
//! A survey of the same shape across `*.rs`, `*.sh` and `*.sql`
//! comments on 6cff49a6 found 84 mentions of a repo-relative path that
//! does not exist, 22 of them inside `infra/postgres/schema/` where no
//! edit is permitted. Catching the class mechanically is a separate
//! question and a separate car; it is named in the packet, not solved
//! here.

use boss_testing::repo_root;

/// The frozen header, and the two paths it sends a reader after.
const FROZEN_MIGRATION: &str = "infra/postgres/schema/41-dispatcher.sql";
const DEAD_INPUT: &str = "infra/dispatcher/rules.toml";
const DEAD_GENERATOR: &str = "infra/dispatcher/gen-seed.py";

/// Where the correction lives: the definition directory's front door.
const REGISTRY_README: &str = "infra/dispatcher/rules/README.md";

fn read(path: &str) -> String {
    std::fs::read_to_string(repo_root().join(path))
        .unwrap_or_else(|e| panic!("{path} is tracked in this repo and must be readable: {e}"))
}

/// The trap: the header names both files, and both are gone.
///
/// Stated as a test rather than as a sentence in the README, because
/// the README's paragraph is only worth its space while this holds.
#[test]
fn the_frozen_header_still_sends_a_reader_after_two_deleted_files() {
    let header = read(FROZEN_MIGRATION);
    for named in [DEAD_INPUT, DEAD_GENERATOR] {
        assert!(
            header.contains(named),
            "{FROZEN_MIGRATION} no longer names {named}. An applied migration's prose does \
             not change by itself, so if this is real the schema directory was rewritten — \
             re-read the header, and if the trap is gone delete the paragraph it is \
             corrected by in {REGISTRY_README}, and this test with it."
        );
        assert!(
            !repo_root().join(named).exists(),
            "{named} exists again, so {FROZEN_MIGRATION}'s header is no longer misleading. \
             Delete the correcting paragraph in {REGISTRY_README} and this test: a note \
             about a trap that is gone is the same defect it was written for."
        );
    }
}

/// The correction: the registry's front door names the frozen header and
/// both dead paths, so a reader who follows the header's instruction and
/// fails finds out why here.
#[test]
fn the_registry_readme_names_the_frozen_header_and_what_it_points_at() {
    let readme = read(REGISTRY_README);
    // The file name alone, not the directory path: the README already
    // says `infra/postgres/schema/` several times in other paragraphs,
    // so a substring test on the directory would pass vacuously.
    for named in ["41-dispatcher.sql", DEAD_GENERATOR] {
        assert!(
            readme.contains(named),
            "{REGISTRY_README} does not name {named}. {FROZEN_MIGRATION} tells its reader to \
             edit files that do not exist and not to touch the rows in front of them, and it \
             is an applied migration, so that prose cannot be repaired in place \
             (migrations-append-only.sh; the 2026-08-13 outage). The correction belongs \
             here, where a reader of the registry lands."
        );
    }
}
