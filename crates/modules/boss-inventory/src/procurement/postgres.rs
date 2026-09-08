//! Postgres adapter for the Vendor CRM. Keeps write paths idempotent
//! on `id` so replay and sim scenarios don't duplicate.

use sqlx::{PgPool, Row, postgres::PgRow};

use super::types::{
    InteractionCommitment, NewVendorAccountTeamMember, NewVendorContact, NewVendorContract,
    NewVendorInteraction, VendorAccountTeamMember, VendorContact, VendorContract,
    VendorInteraction,
};
use crate::port::InventoryError;

/// Begin a tx / record / commit helpers for the outbox-phase-2 write
/// paths: every CRM mutation records its audit event in the SAME
/// transaction as the row, so the event and the state commit or abort
/// together (boss-event-relay moves it to audit_log + NATS).
async fn begin(pool: &PgPool) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, InventoryError> {
    pool.begin().await.map_err(store)
}

async fn record(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    stamp: &boss_core::publisher::EventStamp,
    kind: &str,
    payload: serde_json::Value,
) -> Result<(), InventoryError> {
    boss_events::outbox::record_event_in_tx(tx, &stamp.event(kind, payload))
        .await
        .map_err(InventoryError::Storage)
}

async fn commit(tx: sqlx::Transaction<'_, sqlx::Postgres>) -> Result<(), InventoryError> {
    tx.commit().await.map_err(store)
}

pub struct PgProcurement {
    pool: PgPool,
}

impl PgProcurement {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    // --- vendor contacts --------------------------------------------------

    pub async fn list_contacts(
        &self,
        vendor_id: &str,
    ) -> Result<Vec<VendorContact>, InventoryError> {
        let rows = sqlx::query(
            "SELECT id, vendor_id, name, role, email, phone, territory, specialties, \
                    is_primary, relationship_start, notes, created_at, updated_at \
             FROM vendor_contacts \
             WHERE vendor_id = $1 \
             ORDER BY is_primary DESC, name",
        )
        .bind(vendor_id)
        .fetch_all(&self.pool)
        .await
        .map_err(store)?;
        Ok(rows.iter().map(row_to_contact).collect())
    }

    pub async fn upsert_contact(
        &self,
        new: NewVendorContact,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<VendorContact, InventoryError> {
        let mut tx = begin(&self.pool).await?;
        let specialties = serde_json::to_value(&new.specialties).unwrap_or(serde_json::json!([]));
        let row = sqlx::query(
            "INSERT INTO vendor_contacts \
                (id, vendor_id, name, role, email, phone, territory, specialties, \
                 is_primary, relationship_start, notes) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (id) DO UPDATE SET \
                name = EXCLUDED.name, \
                role = EXCLUDED.role, \
                email = EXCLUDED.email, \
                phone = EXCLUDED.phone, \
                territory = EXCLUDED.territory, \
                specialties = EXCLUDED.specialties, \
                is_primary = EXCLUDED.is_primary, \
                relationship_start = EXCLUDED.relationship_start, \
                notes = EXCLUDED.notes, \
                updated_at = NOW() \
             RETURNING id, vendor_id, name, role, email, phone, territory, specialties, \
                       is_primary, relationship_start, notes, created_at, updated_at",
        )
        .bind(&new.id)
        .bind(&new.vendor_id)
        .bind(&new.name)
        .bind(&new.role)
        .bind(&new.email)
        .bind(new.phone.as_deref())
        .bind(new.territory.as_deref())
        .bind(&specialties)
        .bind(new.is_primary)
        .bind(new.relationship_start)
        .bind(new.notes.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(store)?;
        let contact = row_to_contact(&row);
        record(
            &mut tx,
            stamp,
            crate::events::VENDOR_CONTACT_UPSERTED,
            serde_json::to_value(&contact).unwrap_or_default(),
        )
        .await?;
        commit(tx).await?;
        Ok(contact)
    }

    pub async fn delete_contact(
        &self,
        vendor_id: &str,
        id: &str,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), InventoryError> {
        let mut tx = begin(&self.pool).await?;
        let result = sqlx::query("DELETE FROM vendor_contacts WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(store)?;
        // Event only when a row actually went away — a delete of a
        // missing contact stays a no-op instead of logging a phantom.
        if result.rows_affected() > 0 {
            record(
                &mut tx,
                stamp,
                crate::events::VENDOR_CONTACT_DELETED,
                serde_json::json!({
                    "id": id,
                    "vendor_id": vendor_id,
                    "deleted_at": stamp.timestamp,
                }),
            )
            .await?;
        }
        commit(tx).await?;
        Ok(())
    }

    // --- vendor interactions ----------------------------------------------

    pub async fn list_interactions(
        &self,
        vendor_id: &str,
        limit: i64,
    ) -> Result<Vec<VendorInteraction>, InventoryError> {
        let rows = sqlx::query(
            "SELECT id, vendor_id, vendor_contact_id, actor_id, kind, body, commitments, \
                    linked_po_id, linked_part_sku, linked_job_id, occurred_at, created_at \
             FROM vendor_interactions \
             WHERE vendor_id = $1 AND deleted_at IS NULL \
             ORDER BY occurred_at DESC \
             LIMIT $2",
        )
        .bind(vendor_id)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await
        .map_err(store)?;
        Ok(rows.iter().map(row_to_interaction).collect())
    }

    pub async fn insert_interaction(
        &self,
        new: NewVendorInteraction,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<VendorInteraction, InventoryError> {
        let mut tx = begin(&self.pool).await?;
        let commitments = serde_json::to_value(&new.commitments).unwrap_or(serde_json::json!([]));
        let row = sqlx::query(
            "INSERT INTO vendor_interactions \
                (id, vendor_id, vendor_contact_id, actor_id, kind, body, commitments, \
                 linked_po_id, linked_part_sku, linked_job_id, occurred_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, COALESCE($11, NOW())) \
             ON CONFLICT (id) DO UPDATE SET \
                body = EXCLUDED.body, \
                commitments = EXCLUDED.commitments, \
                linked_po_id = EXCLUDED.linked_po_id, \
                linked_part_sku = EXCLUDED.linked_part_sku, \
                linked_job_id = EXCLUDED.linked_job_id \
             RETURNING id, vendor_id, vendor_contact_id, actor_id, kind, body, commitments, \
                       linked_po_id, linked_part_sku, linked_job_id, occurred_at, created_at",
        )
        .bind(&new.id)
        .bind(&new.vendor_id)
        .bind(new.vendor_contact_id.as_deref())
        .bind(&new.actor_id)
        .bind(&new.kind)
        .bind(&new.body)
        .bind(&commitments)
        .bind(new.linked_po_id.as_deref())
        .bind(new.linked_part_sku.as_deref())
        .bind(new.linked_job_id.as_deref())
        .bind(new.occurred_at)
        .fetch_one(&mut *tx)
        .await
        .map_err(store)?;
        let interaction = row_to_interaction(&row);
        record(
            &mut tx,
            stamp,
            crate::events::VENDOR_INTERACTION_RECORDED,
            serde_json::to_value(&interaction).unwrap_or_default(),
        )
        .await?;
        commit(tx).await?;
        Ok(interaction)
    }

    pub async fn soft_delete_interaction(
        &self,
        id: &str,
        by_employee_id: &str,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), InventoryError> {
        let mut tx = begin(&self.pool).await?;
        // deleted_at binds the SAME instant the event payload carries
        // (stamp.timestamp) — NOW() minted a second wall reading, so
        // the replayed row (which binds the payload's deleted_at)
        // never matched the live one (packet d7b8158e).
        let result = sqlx::query(
            "UPDATE vendor_interactions \
                SET deleted_at = $3, deleted_by = $2 \
              WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(by_employee_id)
        .bind(stamp.timestamp)
        .execute(&mut *tx)
        .await
        .map_err(store)?;
        // The `deleted_at IS NULL` guard makes a re-delete a 0-row
        // no-op; gating the event on it keeps the log duplicate-free.
        if result.rows_affected() > 0 {
            record(
                &mut tx,
                stamp,
                crate::events::VENDOR_INTERACTION_DELETED,
                serde_json::json!({
                    "id": id,
                    "deleted_by": by_employee_id,
                    "deleted_at": stamp.timestamp,
                }),
            )
            .await?;
        }
        commit(tx).await?;
        Ok(())
    }

    // --- vendor account team ----------------------------------------------

    pub async fn list_account_team(
        &self,
        vendor_id: &str,
    ) -> Result<Vec<VendorAccountTeamMember>, InventoryError> {
        let rows = sqlx::query(
            "SELECT id, vendor_id, employee_id, role, assigned_on, notes, created_at \
             FROM vendor_account_team \
             WHERE vendor_id = $1 \
             ORDER BY role",
        )
        .bind(vendor_id)
        .fetch_all(&self.pool)
        .await
        .map_err(store)?;
        Ok(rows.iter().map(row_to_team_member).collect())
    }

    pub async fn upsert_account_team_member(
        &self,
        new: NewVendorAccountTeamMember,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<VendorAccountTeamMember, InventoryError> {
        let mut tx = begin(&self.pool).await?;
        let row = sqlx::query(
            "INSERT INTO vendor_account_team \
                (id, vendor_id, employee_id, role, assigned_on, notes) \
             VALUES ($1, $2, $3, $4, COALESCE($5, CURRENT_DATE), $6) \
             ON CONFLICT (vendor_id, role) DO UPDATE SET \
                employee_id = EXCLUDED.employee_id, \
                assigned_on = EXCLUDED.assigned_on, \
                notes = EXCLUDED.notes \
             RETURNING id, vendor_id, employee_id, role, assigned_on, notes, created_at",
        )
        .bind(&new.id)
        .bind(&new.vendor_id)
        .bind(&new.employee_id)
        .bind(&new.role)
        .bind(new.assigned_on)
        .bind(new.notes.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(store)?;
        let member = row_to_team_member(&row);
        record(
            &mut tx,
            stamp,
            crate::events::VENDOR_TEAM_ASSIGNED,
            serde_json::to_value(&member).unwrap_or_default(),
        )
        .await?;
        commit(tx).await?;
        Ok(member)
    }

    pub async fn remove_account_team_member(
        &self,
        vendor_id: &str,
        role: &str,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), InventoryError> {
        let mut tx = begin(&self.pool).await?;
        // Capture the prior assignment IN the tx so the unassigned
        // event carries the employee_id at the moment of removal
        // (auditors see who was unassigned without walking history;
        // previously this was a racy pre-read outside the delete).
        let prior_employee_id: Option<String> = sqlx::query_scalar(
            "SELECT employee_id FROM vendor_account_team WHERE vendor_id = $1 AND role = $2",
        )
        .bind(vendor_id)
        .bind(role)
        .fetch_optional(&mut *tx)
        .await
        .map_err(store)?;
        let result =
            sqlx::query("DELETE FROM vendor_account_team WHERE vendor_id = $1 AND role = $2")
                .bind(vendor_id)
                .bind(role)
                .execute(&mut *tx)
                .await
                .map_err(store)?;
        if result.rows_affected() > 0 {
            record(
                &mut tx,
                stamp,
                crate::events::VENDOR_TEAM_UNASSIGNED,
                serde_json::json!({
                    "vendor_id": vendor_id,
                    "role": role,
                    "employee_id": prior_employee_id,
                    "unassigned_at": stamp.timestamp,
                }),
            )
            .await?;
        }
        commit(tx).await?;
        Ok(())
    }

    // --- vendor contracts -------------------------------------------------

    pub async fn list_contracts(
        &self,
        vendor_id: &str,
        status: Option<&str>,
    ) -> Result<Vec<VendorContract>, InventoryError> {
        let rows = sqlx::query(
            "SELECT id, vendor_id, kind, title, effective_on, expires_on, auto_renew, \
                    terms, document_uri, status, signed_by_employee_id, signed_at, notes, \
                    created_at, updated_at \
             FROM vendor_contracts \
             WHERE vendor_id = $1 \
               AND ($2::text IS NULL OR status = $2) \
             ORDER BY effective_on DESC",
        )
        .bind(vendor_id)
        .bind(status)
        .fetch_all(&self.pool)
        .await
        .map_err(store)?;
        Ok(rows.iter().map(row_to_contract).collect())
    }

    pub async fn upsert_contract(
        &self,
        new: NewVendorContract,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<VendorContract, InventoryError> {
        let mut tx = begin(&self.pool).await?;
        let terms = if new.terms.is_null() {
            serde_json::json!({})
        } else {
            new.terms
        };
        let row = sqlx::query(
            "INSERT INTO vendor_contracts \
                (id, vendor_id, kind, title, effective_on, expires_on, auto_renew, \
                 terms, document_uri, status, signed_by_employee_id, signed_at, notes) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, COALESCE($10, 'draft'), $11, $12, $13) \
             ON CONFLICT (id) DO UPDATE SET \
                kind = EXCLUDED.kind, \
                title = EXCLUDED.title, \
                effective_on = EXCLUDED.effective_on, \
                expires_on = EXCLUDED.expires_on, \
                auto_renew = EXCLUDED.auto_renew, \
                terms = EXCLUDED.terms, \
                document_uri = EXCLUDED.document_uri, \
                status = EXCLUDED.status, \
                signed_by_employee_id = EXCLUDED.signed_by_employee_id, \
                signed_at = EXCLUDED.signed_at, \
                notes = EXCLUDED.notes, \
                updated_at = NOW() \
             RETURNING id, vendor_id, kind, title, effective_on, expires_on, auto_renew, \
                       terms, document_uri, status, signed_by_employee_id, signed_at, notes, \
                       created_at, updated_at",
        )
        .bind(&new.id)
        .bind(&new.vendor_id)
        .bind(&new.kind)
        .bind(&new.title)
        .bind(new.effective_on)
        .bind(new.expires_on)
        .bind(new.auto_renew)
        .bind(&terms)
        .bind(new.document_uri.as_deref())
        .bind(new.status.as_deref())
        .bind(new.signed_by_employee_id.as_deref())
        .bind(new.signed_at)
        .bind(new.notes.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(store)?;
        let contract = row_to_contract(&row);
        record(
            &mut tx,
            stamp,
            crate::events::VENDOR_CONTRACT_UPSERTED,
            serde_json::to_value(&contract).unwrap_or_default(),
        )
        .await?;
        commit(tx).await?;
        Ok(contract)
    }
}

// --- replay-side upserts -----------------------------------------------------
//
// These mirror the handler-side INSERT/UPSERT SQL but accept a generic
// Executor so the rebuilder can call them inside its transaction. Keeping
// one INSERT statement per entity means handler + rebuilder can never
// drift apart on row shape.

pub(crate) async fn replay_upsert_contact<'e, E>(executor: E, c: &VendorContact) -> sqlx::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let specialties = serde_json::to_value(&c.specialties).unwrap_or(serde_json::json!([]));
    sqlx::query(
        "INSERT INTO vendor_contacts \
            (id, vendor_id, name, role, email, phone, territory, specialties, \
             is_primary, relationship_start, notes, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
         ON CONFLICT (id) DO UPDATE SET \
            name = EXCLUDED.name, role = EXCLUDED.role, \
            email = EXCLUDED.email, phone = EXCLUDED.phone, \
            territory = EXCLUDED.territory, specialties = EXCLUDED.specialties, \
            is_primary = EXCLUDED.is_primary, \
            relationship_start = EXCLUDED.relationship_start, \
            notes = EXCLUDED.notes, updated_at = EXCLUDED.updated_at",
    )
    .bind(&c.id)
    .bind(&c.vendor_id)
    .bind(&c.name)
    .bind(&c.role)
    .bind(&c.email)
    .bind(c.phone.as_deref())
    .bind(c.territory.as_deref())
    .bind(&specialties)
    .bind(c.is_primary)
    .bind(c.relationship_start)
    .bind(c.notes.as_deref())
    .bind(c.created_at)
    .bind(c.updated_at)
    .execute(executor)
    .await
    .map(|_| ())
}

pub(crate) async fn replay_delete_contact<'e, E>(executor: E, id: &str) -> sqlx::Result<u64>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let r = sqlx::query("DELETE FROM vendor_contacts WHERE id = $1")
        .bind(id)
        .execute(executor)
        .await?;
    Ok(r.rows_affected())
}

pub(crate) async fn replay_upsert_interaction<'e, E>(
    executor: E,
    i: &VendorInteraction,
) -> sqlx::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let commitments = serde_json::to_value(&i.commitments).unwrap_or(serde_json::json!([]));
    sqlx::query(
        "INSERT INTO vendor_interactions \
            (id, vendor_id, vendor_contact_id, actor_id, kind, body, commitments, \
             linked_po_id, linked_part_sku, linked_job_id, occurred_at, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
         ON CONFLICT (id) DO UPDATE SET \
            body = EXCLUDED.body, commitments = EXCLUDED.commitments, \
            linked_po_id = EXCLUDED.linked_po_id, \
            linked_part_sku = EXCLUDED.linked_part_sku, \
            linked_job_id = EXCLUDED.linked_job_id",
    )
    .bind(&i.id)
    .bind(&i.vendor_id)
    .bind(i.vendor_contact_id.as_deref())
    .bind(&i.actor_id)
    .bind(&i.kind)
    .bind(&i.body)
    .bind(&commitments)
    .bind(i.linked_po_id.as_deref())
    .bind(i.linked_part_sku.as_deref())
    .bind(i.linked_job_id.as_deref())
    .bind(i.occurred_at)
    .bind(i.created_at)
    .execute(executor)
    .await
    .map(|_| ())
}

pub(crate) async fn replay_soft_delete_interaction<'e, E>(
    executor: E,
    id: &str,
    deleted_by: &str,
    deleted_at: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<u64>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let r = sqlx::query(
        "UPDATE vendor_interactions \
            SET deleted_at = $2, deleted_by = $3 \
          WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .bind(deleted_at)
    .bind(deleted_by)
    .execute(executor)
    .await?;
    Ok(r.rows_affected())
}

pub(crate) async fn replay_upsert_team_member<'e, E>(
    executor: E,
    m: &VendorAccountTeamMember,
) -> sqlx::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "INSERT INTO vendor_account_team \
            (id, vendor_id, employee_id, role, assigned_on, notes, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (vendor_id, role) DO UPDATE SET \
            employee_id = EXCLUDED.employee_id, \
            assigned_on = EXCLUDED.assigned_on, \
            notes = EXCLUDED.notes",
    )
    .bind(&m.id)
    .bind(&m.vendor_id)
    .bind(&m.employee_id)
    .bind(&m.role)
    .bind(m.assigned_on)
    .bind(m.notes.as_deref())
    .bind(m.created_at)
    .execute(executor)
    .await
    .map(|_| ())
}

pub(crate) async fn replay_remove_team_member<'e, E>(
    executor: E,
    vendor_id: &str,
    role: &str,
) -> sqlx::Result<u64>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let r = sqlx::query("DELETE FROM vendor_account_team WHERE vendor_id = $1 AND role = $2")
        .bind(vendor_id)
        .bind(role)
        .execute(executor)
        .await?;
    Ok(r.rows_affected())
}

pub(crate) async fn replay_upsert_contract<'e, E>(
    executor: E,
    c: &VendorContract,
) -> sqlx::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(
        "INSERT INTO vendor_contracts \
            (id, vendor_id, kind, title, effective_on, expires_on, auto_renew, \
             terms, document_uri, status, signed_by_employee_id, signed_at, notes, \
             created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) \
         ON CONFLICT (id) DO UPDATE SET \
            kind = EXCLUDED.kind, title = EXCLUDED.title, \
            effective_on = EXCLUDED.effective_on, expires_on = EXCLUDED.expires_on, \
            auto_renew = EXCLUDED.auto_renew, terms = EXCLUDED.terms, \
            document_uri = EXCLUDED.document_uri, status = EXCLUDED.status, \
            signed_by_employee_id = EXCLUDED.signed_by_employee_id, \
            signed_at = EXCLUDED.signed_at, notes = EXCLUDED.notes, \
            updated_at = EXCLUDED.updated_at",
    )
    .bind(&c.id)
    .bind(&c.vendor_id)
    .bind(&c.kind)
    .bind(&c.title)
    .bind(c.effective_on)
    .bind(c.expires_on)
    .bind(c.auto_renew)
    .bind(&c.terms)
    .bind(c.document_uri.as_deref())
    .bind(&c.status)
    .bind(c.signed_by_employee_id.as_deref())
    .bind(c.signed_at)
    .bind(c.notes.as_deref())
    .bind(c.created_at)
    .bind(c.updated_at)
    .execute(executor)
    .await
    .map(|_| ())
}

// --- row decoders --------------------------------------------------------

fn store(e: sqlx::Error) -> InventoryError {
    InventoryError::Storage(e.to_string())
}

fn row_to_contact(row: &PgRow) -> VendorContact {
    VendorContact {
        id: row.get("id"),
        vendor_id: row.get("vendor_id"),
        name: row.get("name"),
        role: row.get("role"),
        email: row.get("email"),
        phone: row.try_get("phone").ok(),
        territory: row.try_get("territory").ok(),
        specialties: row
            .try_get::<serde_json::Value, _>("specialties")
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default(),
        is_primary: row.get("is_primary"),
        relationship_start: row.try_get("relationship_start").ok(),
        notes: row.try_get("notes").ok(),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn row_to_interaction(row: &PgRow) -> VendorInteraction {
    let commitments: Vec<InteractionCommitment> = row
        .try_get::<serde_json::Value, _>("commitments")
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    VendorInteraction {
        id: row.get("id"),
        vendor_id: row.get("vendor_id"),
        vendor_contact_id: row.try_get("vendor_contact_id").ok(),
        actor_id: row.get("actor_id"),
        kind: row.get("kind"),
        body: row.get("body"),
        commitments,
        linked_po_id: row.try_get("linked_po_id").ok(),
        linked_part_sku: row.try_get("linked_part_sku").ok(),
        linked_job_id: row.try_get("linked_job_id").ok(),
        occurred_at: row.get("occurred_at"),
        created_at: row.get("created_at"),
    }
}

fn row_to_team_member(row: &PgRow) -> VendorAccountTeamMember {
    VendorAccountTeamMember {
        id: row.get("id"),
        vendor_id: row.get("vendor_id"),
        employee_id: row.get("employee_id"),
        role: row.get("role"),
        assigned_on: row.get("assigned_on"),
        notes: row.try_get("notes").ok(),
        created_at: row.get("created_at"),
    }
}

fn row_to_contract(row: &PgRow) -> VendorContract {
    VendorContract {
        id: row.get("id"),
        vendor_id: row.get("vendor_id"),
        kind: row.get("kind"),
        title: row.get("title"),
        effective_on: row.get("effective_on"),
        expires_on: row.try_get("expires_on").ok(),
        auto_renew: row.get("auto_renew"),
        terms: row
            .try_get::<serde_json::Value, _>("terms")
            .unwrap_or(serde_json::json!({})),
        document_uri: row.try_get("document_uri").ok(),
        status: row.get("status"),
        signed_by_employee_id: row.try_get("signed_by_employee_id").ok(),
        signed_at: row.try_get("signed_at").ok(),
        notes: row.try_get("notes").ok(),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}
