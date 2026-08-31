//! Reorder-rule helpers — `open_po_exists`, `vendor_for`, `open_restock_exists`.
//!
//! These are the helper functions the canonical reorder-threshold rule
//! calls. The first two query the inventory-api; `open_restock_exists`
//! queries the jobs-api to dedup per-SKU on an in-flight restock Job (which
//! exists the instant it's spawned) rather than on the PO it places later.
//!
//! Implementation note: HelperResolver::call is sync because the
//! expression evaluator is sync. The matcher runs inside tokio, so
//! we use `block_in_place` + `Handle::current().block_on` to bridge
//! to async reqwest. Requires a multi-threaded tokio runtime (which
//! the dispatcher binary uses by default via #[tokio::main]).

use super::expr::{EvalError, HelperResolver, Value};

const SYSTEM_USER_HEADER: &str = r#"{"id":"automation:dispatcher","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

pub struct InventoryHelpers {
    client: reqwest::Client,
    inventory_base: String,
    jobs_base: String,
}

impl InventoryHelpers {
    pub fn new(inventory_base: impl Into<String>, jobs_base: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .expect("async client builds"),
            inventory_base: inventory_base.into(),
            jobs_base: jobs_base.into(),
        }
    }

    fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        helper: &str,
    ) -> Result<T, EvalError> {
        // A read emits nothing, so this header changes no recorded
        // fact today. It is stamped anyway to keep the rule without
        // exceptions: "every dispatcher call declares its origin" is
        // a rule anyone can apply, whereas "…except reads, unless the
        // service logs them" is one people get wrong.
        let req = self
            .client
            .get(url)
            .header("x-boss-user", SYSTEM_USER_HEADER)
            .header("x-sim-origin", crate::dispatcher::sim_origin_value())
            .send();
        let resp = tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(req))
            .map_err(|e| EvalError::HelperFailed {
                name: helper.to_string(),
                msg: format!("GET {url}: {e}"),
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(EvalError::HelperFailed {
                name: helper.to_string(),
                msg: format!("GET {url} returned {status}"),
            });
        }
        let body = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(resp.json::<T>())
        })
        .map_err(|e| EvalError::HelperFailed {
            name: helper.to_string(),
            msg: format!("decode body: {e}"),
        })?;
        Ok(body)
    }
}

impl HelperResolver for InventoryHelpers {
    fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
        match name {
            "open_po_exists" => open_po_exists(self, args),
            "vendor_for" => vendor_for(self, args),
            "open_restock_exists" => open_restock_exists(self, args),
            // Not inventory-flavored, but this is the ONE resolver the
            // runner binds and it already holds jobs_base; the module
            // outgrew its name the day a second domain needed a dedup
            // helper (design-review-spawn, dogfooding arc e556c000).
            "open_review_exists" => open_review_exists(self, args),
            "open_car_exists" => open_car_exists(self, args),
            "open_publish_exists" => open_publish_exists(self, args),
            // The generalization the three guards above were converging
            // on: any (kind, subject) pair, so the NEXT daily spawner
            // gets its dedup as rule data instead of a fourth one-off
            // helper (0517387b — the sweep spawners had none at all).
            "open_job_exists" => open_job_exists(self, args),
            other => Err(EvalError::UnknownHelper(other.to_string())),
        }
    }
}

fn first_string<'a>(args: &'a [Value], helper: &str) -> Result<&'a str, EvalError> {
    match args.first() {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(other) => Err(EvalError::TypeError {
            expected: "string sku",
            got: other.kind(),
        }),
        None => Err(EvalError::HelperFailed {
            name: helper.to_string(),
            msg: "missing required arg".into(),
        }),
    }
}

fn second_string<'a>(args: &'a [Value], helper: &str) -> Result<&'a str, EvalError> {
    match args.get(1) {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(other) => Err(EvalError::TypeError {
            expected: "string subject id",
            got: other.kind(),
        }),
        None => Err(EvalError::HelperFailed {
            name: helper.to_string(),
            msg: "missing required second arg".into(),
        }),
    }
}

/// Minimal percent-encoding for a query value, no new crate: the
/// values are slugs and repo paths, this guards the day one carries a
/// space or '&'.
fn percent_encode(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[derive(serde::Deserialize)]
struct OpenPoResponse {
    exists: bool,
}

fn open_po_exists(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let sku = first_string(args, "open_po_exists")?;
    let url = format!(
        "{}/api/inventory/items/{sku}/open-po-exists",
        h.inventory_base.trim_end_matches('/')
    );
    let r: OpenPoResponse = h.get_json(&url, "open_po_exists")?;
    Ok(Value::Bool(r.exists))
}

#[derive(serde::Deserialize)]
struct VendorForResponse {
    vendor_id: String,
}

fn vendor_for(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let sku = first_string(args, "vendor_for")?;
    let url = format!(
        "{}/api/inventory/items/{sku}/primary-vendor",
        h.inventory_base.trim_end_matches('/')
    );
    let r: VendorForResponse = h.get_json(&url, "vendor_for")?;
    Ok(Value::String(r.vendor_id))
}

#[derive(serde::Deserialize)]
struct JobsListResponse {
    data: Vec<serde_json::Value>,
}

/// True if an `ingredient-restock` Job for `part_sku` is already open.
///
/// The reorder rule dedups on this — per-SKU, on the in-flight restock Job
/// (which exists the instant it's spawned) — rather than on the open PO the
/// restock places much later (after its audit-stock step). That lag let a
/// cold-start burst of `inventory.item.consumed` events spawn dozens of
/// duplicate restocks for the same ingredient before any PO landed. Matches
/// on the Job's `metadata.part_sku` (stamped by the reorder rule via
/// `jobs.spawn`'s `metadata.<field>` args), since the restock's subject is
/// the vendor, not the part.
/// Is there an open design-doc-review Job for this doc path? The
/// spawn rule's dedup: `docs.design.indexed` re-fires on every
/// question-count change, and each firing must not open another
/// review. Subject-filtered server-side — the doc path IS the Job's
/// subject id.
fn open_review_exists(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let doc_path = first_string(args, "open_review_exists")?;
    let encoded = percent_encode(doc_path);
    let url = format!(
        "{}/api/jobs?kind=design-doc-review&status=open&subject_id={}&limit=1",
        h.jobs_base.trim_end_matches('/'),
        encoded,
    );
    let r: JobsListResponse = h.get_json(&url, "open_review_exists")?;
    Ok(Value::Bool(!r.data.is_empty()))
}

/// Is there already an open `ship-a-change` car for this recurring
/// finding? The dedup for `spawn-car-on-sweep-remediated`.
///
/// A sweep runs on a cadence and spawns a car every time it closes
/// `remediated`, so a condition that PERSISTS across days mints one
/// car per day. Measured 2026-08-17 (defect e74b32a1): two cars on the
/// board, `dcff2c74` and `5621606f`, both titled exactly "Stale build
/// cache sweep", both from the same `stale-build-caches` target, one
/// day apart — with no summary, no branch and no body, because the
/// title is templated from the sweep. They are only distinguishable by
/// opening each one and reading its metadata. The agent holding the
/// measurements nearly closed one as a duplicate of the other and had
/// to revert the flag; a reader scanning My Day has no chance.
///
/// Keyed on the sweep's SUBJECT (`stale-build-caches`), not its id or
/// title: the id is fresh every firing, and the title is templated per
/// target, so neither separates "the same finding again" from "a
/// different finding". `design-review-spawn` has had exactly this
/// guard — `NOT open_review_exists(path)` — since it was written; this
/// rule simply never got one.
fn open_car_exists(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let target = first_string(args, "open_car_exists")?;
    let url = format!(
        "{}/api/jobs?kind=ship-a-change&status=open&limit=200",
        h.jobs_base.trim_end_matches('/')
    );
    let r: JobsListResponse = h.get_json(&url, "open_car_exists")?;
    let exists = r.data.iter().any(|j| {
        j.get("metadata")
            .and_then(|m| m.get("sweep_target"))
            .and_then(|s| s.as_str())
            == Some(target)
    });
    Ok(Value::Bool(exists))
}

/// True if a `publish-to-github` packet for this mirror subject is
/// already open — the dedup for `publish-to-github-daily`.
///
/// Same defect class as `open_car_exists` above, on a SCHEDULED rule:
/// the daily spawner asked no question, so while one packet sat at its
/// approval sign-off (which, unassigned, notified nobody — 13128a0c),
/// every morning minted another. Measured 2026-08-18: ab13f05f and
/// f4e9cdf6 open at once, one day apart, identical titles (9f0c566a).
///
/// Keyed on the packet's SUBJECT (`github-mirror`): there is one mirror,
/// so one open publish packet is the invariant. Matching on subject
/// rather than a metadata key differs from the car guard deliberately —
/// the mirror subject is the packet's declared identity, not a
/// breadcrumb stamped for the guard's benefit.
fn open_publish_exists(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let subject = first_string(args, "open_publish_exists")?;
    let url = format!(
        "{}/api/jobs?kind=publish-to-github&status=open&limit=200",
        h.jobs_base.trim_end_matches('/')
    );
    let r: JobsListResponse = h.get_json(&url, "open_publish_exists")?;
    let exists = r.data.iter().any(|j| {
        j.get("subject")
            .and_then(|s| s.get("id"))
            .and_then(|s| s.as_str())
            == Some(subject)
    });
    Ok(Value::Bool(exists))
}

/// True if an open Job of `kind` with subject id `subject_id` exists —
/// the generic (kind, subject) dedup guard.
///
/// Born as the fix for 0517387b: every daily maintenance-sweep spawner
/// fired unconditionally, so an undischargeable obligation accumulated
/// one packet per day (5 open cluster-conformance sweeps when
/// measured). The three domain guards above each solved this for one
/// kind; a fourth one-off (`open_sweep_exists`) would have continued
/// the pattern this module's own comment calls outgrown. Keyed on the
/// packet's SUBJECT like `open_publish_exists` — for sweeps the
/// subject IS the target — and filtered server-side like
/// `open_review_exists`, so the answer costs one row.
fn open_job_exists(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let kind = first_string(args, "open_job_exists")?;
    let subject_id = second_string(args, "open_job_exists")?;
    let url = format!(
        "{}/api/jobs?kind={}&status=open&subject_id={}&limit=1",
        h.jobs_base.trim_end_matches('/'),
        percent_encode(kind),
        percent_encode(subject_id),
    );
    let r: JobsListResponse = h.get_json(&url, "open_job_exists")?;
    Ok(Value::Bool(!r.data.is_empty()))
}

fn open_restock_exists(h: &InventoryHelpers, args: &[Value]) -> Result<Value, EvalError> {
    let part_sku = first_string(args, "open_restock_exists")?;
    let url = format!(
        "{}/api/jobs?kind=ingredient-restock&status=open&limit=200",
        h.jobs_base.trim_end_matches('/')
    );
    let r: JobsListResponse = h.get_json(&url, "open_restock_exists")?;
    let exists = r.data.iter().any(|j| {
        j.get("metadata")
            .and_then(|m| m.get("part_sku"))
            .and_then(|s| s.as_str())
            == Some(part_sku)
    });
    Ok(Value::Bool(exists))
}
