//! Serve step-plugin frontend bundles.
//!
//! Plugins are JavaScript files that register a React component via
//! `window.__boss_register_step_plugin(kind, Component)`. The gateway
//! serves them from `/var/lib/boss/step-plugins/` (configurable via
//! `BOSS_PLUGINS_DIR`), mounted under `/plugins/<filename>`.
//!
//! Session-gated: same cookie check as the SPA itself, since a plugin
//! can read step metadata via the SDK and we don't want unauth'd
//! callers grabbing them.
//!
//! Q2 of the step-ux-plugin-model design: files on disk, not DB
//! BYTEA — matches every other static asset in the repo.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::AppState;
use boss_gateway::session::{self, find_cookie};

/// Resolve the plugins directory (cached via env on first call).
pub fn plugins_dir() -> &'static str {
    static DIR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var("BOSS_PLUGINS_DIR")
            .unwrap_or_else(|_| "/var/lib/boss/step-plugins".to_string())
    })
}

pub async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    // Require a valid session. The SPA loads plugin scripts after
    // login; unauthenticated access returns 401 rather than a redirect
    // (browsers can't follow HTML redirects from `<script>` tags).
    if !has_valid_session(req.headers(), &state.session_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let path = req.uri().path();
    let rel = match path.strip_prefix("/plugins/") {
        Some(r) if !r.is_empty() => r,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };

    // Reject path traversal. The strip_prefix above already removed
    // `/plugins/`; any `..` past that point would escape the dir.
    if rel.contains("..") || rel.starts_with('/') {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let mut file_path = PathBuf::from(plugins_dir());
    file_path.push(rel);

    // Only serve .js files. MIME sniffers treat untyped downloads as
    // text/plain; being explicit avoids browsers refusing a module.
    let is_js = file_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("js"))
        .unwrap_or(false);
    if !is_js {
        return StatusCode::NOT_FOUND.into_response();
    }

    match tokio::fs::read(&file_path).await {
        Ok(bytes) => {
            let mut resp = (StatusCode::OK, bytes).into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/javascript; charset=utf-8"),
            );
            // Short cache — plugin bundles are versioned by content
            // hash at publish time; cache-bust on edit via the
            // filename, not this header.
            resp.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=60"),
            );
            resp
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn has_valid_session(headers: &HeaderMap, key: &[u8]) -> bool {
    let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(raw) = find_cookie(cookie, session::COOKIE_NAME) else {
        return false;
    };
    session::Session::decode(raw, key).is_ok()
}
