//! Rebuild the `accounts` + `account_contacts` projections from
//! `audit_log`. See `docs/design/projection-rebuilders.md`.
//!
//! State events consumed:
//! - `accounts.account.created` / `.updated` — full
//!   `AccountWithContacts` payload. Rebuild UPSERTs the account row
//!   and replaces the contact list wholesale (event payload is
//!   canonical).
//! - `accounts.account.deleted` — `{id, deleted_at}`. Rebuild
//!   removes the row; account_contacts cascades via FK.
//!
//! `account_team_members` — `accounts.account.team-assigned`
//! upserts a (account_id, role) row; `accounts.account.team-unassigned`
//! deletes it.
//!
//! `account_notes` — `accounts.account.note-posted` INSERTs into
//! account_notes; `accounts.account.note-deleted` soft-deletes
//! (stamps deleted_at + deleted_by).
//!
//! `support_cases` — `accounts.support-case.opened` INSERTs the row;
//! `accounts.support-case.updated` applies the same COALESCE-style
//! partial UPDATE the handler uses.
//!
//! `account_facts` is intentionally NOT a write target — facts are a
//! projection from jobs.* events (per the schema D2 comment), not a
//! directly-written subledger.

use boss_events::replay::{Applied, replay_projection};
use serde::Deserialize;
use sqlx::PgPool;
use tracing::warn;

use crate::accounts::{Account, AccountContact};

/// Stable advisory-lock key. Distinct from boss-ledger (…_001),
/// boss-messages (…_002), boss-jobs (…_003), boss-inventory (…_004).
const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("accounts");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub accounts_upserted: u64,
    pub accounts_deleted: u64,
    pub team_members_upserted: u64,
    pub team_members_unassigned: u64,
    pub notes_posted: u64,
    pub notes_deleted: u64,
    pub support_cases_opened: u64,
    pub support_cases_updated: u64,
}

/// Payload shape — mirrors `AccountWithContacts` but tolerates
/// pre-enrichment events that just had `{id}`.
#[derive(Debug, Deserialize)]
struct AccountUpsertPayload {
    #[serde(flatten)]
    account: Account,
    #[serde(default)]
    contacts: Vec<AccountContact>,
}

pub async fn rebuild_accounts(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();

    // TRUNCATE … CASCADE so we wipe every table that FKs to
    // accounts in one shot. Out-of-scope tables (account_notes,
    // account_team_members, account_facts, service_agreements,
    // shipments via account_id, invoices via account_id) reference
    // accounts; CASCADE handles the FK fan-out without us having
    // to enumerate every dependent table.
    // CASCADE wipes `account_team_members` + `account_notes`
    // (both FK to accounts). The team-assigned + note-posted event
    // branches below repopulate them from audit_log.
    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &[
            "TRUNCATE accounts, account_contacts, account_team_members, account_notes, support_cases CASCADE",
        ],
        "kind LIKE 'accounts.account.%' OR kind LIKE 'accounts.support-case.%'",
        async |conn, ev| {
            match ev.kind.as_str() {
                "accounts.account.created" | "accounts.account.updated" => {
                    let p: AccountUpsertPayload = match serde_json::from_value(ev.payload.clone()) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!(
                                event_id = ev.audit_id,
                                kind = %ev.kind,
                                error = %e,
                                "skipping account event with non-AccountWithContacts payload \
                                 (likely pre-enrichment id-only)"
                            );
                            return Ok(Applied::Skipped);
                        }
                    };
                    upsert_account(&mut *conn, &p.account, &p.contacts, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.accounts_upserted += 1;
                    Ok(Applied::Yes)
                }
                "accounts.account.team-assigned" => {
                    let evt: crate::account_team_members::AccountTeamAssignmentEvent =
                        match serde_json::from_value(ev.payload.clone()) {
                            Ok(e) => e,
                            Err(e) => {
                                warn!(
                                    event_id = ev.audit_id,
                                    error = %e,
                                    "skipping malformed team-assigned payload"
                                );
                                return Ok(Applied::Skipped);
                            }
                        };
                    crate::account_team_members::upsert_team_member(&mut *conn, &evt, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.team_members_upserted += 1;
                    Ok(Applied::Yes)
                }
                "accounts.account.team-unassigned" => {
                    let evt: crate::account_team_members::AccountTeamUnassignmentEvent =
                        match serde_json::from_value(ev.payload.clone()) {
                            Ok(e) => e,
                            Err(e) => {
                                warn!(
                                    event_id = ev.audit_id,
                                    error = %e,
                                    "skipping malformed team-unassigned payload"
                                );
                                return Ok(Applied::Skipped);
                            }
                        };
                    let n = sqlx::query(
                        "DELETE FROM account_team_members \
                         WHERE account_id = $1 AND role = $2",
                    )
                    .bind(&evt.account_id)
                    .bind(evt.role.as_str())
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| e.to_string())?
                    .rows_affected();
                    if n > 0 {
                        report.team_members_unassigned += 1;
                        Ok(Applied::Yes)
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                "accounts.account.note-posted" => {
                    let evt: crate::account_notes::AccountNotePostedEvent =
                        match serde_json::from_value(ev.payload.clone()) {
                            Ok(e) => e,
                            Err(e) => {
                                warn!(
                                    event_id = ev.audit_id,
                                    error = %e,
                                    "skipping malformed note-posted payload"
                                );
                                return Ok(Applied::Skipped);
                            }
                        };
                    crate::account_notes::upsert_note(&mut *conn, &evt, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.notes_posted += 1;
                    Ok(Applied::Yes)
                }
                "accounts.account.note-deleted" => {
                    let evt: crate::account_notes::AccountNoteDeletedEvent =
                        match serde_json::from_value(ev.payload.clone()) {
                            Ok(e) => e,
                            Err(e) => {
                                warn!(
                                    event_id = ev.audit_id,
                                    error = %e,
                                    "skipping malformed note-deleted payload"
                                );
                                return Ok(Applied::Skipped);
                            }
                        };
                    let n = crate::account_notes::soft_delete_note(&mut *conn, &evt)
                        .await
                        .map_err(|e| e.to_string())?;
                    if n > 0 {
                        report.notes_deleted += 1;
                        Ok(Applied::Yes)
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                "accounts.support-case.opened" => {
                    let case: crate::support_cases::SupportCase =
                        match serde_json::from_value(ev.payload.clone()) {
                            Ok(c) => c,
                            Err(e) => {
                                warn!(
                                    event_id = ev.audit_id,
                                    error = %e,
                                    "skipping malformed support-case-opened payload"
                                );
                                return Ok(Applied::Skipped);
                            }
                        };
                    crate::support_cases::upsert_case(&mut *conn, &case, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.support_cases_opened += 1;
                    Ok(Applied::Yes)
                }
                "accounts.support-case.updated" => {
                    let evt: crate::support_cases::SupportCaseUpdateEvent =
                        match serde_json::from_value(ev.payload.clone()) {
                            Ok(e) => e,
                            Err(e) => {
                                warn!(
                                    event_id = ev.audit_id,
                                    error = %e,
                                    "skipping malformed support-case-updated payload"
                                );
                                return Ok(Applied::Skipped);
                            }
                        };
                    let n = crate::support_cases::apply_case_update(&mut *conn, &evt)
                        .await
                        .map_err(|e| e.to_string())?;
                    if n > 0 {
                        report.support_cases_updated += 1;
                        Ok(Applied::Yes)
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                "accounts.account.deleted" => {
                    let id: Option<String> =
                        ev.payload.get("id").and_then(|v| v.as_str()).map(String::from);
                    if let Some(id) = id {
                        let n = sqlx::query("DELETE FROM accounts WHERE id = $1")
                            .bind(&id)
                            .execute(&mut *conn)
                            .await
                            .map_err(|e| e.to_string())?
                            .rows_affected();
                        if n > 0 {
                            report.accounts_deleted += 1;
                            Ok(Applied::Yes)
                        } else {
                            Ok(Applied::Skipped)
                        }
                    } else {
                        Ok(Applied::Skipped)
                    }
                }
                other => {
                    warn!(event_id = ev.audit_id, kind = %other, "unknown accounts.* event kind");
                    Ok(Applied::Skipped)
                }
            }
        },
    )
    .await
    .map_err(RebuildError::Storage)?;

    report.events_processed = stats.processed;
    report.events_skipped = stats.skipped;
    Ok(report)
}

async fn upsert_account(
    tx: &mut sqlx::PgConnection,
    account: &Account,
    contacts: &[AccountContact],
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), RebuildError> {
    sqlx::query(
        "INSERT INTO accounts (id, name, director, city, state, tier, customer_since, \
                                territory_rep_id, account_type) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (id) DO UPDATE SET \
             name = EXCLUDED.name, \
             director = EXCLUDED.director, \
             city = EXCLUDED.city, \
             state = EXCLUDED.state, \
             tier = EXCLUDED.tier, \
             customer_since = EXCLUDED.customer_since, \
             territory_rep_id = EXCLUDED.territory_rep_id, \
             account_type = EXCLUDED.account_type",
    )
    .bind(&account.id)
    .bind(&account.name)
    .bind(&account.director)
    .bind(&account.city)
    .bind(&account.state)
    .bind(&account.tier)
    .bind(account.customer_since)
    .bind(&account.territory_rep_id)
    .bind(&account.account_type)
    .execute(&mut *tx)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;

    // Replace the contact set wholesale — event payload is canonical.
    sqlx::query("DELETE FROM account_contacts WHERE account_id = $1")
        .bind(&account.id)
        .execute(&mut *tx)
        .await
        .map_err(|e| RebuildError::Storage(e.to_string()))?;
    for c in contacts {
        // created_at = ev.ts, mirroring the live insert's
        // stamp.timestamp bind — leaving it to DEFAULT NOW() re-dated
        // every contact to rebuild time (packet d7b8158e).
        sqlx::query(
            "INSERT INTO account_contacts (id, account_id, name, role, email, phone, is_primary, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&c.id)
        .bind(&account.id)
        .bind(&c.name)
        .bind(&c.role)
        .bind(&c.email)
        .bind(&c.phone)
        .bind(c.is_primary)
        .bind(ts)
        .execute(&mut *tx)
        .await
        .map_err(|e| RebuildError::Storage(e.to_string()))?;
    }
    Ok(())
}
