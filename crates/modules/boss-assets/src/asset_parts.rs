//! Per-asset software config + accessories — the Subject-level Parts
//! on a Asset.
//!
//! Parts are Subjects in their own right
//! (docs/architecture-decisions.md §Primitives & information
//! architecture). Every installed unit
//! carries per-instance state that the catalog Model doesn't capture:
//! firmware version + module set + license tier (the "software
//! config"), and the set of attached accessories (the
//! tenant-defined unit-level installable parts — transceivers on a
//! switch, toner heads on a printer, agitators on a fermenter, etc.).
//! This module owns the two backing tables + the Parts-flavored read
//! surface.
//!
//! Exposed endpoints:
//!
//! - `GET  /api/assets/assets/{id}/parts` — merged `Vec<Part>`
//! - `GET  /api/assets/assets/{id}/software-config` — single row or 404
//! - `POST /api/assets/assets/{id}/software-config` — upsert (sim)
//! - `GET  /api/assets/assets/{id}/accessories` — installed accessories
//! - `POST /api/assets/assets/{id}/accessories` — append (sim)
//!
//! The POST endpoints are how `boss-sim`'s `intake` generator populates
//! these during a 12-month replay. No auth on writes in this wave —
//! a follow-up can gate them behind CurrentUser + policy once the UI
//! edit flows exist.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_core::primitives::Part;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Clone)]
pub struct SystemPartsState {
    pub pool: Arc<PgPool>,
}

pub fn asset_parts_router(pool: PgPool) -> Router {
    let state = SystemPartsState {
        pool: Arc::new(pool),
    };
    Router::new()
        .route("/api/assets/assets/{asset_id}/parts", get(list_parts))
        .route(
            "/api/assets/assets/{asset_id}/software-config",
            get(get_software_config).post(upsert_software_config),
        )
        .route(
            "/api/assets/assets/{asset_id}/accessories",
            get(list_accessories).post(append_accessory),
        )
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SoftwareConfig {
    pub asset_id: String,
    pub firmware_version: String,
    pub modules: serde_json::Value,
    pub license_tier: String,
    pub last_updated_on: Option<NaiveDate>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Accessory {
    pub id: i64,
    pub asset_id: String,
    pub accessory_kind: String,
    pub serial: Option<String>,
    pub installed_on: Option<NaiveDate>,
    pub removed_on: Option<NaiveDate>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpsertSoftwareConfigRequest {
    pub firmware_version: String,
    #[serde(default)]
    pub modules: Vec<String>,
    #[serde(default = "default_license_tier")]
    pub license_tier: String,
    pub last_updated_on: Option<NaiveDate>,
}

fn default_license_tier() -> String {
    "standard".to_string()
}

#[derive(Debug, Deserialize)]
pub struct AppendAccessoryRequest {
    pub accessory_kind: String,
    pub serial: Option<String>,
    pub installed_on: Option<NaiveDate>,
    pub notes: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Unified Parts view. Merges the single software_config row (as one
/// AttributePart) with the N installed accessories (one AttributePart
/// each). Returns an empty
/// `[]` for assets that haven't been seeded yet rather than a
/// 404 — the UI can show "no data" without a special case.
async fn list_parts(
    State(state): State<SystemPartsState>,
    Path(asset_id): Path<String>,
) -> Response {
    let mut parts: Vec<Part> = Vec::new();

    // Software config (0 or 1 rows).
    match fetch_software_config(&state.pool, &asset_id).await {
        Ok(Some(cfg)) => parts.push(software_config_to_part(&cfg)),
        Ok(None) => {}
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }

    // Installed accessories (0..n). Only currently-installed (not
    // removed) rows show up in the live Parts view; removed ones
    // are history for a future "swap log" endpoint.
    match fetch_installed_accessories(&state.pool, &asset_id).await {
        Ok(accs) => {
            for a in &accs {
                parts.push(accessory_to_part(a));
            }
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }

    Json(parts).into_response()
}

async fn get_software_config(
    State(state): State<SystemPartsState>,
    Path(asset_id): Path<String>,
) -> Response {
    match fetch_software_config(&state.pool, &asset_id).await {
        Ok(Some(cfg)) => Json(cfg).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no software config").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn upsert_software_config(
    State(state): State<SystemPartsState>,
    Path(asset_id): Path<String>,
    Json(req): Json<UpsertSoftwareConfigRequest>,
) -> Response {
    let modules = serde_json::to_value(&req.modules).unwrap_or(serde_json::json!([]));
    let result = sqlx::query(
        "INSERT INTO asset_software_configs
             (asset_id, firmware_version, modules, license_tier, last_updated_on, updated_at)
         VALUES ($1, $2, $3, $4, $5, NOW())
         ON CONFLICT (asset_id) DO UPDATE SET
             firmware_version = EXCLUDED.firmware_version,
             modules = EXCLUDED.modules,
             license_tier = EXCLUDED.license_tier,
             last_updated_on = EXCLUDED.last_updated_on,
             updated_at = NOW()",
    )
    .bind(&asset_id)
    .bind(&req.firmware_version)
    .bind(&modules)
    .bind(&req.license_tier)
    .bind(req.last_updated_on)
    .execute(state.pool.as_ref())
    .await;

    match result {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn list_accessories(
    State(state): State<SystemPartsState>,
    Path(asset_id): Path<String>,
) -> Response {
    match fetch_installed_accessories(&state.pool, &asset_id).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn append_accessory(
    State(state): State<SystemPartsState>,
    Path(asset_id): Path<String>,
    Json(req): Json<AppendAccessoryRequest>,
) -> Response {
    let result = sqlx::query(
        "INSERT INTO asset_accessories
             (asset_id, accessory_kind, serial, installed_on, notes)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&asset_id)
    .bind(&req.accessory_kind)
    .bind(&req.serial)
    .bind(req.installed_on)
    .bind(&req.notes)
    .execute(state.pool.as_ref())
    .await;

    match result {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

async fn fetch_software_config(
    pool: &PgPool,
    asset_id: &str,
) -> Result<Option<SoftwareConfig>, String> {
    sqlx::query_as::<_, SoftwareConfig>(
        "SELECT asset_id, firmware_version, modules, license_tier, last_updated_on, updated_at
         FROM asset_software_configs
         WHERE asset_id = $1",
    )
    .bind(asset_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())
}

async fn fetch_installed_accessories(
    pool: &PgPool,
    asset_id: &str,
) -> Result<Vec<Accessory>, String> {
    sqlx::query_as::<_, Accessory>(
        "SELECT id, asset_id, accessory_kind, serial, installed_on, removed_on, notes
         FROM asset_accessories
         WHERE asset_id = $1 AND removed_on IS NULL
         ORDER BY installed_on NULLS LAST, id",
    )
    .bind(asset_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Part conversion
// ---------------------------------------------------------------------------

pub fn software_config_to_part(cfg: &SoftwareConfig) -> Part {
    Part::attribute(
        "software_config",
        serde_json::to_value(cfg).expect("SoftwareConfig serialises"),
    )
}

pub fn accessory_to_part(acc: &Accessory) -> Part {
    Part::attribute(
        "accessory",
        serde_json::to_value(acc).expect("Accessory serialises"),
    )
}

#[cfg(test)]
mod part_conversion_tests {
    use super::*;

    #[test]
    fn software_config_becomes_attribute_part() {
        let cfg = SoftwareConfig {
            asset_id: "SYS-0001".into(),
            firmware_version: "2.3.1".into(),
            modules: serde_json::json!(["erbium-driver", "diode-driver"]),
            license_tier: "pro".into(),
            last_updated_on: NaiveDate::from_ymd_opt(2026, 3, 15),
            updated_at: chrono::Utc::now(),
        };
        let p = software_config_to_part(&cfg);
        match p {
            Part::Attribute { key, value } => {
                assert_eq!(key, "software_config");
                assert_eq!(
                    value.get("firmware_version").and_then(|v| v.as_str()),
                    Some("2.3.1"),
                );
                assert_eq!(
                    value.get("license_tier").and_then(|v| v.as_str()),
                    Some("pro"),
                );
            }
            Part::Subject { .. } => panic!("software_config should be AttributePart"),
        }
    }

    #[test]
    fn accessory_becomes_attribute_part() {
        let acc = Accessory {
            id: 42,
            asset_id: "SYS-0001".into(),
            accessory_kind: "erbium".into(),
            serial: Some("HP-ER-001".into()),
            installed_on: NaiveDate::from_ymd_opt(2026, 1, 10),
            removed_on: None,
            notes: None,
        };
        let p = accessory_to_part(&acc);
        match p {
            Part::Attribute { key, value } => {
                assert_eq!(key, "accessory");
                assert_eq!(
                    value.get("accessory_kind").and_then(|v| v.as_str()),
                    Some("erbium"),
                );
            }
            Part::Subject { .. } => panic!("accessory should be AttributePart"),
        }
    }
}
