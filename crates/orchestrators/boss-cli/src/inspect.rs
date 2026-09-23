//! `boss inspect` — read-only diagnostic surface over the
//! gateway's HTTP APIs.
//!
//! Replaces the muscle-memory pattern of `sudo -u postgres psql
//! -d boss -c "SELECT ..."` for diagnostic reads. Reasons:
//!
//! - Raw SQL reads hide API gaps. If the data you want isn't on a
//!   list endpoint, that's a real coverage hole worth filing —
//!   diving past it via psql means future operators (or the SPA)
//!   still won't have what they need.
//! - The same muscle memory leads to raw SQL *writes*, which
//!   bypass the audit_log + policy gate + replay path. Those
//!   corrupt the event-sourced model. Better to never reach for
//!   psql in the first place.
//!
//! Every subcommand here hits `GET /api/<domain>/...` through the
//! gateway. Output is a pretty-printed table by default; pass
//! `--json` for machine-parseable output (jq, ad-hoc scripts).
//!
//! See memory note `feedback_no_raw_sql_for_diagnostics.md`.
//!
//! Defaults to `http://127.0.0.1:4443` (the canonical local
//! gateway listen address per `BOSS_LISTEN`). Override via the
//! `--gateway-url` flag or `BOSS_GATEWAY_URL=...` env. Every
//! install path (docker-compose, the cluster) binds
//! to `:4443`.

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

/// Resolve the gateway URL. Explicit `--gateway-url` flag wins;
/// then `$BOSS_GATEWAY_URL`; then the documented default.
pub fn resolve_gateway_url(explicit: Option<&str>) -> String {
    if let Some(url) = explicit {
        return url.trim_end_matches('/').to_string();
    }
    if let Ok(url) = std::env::var("BOSS_GATEWAY_URL") {
        return url.trim_end_matches('/').to_string();
    }
    "http://127.0.0.1:4443".to_string()
}

/// `GET <url>` and return the parsed JSON body. Network / non-200
/// errors carry the URL so the operator sees what failed.
async fn fetch_json(url: &str) -> Result<Value> {
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let body = resp.text().await.context("read body")?;
    if !status.is_success() {
        return Err(anyhow!("GET {url} returned {status}\n{body}"));
    }
    serde_json::from_str(&body).with_context(|| format!("parse JSON from {url}"))
}

/// Many list endpoints return `{data: [...], total, limit, offset}`;
/// older ones return a bare array. Normalise to the inner array.
fn unwrap_rows(body: &Value) -> &[Value] {
    if let Some(arr) = body.as_array() {
        return arr;
    }
    if let Some(arr) = body.get("data").and_then(|d| d.as_array()) {
        return arr;
    }
    &[]
}

/// `(rows read, DB-wide total)` when an envelope carries fewer rows
/// than its `total` — the part of the list this read never saw. A bare
/// array says nothing either way, so it answers `None`.
fn unread_tail(body: &Value) -> Option<(usize, u64)> {
    let read = unwrap_rows(body).len();
    let total = body.get("total").and_then(Value::as_u64)?;
    (total > read as u64).then_some((read, total))
}

/// Render a value as a one-line string fit for a table cell.
/// Strings come through unquoted; everything else is JSON-encoded
/// and truncated.
fn cell(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "-".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(other) => {
            let s = other.to_string();
            if s.len() > 60 {
                format!("{}…", &s[..57])
            } else {
                s
            }
        }
    }
}

/// Truncate a column value for fixed-width display.
fn trunc(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

// ---------------------------------------------------------------------------
// Domain handlers
// ---------------------------------------------------------------------------

pub async fn invoices(
    status: Option<&str>,
    account_id: Option<&str>,
    limit: u32,
    json: bool,
    gateway: &str,
) -> Result<()> {
    let mut url = format!("{gateway}/api/commerce/invoices?limit={limit}");
    if let Some(s) = status {
        url.push_str(&format!("&status={s}"));
    }
    if let Some(a) = account_id {
        url.push_str(&format!("&account_id={a}"));
    }
    let body = fetch_json(&url).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let rows = unwrap_rows(&body);
    if rows.is_empty() {
        println!("No invoices found.");
        return Ok(());
    }
    println!(
        "{:<36}  {:<20}  {:<12}  {:>14}  {:<10}  {:<10}",
        "ID", "ACCOUNT", "STATUS", "AMOUNT", "ISSUED", "PAID"
    );
    println!("{}", "-".repeat(112));
    for row in rows {
        let amount_cents = row
            .get("amount_cents")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let dollars = format!("${:.2}", (amount_cents as f64) / 100.0);
        println!(
            "{:<36}  {:<20}  {:<12}  {:>14}  {:<10}  {:<10}",
            trunc(&cell(row.get("id")), 36),
            trunc(&cell(row.get("account_id")), 20),
            trunc(&cell(row.get("status")), 12),
            dollars,
            cell(row.get("issued_on")),
            cell(row.get("paid_on")),
        );
    }
    if let Some(total) = body.get("total").and_then(|v| v.as_u64())
        && total > rows.len() as u64
    {
        println!(
            "\nShowing {} of {} (raise --limit to see more)",
            rows.len(),
            total
        );
    }
    Ok(())
}

pub async fn accounts(
    name_contains: Option<&str>,
    limit: u32,
    json: bool,
    gateway: &str,
) -> Result<()> {
    // The directory is a bounded page since backlog 2d1d298e: ask for
    // the service's most (1000, boss-accounts' MAX_LIST_LIMIT — a
    // larger ask is clamped) and say below when even that was short.
    let url = format!("{gateway}/api/people/accounts?limit=1000");
    let body = fetch_json(&url).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let mut rows: Vec<&Value> = unwrap_rows(&body).iter().collect();
    if let Some(needle) = name_contains.map(|s| s.to_lowercase()) {
        rows.retain(|r| {
            r.get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| n.to_lowercase().contains(&needle))
        });
    }
    let total_matching = rows.len();
    rows.truncate(limit as usize);
    let tail = unread_tail(&body).map(|(read, total)| {
        format!("Searched the first {read} of {total} accounts; the rest were not read.")
    });
    if rows.is_empty() {
        println!("No accounts found.");
        if let Some(t) = &tail {
            println!("{t}");
        }
        return Ok(());
    }
    println!(
        "{:<24}  {:<30}  {:<12}  {:<20}  {:<10}",
        "ID", "NAME", "TIER", "CITY", "STATE"
    );
    println!("{}", "-".repeat(100));
    for row in &rows {
        println!(
            "{:<24}  {:<30}  {:<12}  {:<20}  {:<10}",
            trunc(&cell(row.get("id")), 24),
            trunc(&cell(row.get("name")), 30),
            trunc(&cell(row.get("tier")), 12),
            trunc(&cell(row.get("city")), 20),
            cell(row.get("state")),
        );
    }
    if total_matching > rows.len() {
        println!(
            "\nShowing {} of {} matching (raise --limit to see more)",
            rows.len(),
            total_matching
        );
    }
    if let Some(t) = &tail {
        println!("{t}");
    }
    Ok(())
}

pub async fn jobs(
    status: Option<&str>,
    kind: Option<&str>,
    account_id: Option<&str>,
    limit: u32,
    json: bool,
    gateway: &str,
) -> Result<()> {
    let mut url = format!("{gateway}/api/jobs?limit={limit}");
    if let Some(s) = status {
        url.push_str(&format!("&status={s}"));
    }
    if let Some(k) = kind {
        url.push_str(&format!("&kind={k}"));
    }
    if let Some(a) = account_id {
        url.push_str(&format!("&account_id={a}"));
    }
    let body = fetch_json(&url).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let rows = unwrap_rows(&body);
    if rows.is_empty() {
        println!("No jobs found.");
        return Ok(());
    }
    println!(
        "{:<36}  {:<20}  {:<12}  {:<10}  {:<10}  TITLE",
        "ID", "KIND", "STATUS", "OPENED", "CLOSED"
    );
    println!("{}", "-".repeat(120));
    for row in rows {
        let title = cell(row.get("title"));
        println!(
            "{:<36}  {:<20}  {:<12}  {:<10}  {:<10}  {}",
            trunc(&cell(row.get("id")), 36),
            trunc(&cell(row.get("kind")), 20),
            trunc(&cell(row.get("status")), 12),
            cell(row.get("opened_on")),
            cell(row.get("closed_on")),
            trunc(&title, 50),
        );
    }
    if let Some(total) = body.get("total").and_then(|v| v.as_u64())
        && total > rows.len() as u64
    {
        println!(
            "\nShowing {} of {} (raise --limit to see more)",
            rows.len(),
            total
        );
    }
    Ok(())
}

pub async fn employees(role: Option<&str>, limit: u32, json: bool, gateway: &str) -> Result<()> {
    let url = format!("{gateway}/api/people");
    let body = fetch_json(&url).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let mut rows: Vec<&Value> = unwrap_rows(&body).iter().collect();
    if let Some(needle) = role {
        rows.retain(|r| {
            r.get("role")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s == needle)
        });
    }
    let total_matching = rows.len();
    rows.truncate(limit as usize);
    if rows.is_empty() {
        println!("No employees found.");
        return Ok(());
    }
    println!(
        "{:<24}  {:<30}  {:<20}  {:<24}  STATUS",
        "ID", "NAME", "ROLE", "EMAIL"
    );
    println!("{}", "-".repeat(110));
    for row in &rows {
        println!(
            "{:<24}  {:<30}  {:<20}  {:<24}  {}",
            trunc(&cell(row.get("id")), 24),
            trunc(&cell(row.get("name")), 30),
            trunc(&cell(row.get("role")), 20),
            trunc(&cell(row.get("email")), 24),
            cell(row.get("status")),
        );
    }
    if total_matching > rows.len() {
        println!(
            "\nShowing {} of {} matching (raise --limit to see more)",
            rows.len(),
            total_matching
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_url_prefers_explicit_flag() {
        assert_eq!(
            resolve_gateway_url(Some("http://example.test:9000")),
            "http://example.test:9000"
        );
    }

    #[test]
    fn resolve_url_strips_trailing_slash() {
        assert_eq!(
            resolve_gateway_url(Some("http://example.test/")),
            "http://example.test"
        );
    }

    #[test]
    fn resolve_url_falls_back_to_local_gateway_default() {
        // Don't touch BOSS_GATEWAY_URL env in tests — the default
        // path is what we're pinning.
        let saved = std::env::var("BOSS_GATEWAY_URL").ok();
        // SAFETY: this test reads/writes a process-global env var.
        // Other tests in this module also touch env vars; cargo
        // test runs them sequentially in the same module by
        // default for this crate (no --test-threads override
        // problems observed).
        unsafe {
            std::env::remove_var("BOSS_GATEWAY_URL");
        }
        assert_eq!(resolve_gateway_url(None), "http://127.0.0.1:4443");
        if let Some(v) = saved {
            unsafe {
                std::env::set_var("BOSS_GATEWAY_URL", v);
            }
        }
    }

    #[test]
    fn an_envelope_past_its_page_names_the_unread_tail() {
        // 2d1d298e: the accounts read is a bounded page now, so a
        // client-side name search covers only what came back — and
        // must say so rather than answer "No accounts found".
        let capped = json!({"data": [{"a": 1}, {"a": 2}], "total": 5});
        assert_eq!(unread_tail(&capped), Some((2, 5)));
        let whole = json!({"data": [{"a": 1}], "total": 1});
        assert_eq!(unread_tail(&whole), None);
        let bare = json!([{"a": 1}]);
        assert_eq!(unread_tail(&bare), None);
    }

    #[test]
    fn unwrap_rows_handles_envelope_and_bare_array() {
        let envelope = json!({"data": [{"a": 1}, {"a": 2}], "total": 99});
        assert_eq!(unwrap_rows(&envelope).len(), 2);
        let bare = json!([{"a": 1}, {"a": 2}, {"a": 3}]);
        assert_eq!(unwrap_rows(&bare).len(), 3);
        let empty = json!({});
        assert!(unwrap_rows(&empty).is_empty());
    }

    #[test]
    fn cell_formats_value_kinds() {
        assert_eq!(cell(Some(&Value::String("abc".into()))), "abc");
        assert_eq!(cell(Some(&json!(42))), "42");
        assert_eq!(cell(Some(&Value::Bool(true))), "true");
        assert_eq!(cell(None), "-");
        assert_eq!(cell(Some(&Value::Null)), "-");
    }

    #[test]
    fn trunc_handles_unicode() {
        assert_eq!(trunc("hello", 10), "hello");
        assert_eq!(trunc("hello world", 5), "hell…");
    }
}
