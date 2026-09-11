//! The authored registry directory IS the definition; `dispatcher_rules`
//! is DERIVED from it.
//!
//! WHAT THIS COLLAPSES (CLAUDE.md §9a, backlog 41ba00cd). A dispatcher
//! rule used to be declared twice, in two languages, with nothing
//! deriving one from the other: a TOML file under
//! `infra/dispatcher/rules/` carrying the rule's shape and its reviewed
//! `why`, and an `INSERT INTO dispatcher_rules` in a migration under
//! `infra/postgres/schema/` — thirty-one of them. Only the second one
//! ran, so the tree could say a rule existed, every lint and unit test
//! could agree, and the running system could be enforcing something
//! else, with nothing anywhere reporting a difference. §9a is explicit
//! that the test which compared them (`dispatcher_rules_seed_matches_toml`)
//! was a holding action and not a destination.
//!
//! WHICH DIRECTION COLLAPSED, AND WHY THAT ONE. Three measurements
//! decided it, not taste:
//!
//! 1. **Nothing reconciles `dispatcher_rules`.** `bootstrap_reconcile` is
//!    a `WorkflowRegistry` / `PolicyRepository` method over the
//!    `workflows` and `policy_rules` tables; no equivalent exists for
//!    this table, and no code path republishes a rule row at boot. So
//!    the hazard that forced the WORKFLOW move — a `created_by =
//!    'bootstrap'` row rewritten over an operator's edit on every boot
//!    (68331085) — does not apply here, and the decision had to be made
//!    on the other two.
//! 2. **The `why` cannot live in the table.** `dispatcher_rules` has no
//!    column for it and a row cannot carry a justification a reviewer
//!    saw in a diff, while `parse_raw_dir` REFUSES a file without one.
//!    A registry-as-definition direction would either drop the `why` or
//!    keep it in the tree anyway — which is the same two homes, minus
//!    the shape.
//! 3. **Only the tree can supply a fresh database.** The row does not
//!    exist until something in the tree writes it. "Generate the tree
//!    from the live registry" makes the definition live on one box,
//!    reviewable by nobody, which is the packet's own complaint.
//!
//! So the tree is the definition and this is the derivation, the same
//! shape `boss-platform-workflow-seed` gave `infra/platform/workflows/`
//! on 2026-09-11 — where `platform_workflows()` is now `vec![]` and its
//! comment says EMPTY, AND THAT IS THE DESTINATION.
//!
//! THE CONTRACT, and the reason for each half:
//!
//! - **Insert what is absent, at the version the FILE declares.** Not
//!   `MAX(version) + 1` (what `authoring::create_draft` assigns): the
//!   file's `version` is part of the compared rule content, so a fresh
//!   database must land on the same version a converged one has.
//! - **Touch nothing that exists at that version.** A row already at the
//!   authored version is left exactly as it is, whatever its status —
//!   including one an operator deliberately retired to switch a
//!   misbehaving rule off. This is the INSERT-IF-MISSING posture David
//!   set for the workflow bundle ("drift-healing goes away
//!   deliberately: it is the feature that reverts operator edits").
//! - **Never walk a version back.** An operator publishing live through
//!   `POST /api/dispatcher/rules` is still supported — that is what
//!   "registry data, editable without a deploy" means. A live version
//!   AHEAD of the tree is reported and left alone.
//! - **Retire what the tree no longer authors.** Without this the tree
//!   would define adding and changing a rule but not REMOVING one,
//!   leaving retirement to a migration: two homes for one operation,
//!   which is the half-collapse §9a records as worse than the pin. It
//!   also closes the hole the live-rules lint was written for — four
//!   rules authored live through the API and never written down
//!   (backlog 8d471ec5) — mechanically rather than by a check someone
//!   has to read.
//! - **An unreadable directory writes NOTHING.** `parse_raw_path` errors
//!   on an absent, unreadable or rule-less directory, and this function
//!   propagates that before its first write. A wrong `BOSS_DISPATCHER_RULES`
//!   must not read as "the tree authors no rules" and retire the whole
//!   registry (CLAUDE.md §Doors: a wrong target answers instead of
//!   erroring).
//!
//! MEASURED BEFORE LANDING: the live registry enforces sixty rules, the
//! directory authors sixty, the names match exactly and so does every
//! version. The first deployment that runs this seed therefore writes
//! nothing at all — the collapse starts paying from the next rule
//! change, which needs one file and no migration.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use sqlx::PgPool;

use super::authoring::validate;
use super::registry::{RawRule, RegistryError, parse_raw_path};

/// What one seed pass did, per rule. Every field names rules rather than
/// counting them: a count tells the journal something happened, a name
/// tells the next reader which rule to look at.
#[derive(Debug, Default, Clone)]
pub struct SeedReport {
    /// `(name, version)` published from a file that had no row.
    pub inserted: Vec<(String, u32)>,
    /// Already present at the authored version — left untouched.
    pub present: Vec<String>,
    /// Retired because no file in the authored registry names them.
    pub retired: Vec<String>,
    /// `(name, authored_version, live_version)` where the live registry
    /// is AHEAD of the tree: an operator published without writing the
    /// file back. Left alone, reported loudly.
    pub behind: Vec<(String, u32, u32)>,
    /// `(name, reason)` for a file that would not load as a rule, or
    /// whose write failed. Per-rule, so one bad file cannot stop the
    /// other fifty-nine from reaching the registry.
    pub rejected: Vec<(String, String)>,
}

impl SeedReport {
    /// Did this pass change the registry at all? The dispatcher's boot
    /// logs at a higher level when it did.
    pub fn wrote_anything(&self) -> bool {
        !self.inserted.is_empty() || !self.retired.is_empty()
    }
}

fn store<E: std::fmt::Display>(e: E) -> RegistryError {
    RegistryError::Storage(e.to_string())
}

/// Publish every rule the authored registry at `dir` declares and the
/// `dispatcher_rules` table does not have, and retire every enforced rule
/// the registry no longer names. See the module docs for the contract.
///
/// Errors only on the things that make the whole pass meaningless — a
/// registry that will not read, or a table that will not answer. A single
/// bad rule file lands in `rejected`, because the alternative is one
/// typo taking every other rule down with it.
pub async fn seed_authored_rules(
    pool: &PgPool,
    dir: impl AsRef<Path>,
) -> Result<SeedReport, RegistryError> {
    // FIRST, AND BEFORE ANY WRITE. An absent or rule-less directory is an
    // error here, never an empty authored set.
    let authored = parse_raw_path(dir)?;

    let rows: Vec<(String, i32, String)> =
        sqlx::query_as("SELECT name, version, status FROM dispatcher_rules")
            .fetch_all(pool)
            .await
            .map_err(store)?;

    let mut have: HashSet<(String, i32)> = HashSet::new();
    let mut versions: HashMap<String, BTreeSet<i32>> = HashMap::new();
    let mut active: HashMap<String, i32> = HashMap::new();
    for (name, version, status) in rows {
        have.insert((name.clone(), version));
        versions.entry(name.clone()).or_default().insert(version);
        if status == "active" {
            active.insert(name, version);
        }
    }

    let mut report = SeedReport::default();
    let authored_names: HashSet<&str> = authored.rules.iter().map(|r| r.name.as_str()).collect();

    for rule in &authored.rules {
        // The same gate `publish` applies: a row that would not load is
        // refused BEFORE it reaches the active slot, because a registry
        // that will not parse means the rules runner never starts and
        // every rule stops firing.
        if let Err(e) = validate(rule) {
            report.rejected.push((rule.name.clone(), e.to_string()));
            continue;
        }
        let want = rule.version as i32;
        // AHEAD OF THE TREE, asked BEFORE "is the authored version
        // present". A rule whose file says v1 while the registry holds a
        // v5 someone published live has BOTH facts true — v1 exists, and
        // the tree is behind — and only the second is worth a reader's
        // attention. Answering `present` there would be accurate about
        // the row and silent about the drift.
        //
        // The comparison is against the highest version, not the active
        // one, so re-activating a superseded row is impossible too: if
        // every version above `want` has been retired, writing `want`
        // active would walk the rule backwards just as surely.
        let live_max = versions
            .get(&rule.name)
            .and_then(|v| v.iter().next_back().copied());
        if let Some(max) = live_max.filter(|max| *max > want) {
            report
                .behind
                .push((rule.name.clone(), rule.version, max as u32));
            continue;
        }
        if have.contains(&(rule.name.clone(), want)) {
            report.present.push(rule.name.clone());
            continue;
        }
        match insert_active(pool, rule, want).await {
            Ok(true) => report.inserted.push((rule.name.clone(), rule.version)),
            // A concurrent seed (a second dispatcher replica booting)
            // won the row. Benign: the content came from the same file.
            Ok(false) => report.present.push(rule.name.clone()),
            Err(e) => report.rejected.push((rule.name.clone(), e.to_string())),
        }
    }

    for (name, _) in active {
        if authored_names.contains(name.as_str()) {
            continue;
        }
        match sqlx::query(
            "UPDATE dispatcher_rules SET status = 'retired' \
             WHERE name = $1 AND status = 'active'",
        )
        .bind(&name)
        .execute(pool)
        .await
        {
            Ok(_) => report.retired.push(name),
            Err(e) => report.rejected.push((name, e.to_string())),
        }
    }

    report.inserted.sort();
    report.present.sort();
    report.retired.sort();
    report.behind.sort();
    report.rejected.sort();
    Ok(report)
}

/// Append `rule` at `version` as the ACTIVE row, retiring the incumbent
/// in the same transaction — the same append-only move `publish` makes,
/// except it names the version instead of promoting "the latest draft".
/// That distinction is load-bearing: `publish` promotes whatever draft is
/// newest, which on 2026-08-28 regressed a live workflow from v19 to v16
/// when an unrelated draft was sitting armed.
///
/// `Ok(false)` means another writer inserted this exact `(name, version)`
/// first; the transaction rolls back, so the retire does not stand
/// either.
async fn insert_active(pool: &PgPool, rule: &RawRule, version: i32) -> Result<bool, RegistryError> {
    let do_json = serde_json::to_value(&rule.do_steps).map_err(store)?;
    let cadence = rule.schedule.as_ref().map(|s| s.cadence.token());
    let anchor = rule.schedule.as_ref().map(|s| s.anchor_date);
    let calendar = rule
        .schedule
        .as_ref()
        .and_then(|s| s.business_calendar.clone());

    let mut tx = pool.begin().await.map_err(store)?;
    sqlx::query(
        "UPDATE dispatcher_rules SET status = 'retired' WHERE name = $1 AND status = 'active'",
    )
    .bind(&rule.name)
    .execute(&mut *tx)
    .await
    .map_err(store)?;
    let done = sqlx::query(
        "INSERT INTO dispatcher_rules \
            (name, version, status, on_event, when_expr, do_steps, delay, \
             schedule_cadence, schedule_anchor, schedule_calendar) \
         VALUES ($1, $2, 'active', $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (name, version) DO NOTHING",
    )
    .bind(&rule.name)
    .bind(version)
    .bind(&rule.on_event)
    .bind(&rule.when)
    .bind(&do_json)
    .bind(&rule.delay)
    .bind(cadence)
    .bind(anchor)
    .bind(calendar)
    .execute(&mut *tx)
    .await
    .map_err(store)?;
    if done.rows_affected() == 0 {
        tx.rollback().await.map_err(store)?;
        return Ok(false);
    }
    tx.commit().await.map_err(store)?;
    Ok(true)
}
