//! Postgres adapter for `KbRepository`.
//!
//! Queries `asset_models` and its satellite tables (wavelengths, spot
//! sizes, indications, failure modes, preventive maintenance checklist, spare parts,
//! consumables, documents) and assembles them into `AssetModel` structs.
//!
//! The catalog is small (< 20 models) so we fetch eagerly and assemble
//! in Rust rather than using complex JOINs.

use async_trait::async_trait;
use sqlx::PgPool;

use crate::port::{KbError, KbRepository};
use crate::types::*;

pub struct PgKb {
    pool: PgPool,
}

impl PgKb {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Serialize an enum to its serde kebab-case string.
pub(crate) fn to_kebab<T: serde::Serialize>(val: &T) -> String {
    serde_json::to_value(val)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

#[async_trait]
impl KbRepository for PgKb {
    async fn all_models(&self) -> Result<Vec<AssetModel>, KbError> {
        let rows: Vec<ModelRow> = sqlx::query_as("SELECT * FROM asset_models ORDER BY sku")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;

        let mut models = Vec::with_capacity(rows.len());
        for row in rows {
            let model = self.assemble(row).await?;
            models.push(model);
        }
        Ok(models)
    }

    async fn model_by_sku(&self, sku: &str) -> Result<Option<AssetModel>, KbError> {
        let row: Option<ModelRow> = sqlx::query_as("SELECT * FROM asset_models WHERE sku = $1")
            .bind(sku)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;

        match row {
            Some(r) => Ok(Some(self.assemble(r).await?)),
            None => Ok(None),
        }
    }

    async fn create_model_at(
        &self,
        model: &AssetModel,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<String, KbError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;
        // Check for duplicate.
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM asset_models WHERE sku = $1)")
                .bind(&model.sku)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| KbError::Storage(e.to_string()))?;
        if exists {
            return Err(KbError::Conflict(format!(
                "SKU {} already exists",
                model.sku
            )));
        }
        crate::extras_schema::validate_extras(
            &self.pool,
            &crate::postgres::to_kebab(&model.category),
            &model.extras,
        )
        .await?;
        upsert_model_row(&mut tx, model, now).await?;
        insert_satellites(&mut tx, model).await?;
        // OUTBOX (phase 2): the created event (full row state)
        // records with the rows.
        let event = stamp.event(
            crate::events::MODEL_CREATED,
            serde_json::to_value(model).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(KbError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;
        Ok(model.sku.clone())
    }

    async fn update_model_at(
        &self,
        sku: &str,
        model: &AssetModel,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), KbError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;
        // Verify exists.
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM asset_models WHERE sku = $1)")
                .bind(sku)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| KbError::Storage(e.to_string()))?;
        if !exists {
            return Err(KbError::NotFound(sku.to_string()));
        }
        crate::extras_schema::validate_extras(
            &self.pool,
            &crate::postgres::to_kebab(&model.category),
            &model.extras,
        )
        .await?;
        // UPSERT preserves `created_at` (load-bearing for rebuild
        // equality). Satellites still get full replacement —
        // they're junction tables with composite keys, no per-row
        // id we can UPSERT against.
        upsert_model_row(&mut tx, model, now).await?;
        delete_satellites(&mut tx, sku).await?;
        insert_satellites(&mut tx, model).await?;
        // OUTBOX (phase 2): the updated event (full row state)
        // records with the rows.
        let event = stamp.event(
            crate::events::MODEL_UPDATED,
            serde_json::to_value(model).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(KbError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn delete_model_at(
        &self,
        sku: &str,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), KbError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;
        delete_satellites(&mut tx, sku).await?;
        let result = sqlx::query("DELETE FROM asset_models WHERE sku = $1")
            .bind(sku)
            .execute(&mut *tx)
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(KbError::NotFound(sku.to_string()));
        }
        // OUTBOX (phase 2): the deleted event records only after the
        // row actually deleted (NotFound above returns pre-recording).
        let event = stamp.event(
            crate::events::MODEL_DELETED,
            serde_json::json!({ "sku": sku, "deleted_at": now }),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(KbError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn all_parts(&self) -> Result<Vec<crate::types::PartCatalogRow>, KbError> {
        use crate::types::PartCatalogRow;
        // Union descriptive parts catalog with every inventory SKU
        // that lacks one — synthesizes a humanized name from the
        // SKU so the SPA's Part detail page renders for every
        // stocked item. Tenants that seed real catalog rows (device
        // refurb) shadow the synthesized rows; tenants that only
        // seed inventory_items (brewery) get auto-populated stubs.
        let rows: Vec<(String, String, String, i64, String, i16)> = sqlx::query_as(
            "SELECT part_sku, name, description, unit_price_cents, currency, lead_time_days \
             FROM parts \
             UNION ALL \
             SELECT i.part_sku, \
                    initcap(replace(i.part_sku, '-', ' ')) AS name, \
                    'Stocked inventory item (catalog metadata not seeded)' AS description, \
                    i.avg_cost_cents AS unit_price_cents, \
                    'USD' AS currency, \
                    7::int2 AS lead_time_days \
             FROM inventory_items i \
             WHERE NOT EXISTS (SELECT 1 FROM parts p WHERE p.part_sku = i.part_sku) \
             ORDER BY part_sku",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KbError::Storage(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(
                |(part_sku, name, description, unit_price_cents, currency, lead_time_days)| {
                    PartCatalogRow {
                        part_sku,
                        name,
                        description,
                        unit_price_cents,
                        currency,
                        lead_time_days: lead_time_days as u16,
                    }
                },
            )
            .collect())
    }

    async fn documents_for(
        &self,
        entity_kind: &str,
        entity_id: &str,
    ) -> Result<Vec<crate::types::EntityDocument>, KbError> {
        use crate::types::EntityDocument;
        let rows: Vec<(
            uuid::Uuid,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            Option<chrono::DateTime<chrono::Utc>>,
        )> = sqlx::query_as(
            "SELECT id, doc_type, title, url, version, audience, uploaded_at \
                 FROM documents \
                 WHERE entity_kind = $1 AND entity_id = $2 \
                 ORDER BY uploaded_at DESC NULLS LAST, title",
        )
        .bind(entity_kind)
        .bind(entity_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KbError::Storage(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(
                |(id, doc_type, title, url, version, audience, uploaded_at)| EntityDocument {
                    id: id.to_string(),
                    doc_type,
                    title,
                    url,
                    version,
                    audience,
                    uploaded_at,
                },
            )
            .collect())
    }
}

impl PgKb {
    /// Fetch all satellite data for a SKU and assemble a full `AssetModel`.
    async fn assemble(&self, row: ModelRow) -> Result<AssetModel, KbError> {
        let sku = &row.sku;

        let (use_cases, failure_modes, checklist, spare_parts, consumables, documents) = tokio::try_join!(
            self.fetch_use_cases(sku),
            self.fetch_failure_modes(sku),
            self.fetch_pm_checklist(sku),
            self.fetch_spare_parts(sku),
            self.fetch_consumables(sku),
            self.fetch_documents(sku),
        )?;

        Ok(row.into_asset_model(
            use_cases,
            failure_modes,
            checklist,
            spare_parts,
            consumables,
            documents,
        ))
    }

    async fn fetch_use_cases(&self, sku: &str) -> Result<Vec<String>, KbError> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT use_case FROM asset_use_cases WHERE sku = $1 ORDER BY use_case")
                .bind(sku)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| KbError::Storage(e.to_string()))?;

        Ok(rows.into_iter().map(|(s,)| s).collect())
    }

    async fn fetch_failure_modes(&self, sku: &str) -> Result<Vec<FailureMode>, KbError> {
        let rows: Vec<FailureModeRow> = sqlx::query_as(
            "SELECT code, name, frequency, typical_fix FROM asset_failure_modes WHERE sku = $1 ORDER BY code",
        )
        .bind(sku)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KbError::Storage(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| FailureMode {
                code: r.code,
                name: r.name,
                frequency: r.frequency,
                typical_fix: r.typical_fix,
            })
            .collect())
    }

    async fn fetch_pm_checklist(&self, sku: &str) -> Result<Vec<String>, KbError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT item FROM asset_pm_checklist WHERE sku = $1 ORDER BY sort_order",
        )
        .bind(sku)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KbError::Storage(e.to_string()))?;

        Ok(rows.into_iter().map(|(s,)| s).collect())
    }

    async fn fetch_spare_parts(&self, sku: &str) -> Result<Vec<SparePart>, KbError> {
        let rows: Vec<SparePartRow> = sqlx::query_as(
            r#"
            SELECT p.part_sku, p.name, p.description, p.unit_price_cents, p.currency, p.lead_time_days, sp.high_usage
            FROM asset_spare_parts sp
            JOIN parts p ON p.part_sku = sp.part_sku
            WHERE sp.sku = $1
            ORDER BY p.part_sku
            "#,
        )
        .bind(sku)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KbError::Storage(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| SparePart {
                part_sku: r.part_sku,
                name: r.name,
                description: r.description,
                unit_price_cents: r.unit_price_cents,
                currency: r.currency,
                lead_time_days: r.lead_time_days as u16,
                high_usage: r.high_usage,
            })
            .collect())
    }

    async fn fetch_consumables(&self, sku: &str) -> Result<Vec<Consumable>, KbError> {
        let rows: Vec<ConsumableRow> = sqlx::query_as(
            r#"
            SELECT p.part_sku, p.name, p.description, p.unit_price_cents, p.currency, dc.treatments_per_unit
            FROM asset_consumables dc
            JOIN parts p ON p.part_sku = dc.part_sku
            WHERE dc.sku = $1
            ORDER BY p.part_sku
            "#,
        )
        .bind(sku)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KbError::Storage(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| Consumable {
                part_sku: r.part_sku,
                name: r.name,
                description: r.description,
                unit_price_cents: r.unit_price_cents,
                currency: r.currency,
                treatments_per_unit: r.treatments_per_unit.map(|v| v as u32),
            })
            .collect())
    }

    async fn fetch_documents(&self, sku: &str) -> Result<Vec<Document>, KbError> {
        let rows: Vec<DocumentRow> = sqlx::query_as(
            "SELECT kind, title, url, version, published, audience FROM asset_documents WHERE sku = $1 ORDER BY id",
        )
        .bind(sku)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| KbError::Storage(e.to_string()))?;

        rows.into_iter()
            .map(|r| {
                Ok(Document {
                    kind: DocumentKind(r.kind),
                    title: r.title,
                    url: r.url,
                    version: r.version,
                    published: r.published,
                    audience: DocumentAudience::new(&r.audience),
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Write helpers — insert/delete for model + satellite tables
// ---------------------------------------------------------------------------

pub(crate) async fn upsert_model_row(
    tx: &mut sqlx::PgConnection,
    m: &AssetModel,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), KbError> {
    sqlx::query(
        "INSERT INTO asset_models (sku, name, manufacturer, model_year, category, \
         list_price_new_cents, typical_refurb_price_cents, currency, lead_time_days, tagline, description, hero_image, \
         width_cm, depth_cm, height_cm, weight_kg, power_requirements, \
         clearance_id, clearance_date, regulator_device_class, \
         preventive_maintenance_hours, preventive_maintenance_interval_months, calibration_interval_months, required_skill_level, depot_required, \
         end_of_support, current_firmware, extras, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,\
                 $21,$22,$23,$24,$25,$26,$27,$28,$29,$29) \
         ON CONFLICT (sku) DO UPDATE SET \
            name = EXCLUDED.name, manufacturer = EXCLUDED.manufacturer, model_year = EXCLUDED.model_year, \
            category = EXCLUDED.category, \
            list_price_new_cents = EXCLUDED.list_price_new_cents, \
            typical_refurb_price_cents = EXCLUDED.typical_refurb_price_cents, \
            currency = EXCLUDED.currency, lead_time_days = EXCLUDED.lead_time_days, \
            tagline = EXCLUDED.tagline, description = EXCLUDED.description, hero_image = EXCLUDED.hero_image, \
            width_cm = EXCLUDED.width_cm, depth_cm = EXCLUDED.depth_cm, height_cm = EXCLUDED.height_cm, \
            weight_kg = EXCLUDED.weight_kg, power_requirements = EXCLUDED.power_requirements, \
            clearance_id = EXCLUDED.clearance_id, clearance_date = EXCLUDED.clearance_date, \
            regulator_device_class = EXCLUDED.regulator_device_class, \
            preventive_maintenance_hours = EXCLUDED.preventive_maintenance_hours, preventive_maintenance_interval_months = EXCLUDED.preventive_maintenance_interval_months, \
            calibration_interval_months = EXCLUDED.calibration_interval_months, \
            required_skill_level = EXCLUDED.required_skill_level, \
            depot_required = EXCLUDED.depot_required, \
            end_of_support = EXCLUDED.end_of_support, current_firmware = EXCLUDED.current_firmware, \
            extras = EXCLUDED.extras, \
            updated_at = EXCLUDED.updated_at",
    )
    .bind(&m.sku)
    .bind(&m.name)
    .bind(&m.manufacturer)
    .bind(m.model_year as i16)
    .bind(to_kebab(&m.category))
    .bind(m.commerce.list_price_new_cents)
    .bind(m.commerce.typical_refurb_price_cents)
    .bind(&m.commerce.currency)
    .bind(m.commerce.lead_time_days.map(|d| d as i16))
    .bind(&m.commerce.tagline)
    .bind(&m.commerce.description)
    .bind(&m.commerce.hero_image)
    .bind(m.physical.width_cm)
    .bind(m.physical.depth_cm)
    .bind(m.physical.height_cm)
    .bind(m.physical.weight_kg)
    .bind(&m.physical.power_requirements)
    .bind(&m.regulatory.clearance_id)
    .bind(m.regulatory.clearance_date)
    .bind(m.regulatory.regulator_device_class as i16)
    .bind(m.service.preventive_maintenance_hours)
    .bind(m.service.preventive_maintenance_interval_months as i16)
    .bind(m.service.calibration_interval_months as i16)
    .bind(m.service.required_skill_level as i16)
    .bind(m.service.depot_required)
    .bind(m.end_of_support)
    .bind(&m.current_firmware)
    .bind(&m.extras)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|e| KbError::Storage(e.to_string()))?;
    Ok(())
}

pub(crate) async fn insert_satellites(
    tx: &mut sqlx::PgConnection,
    m: &AssetModel,
) -> Result<(), KbError> {
    let sku = &m.sku;

    for use_case in &m.commerce.use_cases {
        sqlx::query("INSERT INTO asset_use_cases (sku, use_case) VALUES ($1, $2)")
            .bind(sku)
            .bind(use_case)
            .execute(&mut *tx)
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;
    }
    for fm in &m.service.common_failure_modes {
        sqlx::query("INSERT INTO asset_failure_modes (sku, code, name, frequency, typical_fix) VALUES ($1,$2,$3,$4,$5)")
            .bind(sku).bind(&fm.code).bind(&fm.name).bind(fm.frequency).bind(&fm.typical_fix)
            .execute(&mut *tx).await.map_err(|e| KbError::Storage(e.to_string()))?;
    }
    for (i, item) in m.service.pm_checklist.iter().enumerate() {
        sqlx::query("INSERT INTO asset_pm_checklist (sku, sort_order, item) VALUES ($1,$2,$3)")
            .bind(sku)
            .bind((i + 1) as i32)
            .bind(item)
            .execute(&mut *tx)
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;
    }
    for sp in &m.spare_parts {
        // Ensure part exists in parts table.
        sqlx::query(
            "INSERT INTO parts (part_sku, name, description, unit_price_cents, currency, lead_time_days) \
             VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (part_sku) DO UPDATE SET \
             name=EXCLUDED.name, description=EXCLUDED.description, \
             unit_price_cents=EXCLUDED.unit_price_cents, currency=EXCLUDED.currency, \
             lead_time_days=EXCLUDED.lead_time_days",
        )
        .bind(&sp.part_sku)
        .bind(&sp.name)
        .bind(&sp.description)
        .bind(sp.unit_price_cents)
        .bind(&sp.currency)
        .bind(sp.lead_time_days as i16)
        .execute(&mut *tx)
        .await
        .map_err(|e| KbError::Storage(e.to_string()))?;

        sqlx::query("INSERT INTO asset_spare_parts (sku, part_sku, high_usage) VALUES ($1,$2,$3)")
            .bind(sku)
            .bind(&sp.part_sku)
            .bind(sp.high_usage)
            .execute(&mut *tx)
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;
    }
    for cs in &m.consumables {
        sqlx::query(
            "INSERT INTO parts (part_sku, name, description, unit_price_cents, currency, lead_time_days) \
             VALUES ($1,$2,$3,$4,$5,7) ON CONFLICT (part_sku) DO UPDATE SET \
             name=EXCLUDED.name, description=EXCLUDED.description, \
             unit_price_cents=EXCLUDED.unit_price_cents, currency=EXCLUDED.currency",
        )
        .bind(&cs.part_sku)
        .bind(&cs.name)
        .bind(&cs.description)
        .bind(cs.unit_price_cents)
        .bind(&cs.currency)
        .execute(&mut *tx)
        .await
        .map_err(|e| KbError::Storage(e.to_string()))?;

        sqlx::query(
            "INSERT INTO asset_consumables (sku, part_sku, treatments_per_unit) VALUES ($1,$2,$3)",
        )
        .bind(sku)
        .bind(&cs.part_sku)
        .bind(cs.treatments_per_unit.map(|t| t as i32))
        .execute(&mut *tx)
        .await
        .map_err(|e| KbError::Storage(e.to_string()))?;
    }
    for doc in &m.documents {
        sqlx::query(
            "INSERT INTO asset_documents (sku, kind, title, url, version, published, audience) \
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(sku)
        .bind(doc.kind.as_str())
        .bind(&doc.title)
        .bind(&doc.url)
        .bind(&doc.version)
        .bind(doc.published)
        .bind(doc.audience.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| KbError::Storage(e.to_string()))?;
    }
    Ok(())
}

pub(crate) async fn delete_satellites(
    tx: &mut sqlx::PgConnection,
    sku: &str,
) -> Result<(), KbError> {
    let tables = [
        "asset_use_cases",
        "asset_failure_modes",
        "asset_pm_checklist",
        "asset_spare_parts",
        "asset_consumables",
        "asset_documents",
    ];
    for table in tables {
        sqlx::query(&format!("DELETE FROM {table} WHERE sku = $1"))
            .bind(sku)
            .execute(&mut *tx)
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct ModelRow {
    sku: String,
    name: String,
    manufacturer: String,
    model_year: i16,
    category: String,
    // Commerce
    list_price_new_cents: i64,
    typical_refurb_price_cents: Option<i64>,
    currency: String,
    lead_time_days: Option<i16>,
    tagline: String,
    description: String,
    hero_image: Option<String>,
    // Physical
    width_cm: f32,
    depth_cm: f32,
    height_cm: f32,
    weight_kg: f32,
    power_requirements: String,
    // Regulatory
    clearance_id: Option<String>,
    clearance_date: Option<chrono::NaiveDate>,
    regulator_device_class: i16,
    // Service
    preventive_maintenance_hours: f32,
    preventive_maintenance_interval_months: i16,
    calibration_interval_months: i16,
    required_skill_level: i16,
    depot_required: bool,
    // Support
    end_of_support: Option<chrono::NaiveDate>,
    current_firmware: Option<String>,
    // Tenant-defined kind-specific specs (D2 + D5).
    extras: serde_json::Value,
    // No `created_at` / `updated_at`: the domain type carries neither,
    // and `FromRow` ignores columns it has no field for.
}

impl ModelRow {
    #[allow(clippy::too_many_arguments)]
    fn into_asset_model(
        self,
        use_cases: Vec<String>,
        failure_modes: Vec<FailureMode>,
        pm_checklist: Vec<String>,
        spare_parts: Vec<SparePart>,
        consumables: Vec<Consumable>,
        documents: Vec<Document>,
    ) -> AssetModel {
        AssetModel {
            sku: self.sku,
            name: self.name,
            manufacturer: self.manufacturer,
            model_year: self.model_year as u16,
            category: DeviceCategory::new(self.category),
            extras: self.extras,
            physical: Physical {
                width_cm: self.width_cm,
                depth_cm: self.depth_cm,
                height_cm: self.height_cm,
                weight_kg: self.weight_kg,
                power_requirements: self.power_requirements,
            },
            regulatory: Regulatory {
                clearance_id: self.clearance_id,
                clearance_date: self.clearance_date,
                regulator_device_class: self.regulator_device_class as u8,
            },
            commerce: Commerce {
                list_price_new_cents: self.list_price_new_cents,
                typical_refurb_price_cents: self.typical_refurb_price_cents,
                currency: self.currency,
                lead_time_days: self.lead_time_days.map(|v| v as u16),
                tagline: self.tagline,
                description: self.description,
                use_cases,
                hero_image: self.hero_image,
            },
            service: ServiceProfile {
                preventive_maintenance_hours: self.preventive_maintenance_hours,
                preventive_maintenance_interval_months: self.preventive_maintenance_interval_months
                    as u8,
                calibration_interval_months: self.calibration_interval_months as u8,
                required_skill_level: self.required_skill_level as u8,
                depot_required: self.depot_required,
                common_failure_modes: failure_modes,
                pm_checklist,
            },
            spare_parts,
            consumables,
            documents,
            end_of_support: self.end_of_support,
            current_firmware: self.current_firmware,
        }
    }
}

#[derive(sqlx::FromRow)]
struct FailureModeRow {
    code: String,
    name: String,
    frequency: f32,
    typical_fix: String,
}

#[derive(sqlx::FromRow)]
struct SparePartRow {
    part_sku: String,
    name: String,
    description: String,
    unit_price_cents: i64,
    currency: String,
    lead_time_days: i16,
    high_usage: bool,
}

#[derive(sqlx::FromRow)]
struct ConsumableRow {
    part_sku: String,
    name: String,
    description: String,
    unit_price_cents: i64,
    currency: String,
    treatments_per_unit: Option<i32>,
}

#[derive(sqlx::FromRow)]
struct DocumentRow {
    kind: String,
    title: String,
    url: String,
    version: Option<String>,
    published: Option<chrono::NaiveDate>,
    audience: String,
}

// ---------------------------------------------------------------------------
// Enum parsing helpers
// ---------------------------------------------------------------------------

// DeviceCategory, DocumentKind, and DocumentAudience are free-text
// wrappers, lifted to the Class registry under subject_kind='asset'
// (member_attribute 'category' / 'document-kind' / 'document-audience').
// Validation against the active Class set lives at the API boundary, not
// the storage adapter, so this layer trusts whatever the DB carries.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_category_round_trips_through_serde() {
        // Free-text wrapper serializes transparently to a bare
        // string and round-trips back. Validation against the
        // active Class set lives at the API boundary, not here.
        let cat = DeviceCategory::new("router");
        let s = serde_json::to_string(&cat).unwrap();
        assert_eq!(s, "\"router\"");
        let back: DeviceCategory = serde_json::from_str(&s).unwrap();
        assert_eq!(back, cat);
    }

    #[test]
    fn document_kind_round_trips_through_serde() {
        // Free-text wrapper serializes transparently to a bare string
        // and round-trips back. Validation against the active Class set
        // lives at the API boundary, not here.
        let k = DocumentKind::new("operator-manual");
        let s = serde_json::to_string(&k).unwrap();
        assert_eq!(s, "\"operator-manual\"");
        let back: DocumentKind = serde_json::from_str(&s).unwrap();
        assert_eq!(back, k);
    }

    #[test]
    fn document_audience_round_trips_through_serde() {
        // Free-text wrapper serializes transparently to a bare string
        // and round-trips back — same contract as DocumentKind.
        let a = DocumentAudience::new("partner");
        let s = serde_json::to_string(&a).unwrap();
        assert_eq!(s, "\"partner\"");
        let back: DocumentAudience = serde_json::from_str(&s).unwrap();
        assert_eq!(back, a);
    }
}
