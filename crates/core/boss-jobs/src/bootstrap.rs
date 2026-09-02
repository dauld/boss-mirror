//! Tenant Workflow-publish bootstrap, shared by tenant `prepare`
//! steps (the brewery's converged prepare and the used-device-shop's
//! prepare both call this) — the Workflow-registry sibling of
//! `boss_policy::bootstrap::publish_policy_rules`: one impl seeds a
//! tenant's `workflows.toml` through the public API so the offline
//! regen, the live demo, and a fresh-VM install cannot drift.
//!
//! [`publish_workflows`] opens one `workflow-design` Job per
//! Workflow in the seed file, walks it to closure, and lets the
//! `workflow-publish` dispatch path land the spec in the registry.
//!
//! Tenant kinds arrive with full provenance this way: audit_log
//! captures the meta-Job that authored each, including author /
//! approver / published-at. See
//! [`crate::registry::platform_workflows`] for the meta-kind itself.
//!
//! Idempotent: if a `workflow-design` Job has already published a
//! given target kind (the registry has an active row with an
//! `authoring_job_id`), the publish skips it. Re-running after a
//! partial failure resumes from where it left off.
//!
//! Hard-fails on any non-2xx response. The seed regens that consume
//! this output expect every kind to actually land in the registry.

use std::path::Path;

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::registry::WorkflowSpec;

/// Open one `workflow-design` Job per Workflow in `seeds`, walk each
/// to closure, and let the `workflow-publish` dispatch path land the
/// spec in the registry.
///
/// `api_base` is the jobs-api (or gateway) base URL; `owning_team`
/// is stamped on every loaded spec (convention: the tenant id or
/// `"<tenant>-bootstrap"` — see
/// [`crate::seed_loader::load_workflows_with_owning_team`]); `dev`
/// auto-walks the sign-off step (development only);
/// `force_republish` re-publishes even already-operator-published
/// kinds (each lands as a new version); `x_boss_user` overrides the
/// default `automation:bootstrap` / `platform-admin` / `operator`
/// header when `Some`.
///
/// Idempotent + hard-fails on any non-2xx response — see the
/// module docs.
pub fn publish_workflows(
    api_base: &str,
    seeds: &Path,
    owning_team: &str,
    dev: bool,
    force_republish: bool,
    x_boss_user: Option<&str>,
) -> Result<()> {
    let user_header = x_boss_user.map(|s| s.to_string()).unwrap_or_else(|| {
        json!({
            "id": "automation:bootstrap",
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "platform",
        })
        .to_string()
    });
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-boss-user",
        reqwest::header::HeaderValue::from_str(&user_header).context("x-boss-user header value")?,
    );
    boss_core::machine_token::attach(&mut headers);
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let specs = crate::seed_loader::load_workflows_with_owning_team(seeds, owning_team)
        .with_context(|| format!("loading workflows.toml for `{owning_team}`"))?;

    info!(
        seeds = %seeds.display(),
        api_base = %api_base,
        owning_team = %owning_team,
        kind_count = specs.len(),
        dev = dev,
        "starting workflow bootstrap"
    );

    let mut published = 0usize;
    let mut skipped = 0usize;
    for spec in &specs {
        // Skip if already operator-published. The registry's
        // `created_by` discriminator is the source of truth: rows
        // landed via a Job have `created_by = "job-<uuid>"`,
        // rows that came from `platform_workflows()` carry
        // `created_by = "bootstrap"`.
        match active_kind_provenance(&client, api_base, &headers, &spec.kind)? {
            Provenance::OperatorPublished if !force_republish => {
                info!(kind = %spec.kind, "already operator-published; skipping");
                skipped += 1;
                continue;
            }
            Provenance::OperatorPublished => {
                info!(kind = %spec.kind, "already operator-published; --force-republish set, publishing new version");
            }
            Provenance::BootstrapOwned | Provenance::Missing => {}
        }
        bootstrap_kind(&client, api_base, &headers, spec, dev)
            .with_context(|| format!("bootstrap of `{}`", spec.kind))?;
        published += 1;
    }

    info!(
        published,
        skipped,
        total = specs.len(),
        owning_team = %owning_team,
        "workflow bootstrap complete"
    );
    Ok(())
}

fn jobs_url(api_base: &str, path: &str) -> String {
    format!("{}{}", api_base.trim_end_matches('/'), path)
}

/// Where the active row for `kind` came from. `Missing` means no
/// row exists; `BootstrapOwned` is from `platform_workflows()`;
/// `OperatorPublished` is from a real Job (or an admin PUT).
#[derive(Debug, PartialEq, Eq)]
enum Provenance {
    Missing,
    BootstrapOwned,
    OperatorPublished,
}

/// Classify an active-kind response body. The wire shape doesn't
/// expose `created_by`; the next-best signal we have without a schema
/// change is `authoring_job_id`, which is set iff the row came from
/// `publish_authored`. A `bootstrap`-owned row never has it.
fn provenance_of(body: &Value) -> Provenance {
    if body
        .get("authoring_job_id")
        .and_then(|v| v.as_str())
        .is_some()
    {
        Provenance::OperatorPublished
    } else {
        Provenance::BootstrapOwned
    }
}

fn active_kind_provenance(
    client: &Client,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    kind: &str,
) -> Result<Provenance> {
    let url = jobs_url(api_base, &format!("/api/workflows/{kind}"));
    let resp = client.get(&url).headers(headers.clone()).send()?;
    if resp.status() == 404 {
        return Ok(Provenance::Missing);
    }
    if !resp.status().is_success() {
        anyhow::bail!(
            "GET {url} → {} {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    let body: Value = resp.json()?;
    Ok(provenance_of(&body))
}

fn bootstrap_kind(
    client: &Client,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    target: &WorkflowSpec,
    dev: bool,
) -> Result<()> {
    info!(kind = %target.kind, "opening workflow-design Job");

    // 1. POST /api/jobs to open a workflow-design Job whose
    //    Subject points at the target kind. The metadata carries
    //    a placeholder; metadata for individual steps gets PUT
    //    in subsequent calls.
    let create_body = json!({
        "kind": "workflow-design",
        // Subject is uniformly a {subject_kind, id} pair — every
        // kind, including this meta-Job's `workflow` subject, uses
        // the same shape.
        "subject": {
            "subject_kind": "workflow",
            "id": target.kind,
        },
        "title": format!("Design `{}`", target.kind),
        "owner_id": "automation:bootstrap",
        "status": "open",
        "priority": "standard",
        // opened_on is intentionally omitted so the jobs-api stamps it
        // from the sim clock (its create_job default) — the SAME clock the
        // step-walk below closes the Job against. A hardcoded epoch date
        // diverged from the prior-day seed anchor the close lands on
        // (seed_tenant_data's configure_clock_to_epoch rebases the clock to
        // the prior day), so closed_on < opened_on whenever Workflows publish
        // after that rebase — tripping the lifecycle-ordering invariant.
        "metadata": json!({
            "target_kind": target.kind,
        }),
        "tags": [],
    });
    let create_url = jobs_url(api_base, "/api/jobs");
    let resp = client
        .post(&create_url)
        .headers(headers.clone())
        .json(&create_body)
        .send()?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "POST {create_url} → {} {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    let job: Value = resp.json()?;
    let job_id = job
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("POST /api/jobs returned no id"))?
        .to_string();
    info!(kind = %target.kind, %job_id, "Job opened");

    // 2. List the Job's steps. The materializer expanded the four
    //    tiers into four steps; we walk them in sort_order.
    let steps_url = jobs_url(api_base, &format!("/api/jobs/{job_id}/steps"));
    let resp = client.get(&steps_url).headers(headers.clone()).send()?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "GET {steps_url} → {} {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    let mut steps: Vec<Value> = resp.json()?;
    steps.sort_by_key(|s| s.get("sort_order").and_then(|v| v.as_i64()).unwrap_or(0));

    // 3. Walk each step to done.
    for step in &steps {
        let step_id = step
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("step missing id"))?;
        let step_kind = step.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        walk_step(
            client, api_base, headers, &job_id, step_id, step_kind, step, target, dev,
        )
        .with_context(|| format!("walk_step `{step_kind}` ({step_id})"))?;
    }

    info!(kind = %target.kind, "publish complete");
    Ok(())
}

/// The completion metadata a walked step needs: every `fields[]` entry
/// the materialized step declares `required` that neither the step's
/// existing metadata nor the Workflow's defaults already carry, filled
/// with a type-appropriate value. Sibling of the sim workforce's
/// `synth_field_value` (boss-sim), kept local because core cannot
/// depend on an orchestrator; the value policy is deliberately simpler
/// — a publish walk is a dev-mode artifact, not a population.
fn synthesized_completion_metadata(step: &Value) -> serde_json::Map<String, Value> {
    let existing = step
        .get("metadata")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut out = existing.clone();
    let mut added = false;
    for f in step
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if !f.get("required").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let Some(name) = f.get("name").and_then(Value::as_str) else {
            continue;
        };
        if existing.contains_key(name) {
            continue;
        }
        let ftype = f.get("field_type").and_then(Value::as_str).unwrap_or("");
        let value = match ftype {
            "number" | "integer" => json!(1),
            "boolean" => json!(true),
            "array" => json!([]),
            "object" => json!({}),
            // Fixed sentinels, not wall-clock: the walker runs at seed
            // time and must stay deterministic (no-wallclock).
            "date" => json!("2026-01-01"),
            "date-time" => json!("2026-01-01T00:00:00Z"),
            "uri" => json!("https://docs.example.internal/sop"),
            s if s.contains('|') => {
                json!(s.split('|').next().unwrap_or("").trim())
            }
            _ => json!(format!(
                "{} (walked at publish)",
                name.replace(['-', '_'], " ")
            )),
        };
        out.insert(name.to_string(), value);
        added = true;
    }
    if !added {
        return serde_json::Map::new();
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn walk_step(
    client: &Client,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    job_id: &str,
    step_id: &str,
    step_kind: &str,
    step: &Value,
    target: &WorkflowSpec,
    dev: bool,
) -> Result<()> {
    let url = jobs_url(api_base, &format!("/api/jobs/{job_id}/steps/{step_id}"));

    // PATCH-shape body — the boss-jobs HTTP handler uses PUT with
    // overlay semantics. Fields we omit get preserved.
    let body = match step_kind {
        "task" => {
            // The materialized step may REQUIRE fields at completion —
            // and the live registry's workflow-design version can carry
            // required fields the tree's fixtures never saw. On
            // 2026-09-02 a bare {"status":"completed"} met a live
            // `sign_off_context` requirement, the 400 aborted the
            // brewery prepare, and the container crash-looped: a
            // registry ROW bricked production boot. Data must never be
            // able to do that, so the walker now fills whatever the
            // step declares as required, the way the sim workforce
            // fills a form before marking it done. Existing metadata
            // keys always win; the merge is top-level because the step
            // PUT replaces metadata wholesale.
            let synthesized = synthesized_completion_metadata(step);
            if synthesized.is_empty() {
                json!({ "status":"completed" })
            } else {
                json!({ "status":"completed", "metadata": synthesized })
            }
        }
        "sign-off" => {
            if !dev {
                anyhow::bail!(
                    "sign-off step must be approved by a real reviewer; \
                     re-run with --dev for unattended bootstrap (development only)"
                );
            }
            // Sign-off contract: metadata lands first, the stamp attests the
            // final shape, then the status flip below completes it.
            //
            // The `workflow-design` approve step requires the
            // `workflow-approver` authority (boss-jobs registry), so the
            // stamp's `role` must equal that — the sign-off endpoint
            // rejects any role not in `sign_offs_required`. We stamp as the
            // `platform-admin` automation identity, which holds
            // `step-signoff:workflow-approver` via the core policy defaults;
            // seed-time provisioning therefore never depends on the tenant's
            // approver grants having loaded first.
            let md_url = jobs_url(api_base, &format!("/api/jobs/{job_id}/steps/{step_id}"));
            let md_resp = client
                .put(&md_url)
                .headers(headers.clone())
                .json(&json!({
                    "metadata": {
                        "authority_role": "workflow-approver",
                        "signed_by": "emp-cto",
                    },
                }))
                .send()
                .with_context(|| format!("PUT {md_url}"))?;
            if !md_resp.status().is_success() {
                let status = md_resp.status();
                let body = md_resp.text().unwrap_or_default();
                anyhow::bail!("PUT {md_url} returned {status}: {body}");
            }
            let stamp_url = jobs_url(
                api_base,
                &format!("/api/jobs/{job_id}/steps/{step_id}/sign-offs"),
            );
            let stamper = json!({
                "id": "emp-cto",
                "role": "platform-admin",
                "access_tier": "operator",
                "territory_account_ids": [],
                "direct_report_ids": [],
                "department": "executive",
            })
            .to_string();
            let resp = client
                .post(&stamp_url)
                .headers(headers.clone())
                .header(
                    "x-boss-user",
                    reqwest::header::HeaderValue::from_str(&stamper).context("stamper header")?,
                )
                .json(&json!({ "role": "workflow-approver" }))
                .send()
                .with_context(|| format!("POST {stamp_url}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().unwrap_or_default();
                anyhow::bail!("POST {stamp_url} returned {status}: {body}");
            }
            json!({ "status":"completed" })
        }
        "workflow-publish" => {
            // The terminal step. Metadata MUST carry the full
            // WorkflowSpec so the dispatch handler in
            // boss-jobs::http::update_step can call
            // publish_authored.
            let spec_value = serde_json::to_value(target)
                .context("serializing WorkflowSpec for publish step")?;
            json!({
                "status":"completed",
                "metadata": {
                    "workflow_spec": spec_value,
                },
            })
        }
        other => {
            warn!(step_kind = %other, "unrecognized step kind on workflow-design; flipping to done");
            json!({ "status":"completed" })
        }
    };

    let resp = client
        .put(&url)
        .headers(headers.clone())
        .json(&body)
        .send()?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "PUT {url} → {} {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    info!(step_kind, "step done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authoring_job_id_marks_operator_published() {
        let body = json!({ "kind": "device-intake", "authoring_job_id": "job-abc" });
        assert_eq!(provenance_of(&body), Provenance::OperatorPublished);
    }

    #[test]
    fn missing_authoring_job_id_is_bootstrap_owned() {
        let body = json!({ "kind": "device-intake" });
        assert_eq!(provenance_of(&body), Provenance::BootstrapOwned);
        let null_id = json!({ "kind": "device-intake", "authoring_job_id": null });
        assert_eq!(provenance_of(&null_id), Provenance::BootstrapOwned);
    }

    #[test]
    fn jobs_url_trims_trailing_slash() {
        assert_eq!(
            jobs_url("http://localhost:7900/", "/api/jobs"),
            "http://localhost:7900/api/jobs"
        );
    }
}

#[cfg(test)]
mod walker_tests {
    use super::*;
    use serde_json::json;

    /// The 2026-09-02 crash-loop shape verbatim: a live-registry task
    /// step requiring `sign_off_context` that the walker's bare
    /// completion missed. The synthesized metadata must carry it, and
    /// must MERGE (not replace) the step's existing keys, because the
    /// step PUT replaces metadata wholesale.
    #[test]
    fn the_walker_fills_a_live_required_field_and_merges() {
        let step = json!({
            "metadata": { "already": "here" },
            "fields": [
                {"name": "sign_off_context", "field_type": "string", "required": true},
                {"name": "optional_note", "field_type": "string", "required": false}
            ]
        });
        let md = synthesized_completion_metadata(&step);
        assert!(
            md.get("sign_off_context")
                .and_then(|v| v.as_str())
                .is_some()
        );
        assert_eq!(
            md.get("already"),
            Some(&json!("here")),
            "existing keys ride along"
        );
        assert!(!md.contains_key("optional_note"), "optional stays unfilled");
    }

    /// Nothing required, or everything already present -> EMPTY map,
    /// so the completion body stays the bare status flip and omitted
    /// metadata is preserved server-side (overlay semantics).
    #[test]
    fn a_satisfied_step_synthesizes_nothing() {
        assert!(synthesized_completion_metadata(&json!({})).is_empty());
        let satisfied = json!({
            "metadata": { "sign_off_context": "authored" },
            "fields": [{"name": "sign_off_context", "field_type": "string", "required": true}]
        });
        assert!(synthesized_completion_metadata(&satisfied).is_empty());
    }

    /// Type-appropriateness for the shapes the registry declares —
    /// enums take their first variant, numbers count, dates are fixed
    /// sentinels (the walker is seed-time and must be deterministic).
    #[test]
    fn synthesized_values_are_type_appropriate() {
        let step = json!({ "fields": [
            {"name": "verdict", "field_type": "pass|fail", "required": true},
            {"name": "count", "field_type": "number", "required": true},
            {"name": "when", "field_type": "date", "required": true}
        ]});
        let md = synthesized_completion_metadata(&step);
        assert_eq!(md.get("verdict"), Some(&json!("pass")));
        assert_eq!(md.get("count"), Some(&json!(1)));
        assert_eq!(md.get("when"), Some(&json!("2026-01-01")));
    }
}
