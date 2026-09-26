//! A stand-in for the forge's admin token API, for the tests that drive
//! the credential broker through its REAL adapter (`ForgejoAdmin`)
//! rather than an in-memory issuer — because the three defects the
//! round-3 review of car 85b7b55f found (2026-09-26) all live in the
//! adapter's contract with the forge, where a fake issuer cannot see
//! them: a listing that stopped at one page (F1c), a DELETE whose path
//! carried a string from job metadata (F1d; reqwest resolves `..`
//! segments, so `tokens/../../../../repos/david/boss` reaches
//! `/api/v1/repos/david/boss`), and the ledger both are judged against.
//!
//! One catch-all route, so EVERY request is logged exactly as the forge
//! would receive it — method and normalised path — including the ones
//! no real token route would match. It answers as Forgejo does:
//!   GET    /api/v1/admin/users/{u}/tokens?page=&limit=  newest first,
//!          `limit` clamped to `max_limit` (Forgejo's MAX_RESPONSE_ITEMS),
//!          `X-Total-Count` set
//!   POST   /api/v1/admin/users/{u}/tokens               mint
//!   DELETE /api/v1/admin/users/{u}/tokens/{ref}         a number is an
//!          id, anything else a name (Forgejo's own parse)
//!   GET    /api/v1/repos/{o}/{r}                        200 when the
//!          `token` header is a live token's value
//!   DELETE anything else                                204 — the forge
//!          would have done whatever that path means
//!
//! Test-only: declared `#[cfg(test)]` in `handlers/mod.rs`.

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// One token as the forge holds it: the value lives here and nowhere
/// else, exactly as on the real forge.
#[derive(Debug, Clone)]
pub(crate) struct StubToken {
    pub id: i64,
    pub name: String,
    pub value: String,
}

impl StubToken {
    pub fn new(id: i64, name: &str, value: &str) -> Self {
        Self {
            id,
            name: name.into(),
            value: value.into(),
        }
    }
}

#[derive(Default)]
pub(crate) struct ForgeState {
    pub tokens: Vec<StubToken>,
    /// Forgejo's MAX_RESPONSE_ITEMS: a `limit` above it is clamped.
    pub max_limit: usize,
    /// A forge that answers every page with page one.
    pub ignores_page: bool,
    /// An `X-Total-Count` other than the tokens it holds — a ledger that
    /// moved while it was being read.
    pub claims_total: Option<usize>,
    /// Every request, as `"<METHOD> <path>"` (query stripped).
    pub requests: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct ForgeStub {
    pub url: String,
    pub state: Arc<Mutex<ForgeState>>,
}

impl ForgeStub {
    pub fn listed(&self, name: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .tokens
            .iter()
            .any(|t| t.name == name)
    }

    /// Every DELETE the forge received, as its path.
    pub fn deletes(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter_map(|r| r.strip_prefix("DELETE ").map(str::to_string))
            .collect()
    }

    pub fn mints(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.starts_with("POST "))
            .count()
    }
}

fn last_eight(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars[chars.len().saturating_sub(8)..].iter().collect()
}

fn query_usize(uri: &Uri, key: &str) -> Option<usize> {
    uri.query()?
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.parse().ok())
}

fn answer(
    state: &Mutex<ForgeState>,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    let path = uri.path().to_string();
    let mut st = state.lock().unwrap();
    st.requests.push(format!("{method} {path}"));
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (method.clone(), parts.as_slice()) {
        (Method::GET, ["api", "v1", "admin", "users", _u, "tokens"]) => {
            let limit = query_usize(uri, "limit").unwrap_or(10).min(st.max_limit);
            let page = if st.ignores_page {
                1
            } else {
                query_usize(uri, "page").unwrap_or(1).max(1)
            };
            let mut newest_first = st.tokens.clone();
            newest_first.sort_by_key(|t| std::cmp::Reverse(t.id));
            let rows: Vec<Value> = newest_first
                .iter()
                .skip((page - 1) * limit)
                .take(limit)
                .map(|t| json!({"id": t.id, "name": t.name, "token_last_eight": last_eight(&t.value)}))
                .collect();
            let total = st.claims_total.unwrap_or(st.tokens.len()).to_string();
            ([("X-Total-Count", total)], axum::Json(Value::Array(rows))).into_response()
        }
        (Method::POST, ["api", "v1", "admin", "users", _u, "tokens"]) => {
            let req: Value = serde_json::from_slice(body).unwrap_or_default();
            let name = req["name"].as_str().unwrap_or_default().to_string();
            let id = st.tokens.iter().map(|t| t.id).max().unwrap_or(0) + 1;
            let value = format!("sha1-of-{name}-{id:08}");
            st.tokens.push(StubToken::new(id, &name, &value));
            (
                StatusCode::CREATED,
                axum::Json(json!({"id": id, "name": name, "sha1": value})),
            )
                .into_response()
        }
        (Method::DELETE, ["api", "v1", "admin", "users", _u, "tokens", reference]) => {
            let before = st.tokens.len();
            match reference.parse::<i64>() {
                Ok(id) => st.tokens.retain(|t| t.id != id),
                Err(_) => st.tokens.retain(|t| t.name != *reference),
            }
            if st.tokens.len() < before {
                StatusCode::NO_CONTENT.into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }
        (Method::GET, ["api", "v1", "repos", _o, _r]) => {
            let presented = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("token "))
                .unwrap_or_default();
            if st.tokens.iter().any(|t| t.value == presented) {
                axum::Json(json!({"full_name": "david/boss"})).into_response()
            } else {
                StatusCode::UNAUTHORIZED.into_response()
            }
        }
        (Method::DELETE, _) => StatusCode::NO_CONTENT.into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Serve a forge holding `tokens`, clamping page size to `max_limit`.
pub(crate) async fn serve(tokens: Vec<StubToken>, max_limit: usize) -> ForgeStub {
    let state = Arc::new(Mutex::new(ForgeState {
        tokens,
        max_limit,
        ..ForgeState::default()
    }));
    let st = state.clone();
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let st = st.clone();
            async move { answer(&st, &method, &uri, &headers, &body) }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    ForgeStub {
        url: format!("http://{addr}"),
        state,
    }
}

/// `n` tokens newer than anything a test seeds by hand (ids from 1000),
/// each with its own last eight — the ledger a busy forge user holds.
pub(crate) fn newer_tokens(n: usize) -> Vec<StubToken> {
    (0..n)
        .map(|i| {
            let id = 1000 + i as i64;
            StubToken::new(
                id,
                &format!("unrelated-{id}"),
                &format!("unrelated-value-{id:08}"),
            )
        })
        .collect()
}
