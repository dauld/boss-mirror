//! `GET /api/tenant/edit-level` — the instance's hosting edit level,
//! one word off the tenant manifest (a479faf7; design 01c3cc3f reader
//! 3: "read at boss dispatch / the claim door and at the gate").
//!
//! WHY HERE. The level is TENANT data — `[meta] edit_level` in
//! tenant.toml, `boss_core::tenant_manifest` — and an instance fact:
//! the product tree a gate checks out has no tenant.toml at its root,
//! and the instance's tenant (the tenant repo, delivered as a
//! ConfigMap) is in no checkout a gate or a dispatch sees. Both doors
//! already speak to one address, `BOSS_JOBS_URL`; the gateway serves
//! the manifest too, but nothing off the pod knows where the gateway
//! is. So the jobs API answers it, from the file the launcher names
//! for every service (`BOSS_TENANT_MANIFEST_TOML`, derived once from
//! `BOSS_TENANT_DIR` in infra/oss-quickstart/services-launcher.sh),
//! through the ONE reader of that file.
//!
//! Read on every request, like the gateway's own manifest handler, so
//! the state struct's fifty-odd constructors stay untouched and a
//! manifest changed under a running pod answers its new value.
//!
//! The answer is the DECLARED word, never a default: `null` when the
//! manifest declares none or there is no manifest, and the doors
//! enforce nothing on `null` (tenant_manifest.rs says why — every
//! instance today is the operator's own). A manifest that exists but
//! does not parse is a 500 in toml's own words: a gate that cannot
//! read the level refuses rather than reads none.

use std::path::Path;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_core::tenant_manifest::TenantToml;
use serde::Serialize;

/// The env var the launcher sets for every service — the same one the
/// gateway reads its manifest from.
pub const MANIFEST_ENV: &str = "BOSS_TENANT_MANIFEST_TOML";

/// What the door answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditLevelAnswer {
    /// The declared tier name, or `null` for none.
    pub edit_level: Option<String>,
    /// The manifest it was read from, or `null` when none is named.
    pub manifest: Option<String>,
}

/// The pure half: the answer for a manifest path, or the reason it
/// could not be read (the path and toml's own words).
pub fn edit_level_answer(path: Option<&Path>) -> Result<EditLevelAnswer, String> {
    let Some(path) = path else {
        return Ok(EditLevelAnswer {
            edit_level: None,
            manifest: None,
        });
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("reading the tenant manifest {}: {e}", path.display()))?;
    let parsed = TenantToml::parse(&text)
        .map_err(|e| format!("the tenant manifest {} does not parse: {e}", path.display()))?;
    Ok(EditLevelAnswer {
        edit_level: parsed.meta.edit_level,
        manifest: Some(path.display().to_string()),
    })
}

/// `GET /api/tenant/edit-level`. Guest-readable: a gate has no session
/// and the level is one word about the instance, not about anyone.
pub(super) async fn edit_level() -> Response {
    let path = std::env::var_os(MANIFEST_ENV).map(std::path::PathBuf::from);
    match edit_level_answer(path.as_deref()) {
        Ok(answer) => Json(answer).into_response(),
        Err(why) => (StatusCode::INTERNAL_SERVER_ERROR, why).into_response(),
    }
}
