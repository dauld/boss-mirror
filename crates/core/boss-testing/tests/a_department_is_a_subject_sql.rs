//! The department registry, RUN against the real schema — the rows a
//! fresh instance gets, held equal to the codes the page march's
//! packets already carry (backlog 5987302f; design 32f18167, answered
//! by David 2026-09-19).
//!
//! THE FACT THAT LIVES TWICE (CLAUDE.md §9a). The department codes are
//! spelled in `apps/web/src/shell/nav-catalog.ts` as the `app` field of
//! each catalogued route, and `apps/web/scripts/open-page-audits.ts`
//! turns that field into the `department` a page-audit packet carries.
//! On 2026-09-19 forty-seven such packets were opened carrying thirteen
//! distinct codes. This migration writes those codes a second time, as
//! rows. Nothing would notice them drifting: a packet whose department
//! code matches no row reads as a perfectly normal packet, and the
//! registry simply answers nothing when asked what owns the route. So
//! the two are pinned equal here — the expected set is DERIVED from the
//! two frontend files, never typed, and the actual set is read from a
//! database with every migration applied.
//!
//! HOME IS NOT A ROW, deliberately. `home` is the fourteenth `app` code
//! in the catalog and it is not a department: Home surfaces are
//! cross-cutting personal work, and IT is the department that builds
//! them. `open-page-audits.ts` already maps it — `departmentFor` sends
//! `home` (and `simulator`) to `HOME_DEPARTMENT`, which is `it` — which
//! is why none of the forty-seven packets carries `home`. A `home` row
//! would be a department no packet, employee or Job would ever name.
//! The mapping is pinned below so that "home is handled at one place"
//! stays true rather than becoming folklore.
//!
//! Never against production: TestDb refuses a server hosting a database
//! named `boss` (test_db.rs).

use boss_testing::{TestDb, repo_root};
use sqlx::Row;
use std::collections::BTreeSet;

const CATALOG: &str = "apps/web/src/shell/nav-catalog.ts";
const OPENER: &str = "apps/web/scripts/open-page-audits.ts";

fn read(path: &str) -> String {
    std::fs::read_to_string(repo_root().join(path)).unwrap_or_else(|e| {
        panic!("{path}: {e} — the derivation cannot be read, so nothing here is a verdict")
    })
}

/// `HOME_DEPARTMENT` as the opener spells it: the department a Home
/// surface's packet names.
fn home_department(opener: &str) -> String {
    let marker = "export const HOME_DEPARTMENT = '";
    let rest = opener
        .split_once(marker)
        .unwrap_or_else(|| {
            panic!("{OPENER} no longer declares {marker}… — re-derive this test before trusting it")
        })
        .1;
    rest.split_once('\'')
        .expect("HOME_DEPARTMENT is an unterminated string literal")
        .0
        .to_string()
}

/// The `app` codes the catalog assigns its routes — every department
/// code the SPA knows, plus the two non-department codes the opener
/// maps away.
fn catalog_apps(catalog: &str) -> BTreeSet<String> {
    let apps: BTreeSet<String> = catalog
        .split("app: '")
        .skip(1)
        .filter_map(|rest| rest.split_once('\'').map(|(code, _)| code.to_string()))
        .collect();
    assert!(
        apps.len() > 5,
        "{CATALOG} yielded {} app codes — the spelling changed and this test is reading nothing",
        apps.len()
    );
    apps
}

/// `departmentFor` in the opener, in Rust: the department a route's
/// packet carries, given the `app` the catalog assigned it.
fn department_for(app: &str, home: &str) -> String {
    match app {
        "home" | "simulator" => home.to_string(),
        other => other.to_string(),
    }
}

/// Every department code a page-audit packet can carry today.
fn expected_departments() -> BTreeSet<String> {
    let home = home_department(&read(OPENER));
    catalog_apps(&read(CATALOG))
        .iter()
        .map(|app| department_for(app, &home))
        .collect()
}

async fn codes(db: &TestDb, sql: &str) -> BTreeSet<String> {
    sqlx::query(sql)
        .fetch_all(&db.pool)
        .await
        .expect("query")
        .iter()
        .map(|r| r.get::<String, _>(0))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_rows_are_exactly_the_codes_the_page_march_carries() {
    let db = TestDb::new().await;
    assert_eq!(
        codes(&db, "SELECT id FROM departments WHERE retired_at IS NULL").await,
        expected_departments(),
        "the department registry and the codes page-audit packets carry have drifted — \
         a packet whose department matches no row reads as a normal packet, which is why \
         this is a test and not a comment ({CATALOG} + {OPENER})"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn home_is_mapped_at_one_place_rather_than_given_a_row() {
    let opener = read(OPENER);
    assert_eq!(
        home_department(&opener),
        "it",
        "Home surfaces are IT's; if that changed, the department rows must be re-read"
    );
    assert!(
        opener
            .lines()
            .any(|l| l.contains("app === 'home'") && l.contains("return HOME_DEPARTMENT")),
        "{OPENER} no longer maps the catalog's `home` app to a department in one line — \
         the reason `home` has no row is that this mapping exists"
    );

    let db = TestDb::new().await;
    assert!(
        codes(&db, "SELECT id FROM departments WHERE id = 'home'")
            .await
            .is_empty(),
        "`home` is a catalog surface, not a department — it is mapped to `it`, not seeded"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_department_is_a_subject_a_job_can_name() {
    let db = TestDb::new().await;
    assert_eq!(
        codes(
            &db,
            "SELECT kind FROM subject_kinds WHERE kind = 'department'"
        )
        .await,
        BTreeSet::from(["department".to_string()]),
        "a department has identity, so it is a Subject kind (design 32f18167)"
    );
    // The jobs existence gate reads `subjects` for every kind, so a
    // department without an identity row is a department no Job can be
    // about — which is the whole claim the design rests on.
    assert_eq!(
        codes(&db, "SELECT id FROM subjects WHERE kind = 'department'").await,
        codes(&db, "SELECT id FROM departments").await,
        "every department row needs its identity row, or `the warehouse ran its retro late` \
         is still unsayable"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_departments_function_is_class_data_on_the_kind() {
    let db = TestDb::new().await;
    let functions = codes(
        &db,
        "SELECT code FROM classes WHERE subject_kind = 'department' AND member_attribute = 'function'",
    )
    .await;
    assert_eq!(
        functions,
        ["governance", "operations", "revenue", "support"]
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<String>>(),
        "the four functions the design names are the Class taxonomy on the department kind"
    );
    let worn = codes(&db, "SELECT DISTINCT function FROM departments").await;
    assert!(
        worn.is_subset(&functions),
        "a department wears a function the Class registry does not declare: {:?}",
        worn.difference(&functions).collect::<Vec<_>>()
    );
    // Not the other way round: a declared function with no department
    // wearing it is a taxonomy with room in it, not a defect.
}
