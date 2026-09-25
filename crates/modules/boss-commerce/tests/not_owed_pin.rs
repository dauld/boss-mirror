//! The SPA's copy of "not owed" is the server's (backlog 926d64a3).
//!
//! `InvoiceStatus::NOT_OWED` is the one definition of owed: open-AR
//! and the summary's AR aging bind it. The Finance InvoicesTab decides
//! "unpaid" in the browser, over rows it already holds, so the list
//! crosses the wire as a copy in `apps/web/src/finance/types.ts`. A
//! fact that lives twice gets an equality test (CLAUDE.md 9a): this
//! reads the TS literal and names what differs when the two drift.

use boss_commerce::types::InvoiceStatus;

#[test]
fn the_spa_not_owed_list_is_the_servers() {
    let path = boss_testing::repo_root().join("apps/web/src/finance/types.ts");
    let src = std::fs::read_to_string(&path).unwrap();
    let decl = "export const NOT_OWED: ReadonlyArray<InvoiceStatus> = [";
    let start = src
        .find(decl)
        .unwrap_or_else(|| panic!("{} no longer declares `{decl}`", path.display()))
        + decl.len();
    let end = start + src[start..].find(']').unwrap();
    let mut spa: Vec<&str> = src[start..end]
        .split(',')
        .map(|s| s.trim().trim_matches('\''))
        .filter(|s| !s.is_empty())
        .collect();
    let mut server: Vec<&str> = InvoiceStatus::NOT_OWED.to_vec();
    spa.sort_unstable();
    server.sort_unstable();
    assert_eq!(
        spa,
        server,
        "{}'s NOT_OWED must equal InvoiceStatus::NOT_OWED",
        path.display()
    );
}
