//! The SPA's guest roles and core's read-only floor are one fact that
//! lives twice (CLAUDE.md §9a), so this pins them equal.
//!
//! `boss_core::roles::READ_ONLY_FLOOR_ROLES` is the set of roles a guest
//! session can carry (design 2830b6b7, decided 2026-09-25: `visitor` on
//! the OSS basic default, `audit-readonly` where an instance opts in to
//! the system-audit read). The web kit's `classifyProbe` recognises the
//! guest by the same roles, in `READ_ONLY_GUEST_ROLES`, because a Rust
//! constant cannot be imported into the SPA. When they drift, the drift
//! is silent in exactly one direction: a floor role the SPA does not
//! know signs a guest in and renders "no matching employee" — the
//! guest button working on the server and broken on the page.

use boss_testing::repo_root;

const CLASSIFY_TS: &str = "libs/web-kit/src/session/classify.ts";

/// The quoted literals on the line that declares `READ_ONLY_GUEST_ROLES`.
fn web_guest_roles() -> Vec<String> {
    let path = repo_root().join(CLASSIFY_TS);
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let line = src
        .lines()
        .find(|l| l.contains("export const READ_ONLY_GUEST_ROLES"))
        .unwrap_or_else(|| panic!("{CLASSIFY_TS} declares no READ_ONLY_GUEST_ROLES on one line"));
    line.split('\'')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

#[test]
fn the_web_guest_roles_are_exactly_the_read_only_floor() {
    let web = web_guest_roles();
    let floor: Vec<String> = boss_core::roles::READ_ONLY_FLOOR_ROLES
        .iter()
        .map(|r| r.to_string())
        .collect();
    assert_eq!(
        web, floor,
        "{CLASSIFY_TS} READ_ONLY_GUEST_ROLES has drifted from \
         boss_core::roles::READ_ONLY_FLOOR_ROLES — a guest carrying a role \
         the SPA does not list is shown as an unrecognised login"
    );
}
