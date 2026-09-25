//! `packaging.allocate` — the agent executor that decides how to package
//! a brewed batch.
//!
//! ## Why this exists
//!
//! A brew is one tank of fungible wort. The brewery packages it into
//! whatever finished-good formats current demand needs — you don't
//! pre-commit a batch to a fixed "N half-BBLs + M sixtels" split and then
//! dump the format nobody wants. The pre-PR5 model did exactly that: two
//! independent per-format demand gates could each *skip*, and when both
//! skipped the wort stranded in WIP forever (251 brews / $6.7M over a
//! 365-day run). A real brewer never dumps a whole batch — they package
//! all of it and hold the surplus as finished-goods buffer.
//!
//! So this handler replaces the two per-format skip gates with one
//! **allocation** decision: on `step.ready.*` it reads real finished-goods
//! stock (crediting in-flight brews, like the demand gate), splits the
//! whole batch across its formats **in proportion to each format's
//! shortfall to target**, and stamps the per-format keg quantities onto the
//! package steps. The whole batch is always packaged → WIP drains 100% to
//! finished goods, `1310` nets ~0, and nothing is dumped.
//!
//! ## Data-driven
//!
//! The policy is generic; the brewery-specific numbers are seed data on the
//! allocation step's metadata — mirroring `gate.resolve`/`demand-gate`:
//! `batch_bbl` (wort volume to allocate), `target_skus` (the formats),
//! `expected_daily_demand` + `demand_window_days` + `oversupply_multiplier`
//! (the per-format target — short if effective on-hand is below it, same as
//! the demand gate), `batch_yield` (per-format yield, for the in-flight
//! credit), and `default_kegs` (the seeded split, used when no format is
//! short so the batch still packages, as buffer). Keg volume is read off the
//! SKU (`FP-…-1-2-BBL` → ½ BBL). No brewery constants live in this file.
//!
//! ## Fork mechanism
//!
//! boss-expr forks on `steps.X.metadata.<key> = "value"`, so the handler
//! stamps a per-format outcome `outcome_<fork_key>` = `package|skip` on
//! its own step (the `fork_keys` map — SKU → short label like `half` —
//! is seed data; the seed also declares each `outcome_<fork_key>` as an
//! inline `package|skip` field so the viability lint proves the fork is
//! exhaustive without a brewery-specific core StepType). It writes every
//! format's allocated keg quantity — INCLUDING a 0 for a skipped format —
//! plus excise onto that format's produce step. The morning-brew DAG keeps
//! its package/skip steps, forked on those outcomes; a format allocated
//! zero kegs routes to its skip step (its produce step never runs), the
//! rest package the whole batch. The 0 it writes only corrects the packaged
//! siblings' `products.produce` cost basis (a skipped format drains no WIP).

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value as ExprValue;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use serde_json::{Value as JsonValue, json};
use std::sync::Arc;

use super::common::{
    StepEvent, dispatcher_actor_header, dispatcher_reader_header, sim_origin_value,
};

/// One finished-good format a brewed batch can be packaged into. All
/// fields come from the allocation step's seed metadata + a live stock
/// read — the pure allocator never touches IO.
#[derive(Debug, Clone, PartialEq)]
pub struct FormatNeed {
    pub sku: String,
    /// Volume per keg in BBL (0.5 for a half-BBL, 1/6 for a sixtel).
    pub keg_bbl: f64,
    /// Demand target in kegs; the format is "short" below this.
    pub target_kegs: i64,
    /// Real on-hand + in-flight yield, in kegs (the demand gate's
    /// `effective_on_hand`).
    pub effective_kegs: i64,
}

/// Allocate a brewed batch's `batch_bbl` of wort across its formats by
/// need, so the **whole** batch is packaged (WIP drains fully to FG — the
/// brewery never dumps a batch).
///
/// - A format short of target absorbs batch volume in proportion to its
///   shortfall (bbl); a format at/above target gets zero (no glut).
/// - When no format is short (demand caught up during the multi-day brew
///   lag), fall back to `default_kegs` (the seeded split) so the batch
///   still packages and lands as bounded FG buffer.
///
/// Returns kegs-to-package per format, index-aligned with `formats`.
/// `Σ(kegs × keg_bbl) ≈ batch_bbl` (modulo per-keg rounding).
pub fn allocate_batch(batch_bbl: f64, formats: &[FormatNeed], default_kegs: &[i64]) -> Vec<i64> {
    // Each format's shortfall, in BBL.
    let shortfalls: Vec<f64> = formats
        .iter()
        .map(|f| ((f.target_kegs - f.effective_kegs).max(0) as f64) * f.keg_bbl)
        .collect();
    let total_short: f64 = shortfalls.iter().sum();

    // Nobody short → the batch still has to go somewhere; use the seeded
    // split and hold it as buffer. Fall back to an even split if the seed
    // didn't provide one (never strands the wort).
    if total_short <= 0.0 {
        if default_kegs.len() == formats.len() {
            return default_kegs.to_vec();
        }
        return even_split(batch_bbl, formats);
    }

    // Allocate the whole batch ∝ shortfall, converted to whole kegs.
    formats
        .iter()
        .zip(&shortfalls)
        .map(|(f, &short)| {
            if f.keg_bbl <= 0.0 {
                return 0;
            }
            let bbl = batch_bbl * (short / total_short);
            (bbl / f.keg_bbl).round() as i64
        })
        .collect()
}

/// Last-resort even split of the batch across formats (used only when no
/// format is short AND the seed provided no default split).
fn even_split(batch_bbl: f64, formats: &[FormatNeed]) -> Vec<i64> {
    if formats.is_empty() {
        return Vec::new();
    }
    let per = batch_bbl / formats.len() as f64;
    formats
        .iter()
        .map(|f| {
            if f.keg_bbl <= 0.0 {
                0
            } else {
                (per / f.keg_bbl).round() as i64
            }
        })
        .collect()
}

/// Total packaged volume (BBL) an allocation yields — the volume the
/// batch's WIP drains across. Used to sanity-check conservation.
pub fn allocated_bbl(formats: &[FormatNeed], kegs: &[i64]) -> f64 {
    formats
        .iter()
        .zip(kegs)
        .map(|(f, &k)| f.keg_bbl * k as f64)
        .sum()
}

/// The `packaging.allocate` agent handler. Reads FG stock (crediting
/// in-flight brews), allocates the batch via [`allocate_batch`], writes
/// each packaged format's keg qty + excise onto its produce step, and
/// stamps per-format `outcome_<fork_key>` on its own step so the DAG
/// routes package vs skip. Every number is data — no brewery constants.
pub struct PackagingAllocate {
    client: reqwest::Client,
    jobs_base: String,
    products_base: String,
}

impl PackagingAllocate {
    pub fn new(jobs_base: impl Into<String>, products_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            jobs_base: jobs_base.into(),
            products_base: products_base.into(),
        })
    }

    /// Real finished-goods on-hand for one SKU (kegs). Unknown / unreachable
    /// reads as 0 (treated as short) so we fail toward packaging, never
    /// toward stranding wort; transport failure errors → NAK + redeliver.
    async fn product_on_hand(&self, sku: &str) -> Result<i64, HandlerError> {
        let url = format!(
            "{}/api/products/{}",
            self.products_base.trim_end_matches('/'),
            sku
        );
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            return Ok(0);
        }
        let v: JsonValue = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("decode product {sku}: {e}")))?;
        Ok(v.get("total_on_hand").and_then(|x| x.as_i64()).unwrap_or(0))
    }

    async fn fetch_job(&self, job_id: &str) -> Result<JsonValue, HandlerError> {
        let url = format!(
            "{}/api/jobs/{}",
            self.jobs_base.trim_end_matches('/'),
            job_id
        );
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status}: {body}"
            )));
        }
        resp.json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} not JSON: {e}")))
    }

    /// Count of OPEN Jobs of `kind` — the in-flight pipeline depth. Non-2xx
    /// reads as 0 (no pipeline credit → fail toward brewing).
    async fn open_jobs_of_kind(&self, kind: &str) -> Result<i64, HandlerError> {
        let url = format!(
            "{}/api/jobs?kind={}&status=open&limit=1",
            self.jobs_base.trim_end_matches('/'),
            kind
        );
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            return Ok(0);
        }
        let v: JsonValue = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("decode jobs list {kind}: {e}")))?;
        Ok(v.get("total").and_then(|x| x.as_i64()).unwrap_or(0))
    }

    /// One step write — the PUT, or the merge door's PATCH — refused
    /// loudly on a non-2xx, naming the method, the url and the answer.
    async fn write_step(
        &self,
        method: reqwest::Method,
        url: &str,
        body: JsonValue,
        rule: &str,
    ) -> Result<(), HandlerError> {
        let resp = self
            .client
            .request(method.clone(), url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(rule))
            .header("x-sim-origin", sim_origin_value())
            .json(&body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("{method} {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "{method} {url} returned {status}: {text}"
            )));
        }
        Ok(())
    }

    /// Write the packaged qty + excise onto the produce step that yields
    /// `sku` ([`produce_stamp`]), through the step merge door. No status
    /// change — the step stays pending until the fork lets it become
    /// ready and the workforce packages it.
    async fn stamp_produce_qty(
        &self,
        job_id: &str,
        steps: &[JsonValue],
        sku: &str,
        kegs: i64,
        keg_bbl: f64,
        rule: &str,
    ) -> Result<(), HandlerError> {
        let levied = (kegs as f64 * keg_bbl).round() as i64;
        let Some((sid, fields)) = produce_stamp(steps, sku, kegs, levied) else {
            return Ok(());
        };
        let url = format!(
            "{}/api/jobs/{}/steps/{}/metadata",
            self.jobs_base.trim_end_matches('/'),
            job_id,
            sid
        );
        self.write_step(
            reqwest::Method::PATCH,
            &url,
            JsonValue::Object(fields),
            rule,
        )
        .await
    }
}

#[async_trait]
impl Handler for PackagingAllocate {
    fn name(&self) -> &'static str {
        "packaging.allocate"
    }

    async fn invoke(
        &self,
        _args: &[(String, ExprValue)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let ev = StepEvent::from_payload(&ctx.event_payload)?;
        let md = ev.metadata;
        // Self-filter: an allocation step carries a positive batch_bbl +
        // target_skus. Anything else is a no-op (this handler shares the
        // step.ready.* subscription).
        let batch_bbl = md.get("batch_bbl").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let skus = string_array(md, "target_skus");
        if batch_bbl <= 0.0 || skus.is_empty() {
            return Ok(());
        }

        let window = md
            .get("demand_window_days")
            .and_then(|v| v.as_f64())
            .unwrap_or(30.0);
        let mult = md
            .get("oversupply_multiplier")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.5);
        let demand = md.get("expected_daily_demand");
        let batch_yield = md.get("batch_yield");
        let fork_keys = md.get("fork_keys");
        let default_map = md.get("default_kegs");

        // Fetch the Job once — used for its Workflow (the in-flight count)
        // and its steps (to write package quantities).
        let job = self.fetch_job(ev.job_id).await?;
        // In-flight pipeline depth (open Jobs of this kind minus this one),
        // crediting yield-in-flight like the demand gate.
        let in_flight = match job.get("kind").and_then(|k| k.as_str()) {
            Some(kind) => (self.open_jobs_of_kind(kind).await? - 1).max(0),
            None => 0,
        };

        let mut formats = Vec::with_capacity(skus.len());
        let mut default_kegs = Vec::with_capacity(skus.len());
        for sku in &skus {
            let keg_bbl = keg_volume_bbl(sku);
            let daily = demand
                .and_then(|d| d.get(sku))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let target_kegs = (daily * window * mult) as i64;
            let real = self.product_on_hand(sku).await?;
            let per_batch = batch_yield
                .and_then(|m| m.get(sku))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            formats.push(FormatNeed {
                sku: sku.clone(),
                keg_bbl,
                target_kegs,
                effective_kegs: real + in_flight * per_batch,
            });
            default_kegs.push(
                default_map
                    .and_then(|m| m.get(sku))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
            );
        }

        let allocated = allocate_batch(batch_bbl, &formats, &default_kegs);

        // Write the packaged formats' quantities + stamp per-format outcomes.
        let steps = job
            .get("steps")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for (fmt, &alloc) in formats.iter().zip(&allocated) {
            // Write the allocated qty onto every format's produce step — the
            // packaged split, INCLUDING a 0 for a skipped format. products.produce
            // reads these quantities to spread the batch's WIP across the formats
            // by packaged volume; a 0-qty format contributes 0 volume, drains no
            // WIP, and its share is absorbed by the packaged formats. (Its produce
            // step never runs — the fork routes it to `skip` — so writing 0 only
            // corrects the sibling's cost basis; it never produces phantom FG.)
            self.stamp_produce_qty(
                ev.job_id,
                &steps,
                &fmt.sku,
                alloc,
                fmt.keg_bbl,
                &ctx.rule_name,
            )
            .await?;
        }

        // Complete this step: the outcomes through the step merge door,
        // THEN a status-only PUT (backlog e39a9d2a). This was one PUT
        // whose metadata was the triggering EVENT's copy plus the
        // outcomes; the PUT replaces metadata wholesale, so anything
        // written to the step since `step.ready` fired was dropped by
        // omission. Merge first: the fork reads the outcomes on the flip.
        let url = format!(
            "{}/api/jobs/{}/steps/{}",
            self.jobs_base.trim_end_matches('/'),
            ev.job_id,
            ev.step_id
        );
        self.write_step(
            reqwest::Method::PATCH,
            &format!("{url}/metadata"),
            JsonValue::Object(outcomes(&formats, &allocated, fork_keys)),
            &ctx.rule_name,
        )
        .await?;
        self.write_step(
            reqwest::Method::PUT,
            &url,
            json!({ "status": "completed" }),
            &ctx.rule_name,
        )
        .await
    }
}

/// The produce-step key the levied volume is stamped under — the name
/// the tenant's ledger rule reads.
const LEVIED_KEY: &str = "excise_bbl";

/// PURE: the first step of `steps` that produces `sku`, and the two keys
/// that stamp its packaged quantity: `produces_products` with `sku`'s
/// `qty` set, and the volume the tax is levied on. ONLY those two — they
/// ride the step merge door, which keeps the step's other keys on the row
/// (backlog e39a9d2a). This was a PUT of the step's whole metadata as the
/// Job read it before the loop, and the PUT replaces metadata wholesale.
///
/// NOT FIXED HERE, and worth its own item: `produces_products` is itself
/// built from that pre-loop read, so two SKUs stamped onto ONE produce
/// step still overwrite each other's `qty` (the second write carries the
/// first SKU's stale 0). Every produce step the seeded tenant declares
/// yields one SKU, so it has not bitten.
fn produce_stamp(
    steps: &[JsonValue],
    sku: &str,
    qty: i64,
    levied_volume: i64,
) -> Option<(String, serde_json::Map<String, JsonValue>)> {
    steps.iter().find_map(|s| {
        let sid = s.get("id").and_then(|v| v.as_str())?;
        let produces = s.pointer("/metadata/produces_products")?.as_array()?;
        if !produces
            .iter()
            .any(|p| p.get("sku").and_then(|v| v.as_str()) == Some(sku))
        {
            return None;
        }
        let stamped: Vec<JsonValue> = produces
            .iter()
            .map(|p| {
                let mut p = p.clone();
                if p.get("sku").and_then(|v| v.as_str()) == Some(sku) {
                    p["qty"] = json!(qty);
                }
                p
            })
            .collect();
        let mut fields = serde_json::Map::new();
        fields.insert("produces_products".into(), JsonValue::Array(stamped));
        fields.insert(LEVIED_KEY.into(), json!(levied_volume));
        Some((sid.to_string(), fields))
    })
}

/// PURE: the per-format fork outcomes the allocation step records —
/// `outcome_<key>` = `package` or `skip` — and nothing else, because
/// they ride the step merge door, which keeps every key it is not sent.
/// The key is the seeded `fork_keys[sku]` (a short predicate-safe label
/// like `half`), else the SKU itself.
fn outcomes(
    formats: &[FormatNeed],
    allocated: &[i64],
    fork_keys: Option<&JsonValue>,
) -> serde_json::Map<String, JsonValue> {
    formats
        .iter()
        .zip(allocated)
        .map(|(fmt, &alloc)| {
            let key = fork_keys
                .and_then(|m| m.get(fmt.sku.as_str()))
                .and_then(|v| v.as_str())
                .unwrap_or(fmt.sku.as_str());
            (
                format!("outcome_{key}"),
                json!(if alloc > 0 { "package" } else { "skip" }),
            )
        })
        .collect()
}

fn string_array(md: &serde_json::Map<String, JsonValue>, key: &str) -> Vec<String> {
    md.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Volume per keg (BBL) from a finished-product SKU like `FP-PALE-1-2-BBL`
/// (½) or `FP-IPA-1-6-BBL` (⅙). Non-keg SKUs → 1.0.
fn keg_volume_bbl(sku: &str) -> f64 {
    let parts: Vec<&str> = sku.split('-').collect();
    if let [.., num, den, unit] = parts.as_slice()
        && unit.eq_ignore_ascii_case("BBL")
        && let (Ok(n), Ok(d)) = (num.parse::<f64>(), den.parse::<f64>())
        && d > 0.0
    {
        return n / d;
    }
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const HALF: f64 = 0.5;
    const SIXTEL: f64 = 1.0 / 6.0;

    fn fmt(sku: &str, keg_bbl: f64, target: i64, effective: i64) -> FormatNeed {
        FormatNeed {
            sku: sku.into(),
            keg_bbl,
            target_kegs: target,
            effective_kegs: effective,
        }
    }

    /// THE COMPLETION MERGES THE OUTCOMES AND NOTHING ELSE (backlog
    /// e39a9d2a, Stage 1). The allocation step was completed with a PUT
    /// whose metadata was the TRIGGERING EVENT's copy plus the outcomes —
    /// stale by construction, and the PUT replaces metadata wholesale, so
    /// any key written to the step after `step.ready` fired was dropped
    /// by omission. The outcomes now go through the step merge door and
    /// the PUT carries the status alone, so the body holds only the keys
    /// this handler decided.
    #[test]
    fn the_outcomes_are_the_only_keys_the_completion_writes() {
        let formats = vec![fmt("SKU-A", HALF, 500, 0), fmt("SKU-B", SIXTEL, 500, 900)];
        let fork_keys = json!({"SKU-A": "half"});
        let out = outcomes(&formats, &[316, 0], Some(&fork_keys));
        assert_eq!(
            JsonValue::Object(out),
            json!({"outcome_half": "package", "outcome_SKU-B": "skip"})
        );
    }

    /// THE PRODUCE STAMP MERGES ITS TWO KEYS AND NOTHING ELSE (backlog
    /// e39a9d2a, stage 2). The packaged quantity was written by PUTting
    /// the produce step's whole metadata back — the copy the Job read
    /// before the loop, plus the new quantity and levied volume — so the
    /// PUT, which replaces metadata wholesale, carried every other key
    /// along from a read that could be stale. It now names only those
    /// two keys, through the step merge door, so the step's other keys
    /// stay as they are on the row.
    #[test]
    fn the_produce_stamp_names_only_the_quantity_and_the_levied_volume() {
        let steps = vec![
            json!({"id": "s-alloc", "metadata": {"target_skus": ["SKU-A"]}}),
            json!({"id": "s-half", "metadata": {
                "authority_role": "platform-admin",
                "produces_products": [{"sku": "SKU-A", "qty": 0}, {"sku": "SKU-C", "qty": 4}],
            }}),
        ];
        let (sid, fields) = produce_stamp(&steps, "SKU-A", 20, 10).expect("SKU-A is produced");
        assert_eq!(sid, "s-half");
        let mut expected = serde_json::Map::new();
        expected.insert(
            "produces_products".into(),
            json!([{"sku": "SKU-A", "qty": 20}, {"sku": "SKU-C", "qty": 4}]),
        );
        expected.insert(LEVIED_KEY.into(), json!(10));
        assert_eq!(fields, expected);
        assert!(produce_stamp(&steps, "SKU-Z", 1, 1).is_none());
    }

    #[test]
    fn only_one_format_short_absorbs_the_whole_batch() {
        // Half-BBL short (0 on hand vs 500 target); sixtel over target.
        let formats = vec![
            fmt("FP-PALE-1-2-BBL", HALF, 500, 0),
            fmt("FP-PALE-1-6-BBL", SIXTEL, 500, 900),
        ];
        let kegs = allocate_batch(158.0, &formats, &[210, 315]);
        // The whole 158 bbl goes to half-BBLs; sixtel gets none (no glut).
        assert_eq!(kegs[1], 0);
        assert_eq!(kegs[0], 316); // 158 / 0.5
        // Whole batch packaged.
        assert!((allocated_bbl(&formats, &kegs) - 158.0).abs() < 1.0);
    }

    #[test]
    fn both_short_split_in_proportion_to_shortfall() {
        // Half short by 100 kegs×0.5 = 50 bbl; sixtel short by 180×(1/6)=30 bbl.
        let formats = vec![
            fmt("FP-PALE-1-2-BBL", HALF, 100, 0),
            fmt("FP-PALE-1-6-BBL", SIXTEL, 180, 0),
        ];
        let kegs = allocate_batch(158.0, &formats, &[210, 315]);
        // total shortfall 80 bbl → half gets 158×50/80=98.75, sixtel 59.25.
        let half_bbl = kegs[0] as f64 * HALF;
        let sixtel_bbl = kegs[1] as f64 * SIXTEL;
        assert!((half_bbl - 98.75).abs() < 1.0, "half {half_bbl}");
        assert!((sixtel_bbl - 59.25).abs() < 1.0, "sixtel {sixtel_bbl}");
        // Whole batch packaged (∝ need), nothing stranded.
        assert!((allocated_bbl(&formats, &kegs) - 158.0).abs() < 1.0);
    }

    #[test]
    fn neither_short_falls_back_to_seeded_split_as_buffer() {
        // Both formats over target → no shortfall → seeded split.
        let formats = vec![
            fmt("FP-PALE-1-2-BBL", HALF, 100, 5000),
            fmt("FP-PALE-1-6-BBL", SIXTEL, 100, 5000),
        ];
        let kegs = allocate_batch(158.0, &formats, &[210, 315]);
        assert_eq!(kegs, vec![210, 315]); // held as buffer, not dumped
    }

    #[test]
    fn neither_short_no_seed_split_falls_back_to_even() {
        let formats = vec![
            fmt("FP-PALE-1-2-BBL", HALF, 100, 5000),
            fmt("FP-PALE-1-6-BBL", SIXTEL, 100, 5000),
        ];
        // Empty default → even split of 158 bbl: 79 bbl each.
        let kegs = allocate_batch(158.0, &formats, &[]);
        assert_eq!(kegs[0], 158); // 79 / 0.5
        assert_eq!(kegs[1], 474); // 79 / (1/6)
        // Still the whole batch — never dumps.
        assert!((allocated_bbl(&formats, &kegs) - 158.0).abs() < 1.0);
    }

    #[test]
    fn the_251_stranding_case_is_gone_batch_always_packages() {
        // The old both-skip case: both formats "oversupplied" at package
        // time. Old model skipped both → wort stranded. Now the batch still
        // packages (as buffer) — allocated volume is never zero.
        let formats = vec![
            fmt("FP-IPA-1-2-BBL", HALF, 240, 9999),
            fmt("FP-IPA-1-6-BBL", SIXTEL, 240, 9999),
        ];
        let kegs = allocate_batch(133.0, &formats, &[160, 320]);
        assert!(allocated_bbl(&formats, &kegs) > 0.0, "batch must package");
    }
}
