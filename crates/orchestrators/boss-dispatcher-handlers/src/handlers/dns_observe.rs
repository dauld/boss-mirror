//! `dns.observe` — the zone read the declaration was written for.
//!
//! THE DECLARED/OBSERVED SPLIT, APPLIED TO A DNS ZONE (design 4c565f8c,
//! backlog 5e58922c). `infra/cluster/dns/<zone>.toml` says what the
//! zone SHOULD hold; this handler reads what it DOES hold and records
//! the difference on a `dns-zone-observation` packet, one verdict per
//! record: MATCH, DRIFT (both values), ABSENT, UNDECLARED. It is the
//! same shape as `estate.compare` over the cluster's nodes, and for the
//! same reason: a zone nobody compares is a zone whose records are a
//! belief.
//!
//! ONE COMPARATOR. The comparison lives in the tree as
//! `infra/cluster/dns/check-declared.sh` — runnable by hand and in tests
//! with the records as INPUT and no credential. This handler is the
//! READER: it is the only place the broker's Cloudflare root token is
//! readable, so it fetches the zone, resolves the declaration's
//! references, and runs THAT script (`--json`) over what it fetched. A
//! Rust re-implementation here would be a second definition of the
//! comparison (CLAUDE.md §9a), free to disagree with the one an operator
//! runs on a laptop.
//!
//! A `tunnel:<credential id>` target is resolved from the system of
//! record, never a hardcoded uuid: the credential row's
//! `storage_location` names the k8s Secret, the Secret's credentials.json
//! names the TunnelID, and only that id crosses into the script's argv.
//! The tunnel secret never leaves the Secret store.
//!
//! ORDER OF WRITES. On DRIFT or ABSENT the estate alarm
//! `dns_drift:<zone>` is filed — or, when one is already open, REFRESHED
//! with this reading (the estate.alarm idiom: a persisting condition is
//! one packet, never a twin) — BEFORE the observe step is completed. A
//! raise that fails therefore NAKs the firing with the step still ready,
//! and the redelivery re-reads and retries; the opposite order would
//! complete the step, find it completed on redelivery, and lose the
//! alarm. UNDECLARED is recorded on the observation packet only — the
//! paperwork class, reported and not raised.
//!
//! REFUSALS ARE LOUD AND LEAVE THE PACKET OPEN. An unconfigured
//! declarations directory, a missing script, an unresolvable reference,
//! a comparator refusal (exit 2) or an unreadable zone each fails the
//! firing with the cause in the error; the dead letter lands on the
//! `observe` step of a packet that then LOOKS open, and the daily
//! spawner files no twin while it is.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use serde_json::{Value as Json, json};
use tokio::io::AsyncWriteExt;

use super::common::{
    StepEvent, api_client, dispatcher_actor_header, dispatcher_reader_header, get_json, post_json,
    sim_origin_value, write_json,
};
use super::credential_issuer::{SecretStore, ZoneRecords, installed_tunnel_id};

/// The packet kind this handler completes a step of.
pub const OBSERVATION_KIND: &str = "dns-zone-observation";
/// The slug of the machine step it completes.
pub const OBSERVE_SLUG: &str = "observe";
/// The comparator beside the declarations.
pub const COMPARATOR: &str = "check-declared.sh";
/// How many open backlog-items the dedup read is allowed to hold; a
/// page shorter than the list's `total` is a truncated dedup and the
/// raise is HELD rather than made blind (estate.alarm's rule).
const DEDUP_PAGE: usize = 200;

// ---------------------------------------------------------------------------
// Pure pieces — the decisions under test
// ---------------------------------------------------------------------------

/// The estate-alarm dedup key for a zone: one condition per zone,
/// whatever the count of drifted records, so a zone with three
/// findings is one packet the operator reads, not three.
pub fn alarm_key(zone: &str) -> String {
    format!("dns_drift:{zone}")
}

/// The k8s Secret a credential row's `storage_location` names, in the
/// registry's own idiom: `k8s Secret <namespace>/<name> key <key>` —
/// the phrase every row seeded so far opens with
/// (20260916040500-the-cloudflare-tunnel-credential-is-declared.sql).
/// `None` when the row does not say, so the caller can name the row
/// rather than guess a Secret.
pub fn secret_location(storage_location: &str) -> Option<(String, String, String)> {
    let rest = storage_location.trim().strip_prefix("k8s Secret ")?;
    let mut words = rest.split_whitespace();
    let (ns, name) = words.next()?.split_once('/')?;
    if words.next()? != "key" {
        return None;
    }
    let key = words.next()?;
    Some((ns.to_string(), name.to_string(), key.to_string()))
}

/// The comparator's `--json` document, read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub verdicts: Vec<Json>,
    pub summary: String,
    pub hard: usize,
    pub counts: Json,
}

pub fn parse_comparison(stdout: &str) -> Result<Comparison, String> {
    let doc: Json = serde_json::from_str(stdout)
        .map_err(|e| format!("comparator output is not JSON ({e}): {stdout}"))?;
    let verdicts = doc
        .get("verdicts")
        .and_then(Json::as_array)
        .cloned()
        .ok_or_else(|| format!("comparator output carries no verdicts list: {stdout}"))?;
    let summary = doc
        .get("summary")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("comparator output carries no summary: {stdout}"))?
        .to_string();
    let hard = doc
        .get("hard")
        .and_then(Json::as_u64)
        .ok_or_else(|| format!("comparator output carries no hard count: {stdout}"))?;
    Ok(Comparison {
        verdicts,
        summary,
        hard: usize::try_from(hard).unwrap_or(usize::MAX),
        counts: doc.get("counts").cloned().unwrap_or(Json::Null),
    })
}

/// The one body the observe completion sends: the fields
/// dns-zone-observation.toml requires at done, over the step's existing
/// metadata (PATCH-on-PUT replaces `metadata` wholesale).
pub fn observe_put_body(existing: &serde_json::Map<String, Json>, c: &Comparison) -> Json {
    let mut metadata = existing.clone();
    metadata.insert("verdicts".into(), Json::Array(c.verdicts.clone()));
    metadata.insert("summary".into(), json!(c.summary));
    metadata.insert(
        "result".into(),
        json!(if c.hard == 0 { "match" } else { "findings" }),
    );
    metadata.insert("counts".into(), c.counts.clone());
    json!({ "status": "completed", "metadata": metadata })
}

/// Only the verdicts that are findings — what an alarm carries.
fn hard_verdicts(c: &Comparison) -> Vec<Json> {
    c.verdicts
        .iter()
        .filter(|v| {
            matches!(
                v.get("verdict").and_then(Json::as_str),
                Some("DRIFT" | "ABSENT")
            )
        })
        .cloned()
        .collect()
}

/// The urgent packet a drifted zone becomes. Keyed like every estate
/// alarm (`estate_finding`), so the same dedup lens reads it; `area:
/// estate` so it sits with its siblings.
pub fn alarm_body(zone: &str, observation_id: &str, c: &Comparison) -> Json {
    let findings = hard_verdicts(c);
    json!({
        "kind": "backlog-item",
        "title": format!(
            "ESTATE ALARM: DNS zone {zone} disagrees with its declaration ({} finding(s))",
            findings.len()
        ),
        "subject": {"subject_kind": "custom", "id": zone},
        "owner_id": "emp-david",
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": {
            "area": "estate",
            "estate_finding": alarm_key(zone),
            "scope": "dns-zone",
            "zone": zone,
            "latest_observation": observation_id,
            "findings": findings,
            "summary": c.summary,
            "detail": format!(
                "Raised by dns.observe (5e58922c): the zone {zone} was read with the \
                 credential broker's root token and compared to infra/cluster/dns/{zone}.toml; \
                 {}. A DRIFT record says something other than the tree declares (both values \
                 in `findings`); an ABSENT record is declared and not in the zone. Either fix \
                 the zone or change the declaration — nothing applies the declaration yet \
                 (the observer reports the gap; apply is the follow-on car). The full \
                 per-record verdict list, UNDECLARED records included, is on the observe \
                 step of packet {observation_id}. Refreshed on every later reading while open.",
                c.summary
            ),
        },
    })
}

/// The metadata merge a later reading makes on an alarm already open:
/// the latest verdicts and the packet that carried them. A refresh, not
/// a twin.
pub fn alarm_refresh(observation_id: &str, c: &Comparison) -> Json {
    json!({
        "latest_observation": observation_id,
        "findings": hard_verdicts(c),
        "summary": c.summary,
    })
}

/// The open packet already carrying `key`, if any, and whether the page
/// that answered can be trusted to be complete.
pub fn already_open(listing: &Json, key: &str) -> Result<Option<String>, String> {
    let rows: Vec<&Json> = listing
        .get("data")
        .and_then(Json::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let total = listing
        .get("total")
        .and_then(Json::as_u64)
        .map(|t| usize::try_from(t).unwrap_or(usize::MAX));
    match total {
        Some(t) if rows.len() >= t => {}
        _ => {
            return Err(format!(
                "dedup read truncated ({} rows, total {total:?}); the alarm is held for retry rather than twinned",
                rows.len()
            ));
        }
    }
    Ok(rows
        .iter()
        .find(|j| j.pointer("/metadata/estate_finding").and_then(Json::as_str) == Some(key))
        .and_then(|j| j.get("id").and_then(Json::as_str))
        .map(str::to_string))
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

pub struct DnsObserve {
    client: reqwest::Client,
    jobs_base: String,
    zone: Arc<dyn ZoneRecords>,
    secrets: Arc<dyn SecretStore>,
    /// The directory holding `<zone>.toml` and the comparator, or the
    /// reason none is configured.
    declarations: Result<PathBuf, String>,
}

impl DnsObserve {
    pub fn new(
        jobs_base: impl Into<String>,
        zone: Arc<dyn ZoneRecords>,
        secrets: Arc<dyn SecretStore>,
        declarations: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            zone,
            secrets,
            declarations: declarations.map(PathBuf::from).ok_or_else(|| {
                "dns observer unconfigured: BOSS_DNS_DECLARATIONS unset (the image carries \
                 infra/cluster/dns at /opt/boss/infra/cluster/dns)"
                    .to_string()
            }),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    async fn get(&self, path: &str) -> Result<Json, HandlerError> {
        let url = format!("{}{path}", self.base());
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let st = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {st}: {body}"
            )));
        }
        resp.json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} not JSON: {e}")))
    }

    /// Run the comparator with `args`, `stdin` on its stdin. Exit 0/1
    /// are verdicts (stdout returned); 2 and 78 are refusals the
    /// script explains on stderr and a redelivery cannot cure.
    async fn comparator(&self, args: &[String], stdin: &str) -> Result<String, HandlerError> {
        let dir = self
            .declarations
            .as_ref()
            .map_err(|e| HandlerError::Permanent(e.clone()))?;
        let script = dir.join(COMPARATOR);
        if !script.is_file() {
            return Err(HandlerError::Permanent(format!(
                "dns observer: comparator {} is not in the image (BOSS_DNS_DECLARATIONS={})",
                script.display(),
                dir.display()
            )));
        }
        let mut child = tokio::process::Command::new("bash")
            .arg(&script)
            .args(args)
            .env("BOSS_DNS_DECLARATIONS", dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| HandlerError::Downstream(format!("spawn {}: {e}", script.display())))?;
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(stdin.as_bytes())
                .await
                .map_err(|e| HandlerError::Downstream(format!("feeding the comparator: {e}")))?;
        }
        let out = child
            .wait_with_output()
            .await
            .map_err(|e| HandlerError::Downstream(format!("waiting for the comparator: {e}")))?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        match out.status.code() {
            Some(0 | 1) => Ok(stdout),
            Some(code @ (2 | 78)) => Err(HandlerError::Permanent(format!(
                "comparator refused (exit {code}): {}",
                stderr.trim()
            ))),
            other => Err(HandlerError::Downstream(format!(
                "comparator exited {other:?}: {}{}",
                stdout.trim(),
                stderr.trim()
            ))),
        }
    }

    /// `tunnel:<credential>` → `--tunnel <credential>=<TunnelID>`, the id
    /// read off the Secret the credential row names.
    async fn resolve_reference(&self, reference: &str) -> Result<[String; 2], HandlerError> {
        let Some(cred) = reference.strip_prefix("tunnel:") else {
            return Err(HandlerError::Permanent(format!(
                "the declaration references {reference:?}, a kind this observer cannot resolve"
            )));
        };
        let row = self.get(&format!("/api/credentials/{cred}")).await?;
        let row = row.get("data").cloned().unwrap_or(row);
        let location = row
            .get("storage_location")
            .and_then(Json::as_str)
            .unwrap_or_default();
        let (ns, name, key) = secret_location(location).ok_or_else(|| {
            HandlerError::Permanent(format!(
                "credential {cred}: storage_location {location:?} does not name a k8s Secret \
                 (`k8s Secret <ns>/<name> key <key>`), so {reference} cannot be resolved"
            ))
        })?;
        let installed = self
            .secrets
            .read_key(&ns, &name, &key)
            .await
            .map_err(HandlerError::Downstream)?;
        let tunnel_id = installed
            .as_deref()
            .and_then(installed_tunnel_id)
            .ok_or_else(|| {
                HandlerError::Downstream(format!(
                    "Secret {ns}/{name} key {key} names no TunnelID — {reference} cannot be resolved \
                     until a rotation installs one"
                ))
            })?;
        Ok(["--tunnel".to_string(), format!("{cred}={tunnel_id}")])
    }

    /// File or refresh the zone's alarm. Reads the open backlog-items
    /// first (a truncated page HOLDS, so a raise is never made blind).
    async fn raise_or_refresh(
        &self,
        rule: &str,
        zone: &str,
        observation_id: &str,
        c: &Comparison,
    ) -> Result<&'static str, HandlerError> {
        let key = alarm_key(zone);
        let listing = get_json(
            &self.client,
            &format!(
                "{}/api/jobs?kind=backlog-item&status=open&limit={DEDUP_PAGE}",
                self.base()
            ),
            rule,
        )
        .await?;
        match already_open(&listing, &key).map_err(HandlerError::Downstream)? {
            Some(open_id) => {
                write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{}/api/jobs/{open_id}/metadata", self.base()),
                    &alarm_refresh(observation_id, c),
                    rule,
                )
                .await?;
                tracing::info!(finding = %key, packet = %open_id, "dns.observe refreshed the open alarm");
                Ok("refreshed")
            }
            None => {
                post_json(
                    &self.client,
                    &format!("{}/api/jobs", self.base()),
                    &alarm_body(zone, observation_id, c),
                    rule,
                )
                .await?;
                tracing::info!(finding = %key, "dns.observe raised an alarm");
                Ok("raised")
            }
        }
    }
}

#[async_trait]
impl Handler for DnsObserve {
    fn name(&self) -> &'static str {
        "dns.observe"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let ev = StepEvent::from_payload(&ctx.event_payload)?;
        // Rides the shared `step.ready.task` subscription: everything
        // that is not this packet's observe step is skipped before a
        // read is spent.
        if ev.kind != "task" {
            return Ok(());
        }
        let zone = arg_string(args, "zone")?;
        let rule = ctx.rule_name.as_str();

        let job = self.get(&format!("/api/jobs/{}", ev.job_id)).await?;
        let job = job.get("data").cloned().unwrap_or(job);
        if job.get("kind").and_then(Json::as_str) != Some(OBSERVATION_KIND) {
            return Ok(());
        }
        let Some(step) = job
            .get("steps")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .find(|s| s.get("id").and_then(Json::as_str) == Some(ev.step_id))
        else {
            return Ok(());
        };
        if step.get("spec_slug").and_then(Json::as_str) != Some(OBSERVE_SLUG)
            || step.get("status").and_then(Json::as_str) != Some("ready")
        {
            // Idempotent: a redelivery finds the step completed and
            // does nothing (the step API would 409 a write anyway).
            return Ok(());
        }
        let existing = step
            .get("metadata")
            .and_then(Json::as_object)
            .cloned()
            .unwrap_or_default();

        // What the declaration references, then each one resolved from
        // the system of record — before the zone is read, so a
        // declaration this observer cannot judge costs no API call.
        let refs = self
            .comparator(&[zone.to_string(), "--list-references".to_string()], "")
            .await?;
        let mut args: Vec<String> = vec![zone.to_string(), "--json".to_string()];
        for reference in refs.lines().map(str::trim).filter(|l| !l.is_empty()) {
            args.extend(self.resolve_reference(reference).await?);
        }

        // The read, with the only token that can make it.
        let records = self
            .zone
            .zone_records(zone)
            .await
            .map_err(HandlerError::Downstream)?;
        let stdout = self
            .comparator(&args, &Json::Array(records).to_string())
            .await?;
        let comparison = parse_comparison(&stdout).map_err(HandlerError::Permanent)?;

        // The alarm FIRST (see the module doc for why), then the step.
        let alarm = if comparison.hard > 0 {
            self.raise_or_refresh(rule, zone, ev.job_id, &comparison)
                .await?
        } else {
            "none"
        };

        let url = format!(
            "{}/api/jobs/{}/steps/{}",
            self.base(),
            ev.job_id,
            ev.step_id
        );
        let resp = self
            .client
            .put(&url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(rule))
            .header("x-sim-origin", sim_origin_value())
            .json(&observe_put_body(&existing, &comparison))
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("PUT {url}: {e}")))?;
        if !resp.status().is_success() {
            let st = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "PUT {url} returned {st}: {body}"
            )));
        }
        tracing::info!(
            zone,
            packet = ev.job_id,
            summary = %comparison.summary,
            alarm,
            "dns.observe recorded the zone"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    // ----- pure pieces -----

    #[test]
    fn the_registry_row_idiom_names_the_secret() {
        assert_eq!(
            secret_location(
                "k8s Secret boss/cloudflare-tunnel-credentials key credentials.json \
                 (cloudflared connection.Credentials: {AccountTag, TunnelSecret, TunnelID})"
            ),
            Some((
                "boss".to_string(),
                "cloudflare-tunnel-credentials".to_string(),
                "credentials.json".to_string()
            ))
        );
        assert_eq!(
            secret_location("k8s Secret boss/boss-credential-broker-root key cloudflare-token"),
            Some((
                "boss".to_string(),
                "boss-credential-broker-root".to_string(),
                "cloudflare-token".to_string()
            ))
        );
        assert_eq!(secret_location("a laptop, somewhere"), None);
        assert_eq!(secret_location("k8s Secret boss/x"), None);
        assert_eq!(secret_location("k8s Secret nonamespace key k"), None);
    }

    fn comparison(hard: usize, verdicts: Json) -> Comparison {
        Comparison {
            verdicts: verdicts.as_array().cloned().unwrap(),
            summary: format!("check-declared: algedonic.dev: {hard} finding(s)"),
            hard,
            counts: json!({"MATCH": 1}),
        }
    }

    #[test]
    fn the_observe_body_completes_with_the_verdicts_and_forks_on_hard() {
        let mut existing = serde_json::Map::new();
        existing.insert("spec_slug".into(), json!("observe"));
        let clean = comparison(
            0,
            json!([{"record": "boss.algedonic.dev A", "verdict": "MATCH"}]),
        );
        let b = observe_put_body(&existing, &clean);
        assert_eq!(b["status"], "completed");
        assert_eq!(b["metadata"]["result"], "match");
        assert_eq!(
            b["metadata"]["spec_slug"], "observe",
            "existing keys ride along"
        );
        assert_eq!(b["metadata"]["verdicts"][0]["verdict"], "MATCH");
        assert_eq!(b["metadata"]["summary"], clean.summary);

        let drifted = comparison(
            1,
            json!([{"record": "boss.algedonic.dev A", "verdict": "DRIFT"}]),
        );
        assert_eq!(
            observe_put_body(&existing, &drifted)["metadata"]["result"],
            "findings"
        );
    }

    #[test]
    fn the_alarm_carries_only_the_hard_verdicts_and_the_estate_key() {
        let c = comparison(
            2,
            json!([
                {"record": "boss.algedonic.dev A", "verdict": "DRIFT"},
                {"record": "playground.algedonic.dev CNAME", "verdict": "MATCH"},
                {"record": "id.algedonic.dev A", "verdict": "UNDECLARED"},
                {"record": "www.algedonic.dev CNAME", "verdict": "ABSENT"},
            ]),
        );
        let b = alarm_body("algedonic.dev", "obs-1", &c);
        assert_eq!(b["kind"], "backlog-item");
        assert_eq!(b["priority"], "urgent");
        assert_eq!(b["metadata"]["estate_finding"], "dns_drift:algedonic.dev");
        assert_eq!(b["metadata"]["area"], "estate");
        assert_eq!(b["metadata"]["latest_observation"], "obs-1");
        let findings = b["metadata"]["findings"].as_array().unwrap();
        assert_eq!(
            findings.len(),
            2,
            "UNDECLARED and MATCH are not alarm material"
        );
        assert!(b["title"].as_str().unwrap().contains("2 finding(s)"));
        assert!(b["metadata"]["detail"].as_str().unwrap().contains("obs-1"));

        let r = alarm_refresh("obs-2", &c);
        assert_eq!(r["latest_observation"], "obs-2");
        assert_eq!(r["findings"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn dedup_reads_the_open_key_and_holds_on_a_truncated_page() {
        let listing = json!({
            "data": [
                {"id": "other", "metadata": {"estate_finding": "not_ready:cp-2"}},
                {"id": "alarm-1", "metadata": {"estate_finding": "dns_drift:algedonic.dev"}},
            ],
            "total": 2,
        });
        assert_eq!(
            already_open(&listing, "dns_drift:algedonic.dev").unwrap(),
            Some("alarm-1".to_string())
        );
        assert_eq!(
            already_open(&listing, "dns_drift:example.org").unwrap(),
            None
        );
        let truncated = json!({"data": [], "total": 201});
        assert!(already_open(&truncated, "dns_drift:algedonic.dev").is_err());
        let no_total = json!({"data": []});
        assert!(
            already_open(&no_total, "dns_drift:algedonic.dev").is_err(),
            "a missing total is treated as truncated, never as zero"
        );
    }

    #[test]
    fn the_comparator_document_is_read_back() {
        let c = parse_comparison(
            r#"{"zone":"z","counts":{"MATCH":1},"hard":0,"summary":"s","verdicts":[{"record":"a A","verdict":"MATCH"}]}"#,
        )
        .unwrap();
        assert_eq!(c.hard, 0);
        assert_eq!(c.summary, "s");
        assert_eq!(c.verdicts.len(), 1);
        assert!(parse_comparison("not json").is_err());
        assert!(parse_comparison(r#"{"hard": 0}"#).is_err());
    }

    // ----- in-memory fakes -----

    struct FakeZone {
        records: Mutex<Result<Vec<Json>, String>>,
        reads: Mutex<usize>,
    }

    impl FakeZone {
        fn with(records: Vec<Json>) -> Arc<Self> {
            Arc::new(Self {
                records: Mutex::new(Ok(records)),
                reads: Mutex::new(0),
            })
        }
        fn dark(msg: &str) -> Arc<Self> {
            Arc::new(Self {
                records: Mutex::new(Err(msg.to_string())),
                reads: Mutex::new(0),
            })
        }
    }

    #[async_trait]
    impl ZoneRecords for FakeZone {
        async fn zone_records(&self, zone_name: &str) -> Result<Vec<Json>, String> {
            assert_eq!(zone_name, "algedonic.dev");
            *self.reads.lock().unwrap() += 1;
            self.records.lock().unwrap().clone()
        }
    }

    #[derive(Default)]
    struct FakeSecrets {
        map: Mutex<HashMap<String, String>>,
    }

    impl FakeSecrets {
        fn seeded(ns: &str, name: &str, key: &str, value: &str) -> Arc<Self> {
            let f = Self::default();
            f.map
                .lock()
                .unwrap()
                .insert(format!("{ns}/{name}/{key}"), value.to_string());
            Arc::new(f)
        }
    }

    #[async_trait]
    impl SecretStore for FakeSecrets {
        async fn read_key(
            &self,
            ns: &str,
            name: &str,
            key: &str,
        ) -> Result<Option<String>, String> {
            Ok(self
                .map
                .lock()
                .unwrap()
                .get(&format!("{ns}/{name}/{key}"))
                .cloned())
        }
        async fn write_key(&self, _n: &str, _s: &str, _k: &str, _v: &str) -> Result<(), String> {
            unreachable!("the observer never writes a Secret")
        }
    }

    const TUNNEL_ID: &str = "d8a8ef3b-0a6e-4a05-839d-1d910f01fef6";

    fn credentials_json() -> String {
        format!(
            r#"{{"AccountTag":"acct-fixture","TunnelSecret":"Zml4dHVyZS1ub3QtYS1yZWFsLXNlY3JldC0wMDAwMDAwMDA=","TunnelID":"{TUNNEL_ID}"}}"#
        )
    }

    fn secrets() -> Arc<FakeSecrets> {
        FakeSecrets::seeded(
            "boss",
            "cloudflare-tunnel-credentials",
            "credentials.json",
            &credentials_json(),
        )
    }

    fn record(name: &str, rtype: &str, content: &str, proxied: bool, ttl: u32) -> Json {
        json!({
            "id": format!("rec-{name}-{rtype}"), "name": name, "type": rtype,
            "content": content, "proxied": proxied, "ttl": ttl,
        })
    }

    /// The zone exactly as measured on 2026-09-16.
    fn as_measured() -> Vec<Json> {
        vec![
            record("boss.algedonic.dev", "A", "10.20.0.33", false, 300),
            record(
                "playground.algedonic.dev",
                "CNAME",
                &format!("{TUNNEL_ID}.cfargotunnel.com"),
                true,
                1,
            ),
        ]
    }

    // ----- jobs-api stub (the house axum idiom) -----

    type Captured = Arc<Mutex<Vec<(String, Json)>>>;

    /// A dns-zone-observation packet at `observe` (ready), the credential
    /// row, and a backlog-item listing holding `open_alarms`. Returns the
    /// base URL and every write captured as (`<VERB> <path>`, body).
    async fn stub_jobs_api(
        observe_status: &'static str,
        open_alarms: Vec<Json>,
        storage_location: &'static str,
    ) -> (String, Captured) {
        use axum::extract::{Path, Query};
        use axum::{Json as AxJson, Router, routing::get, routing::post, routing::put};

        let captured: Captured = Default::default();
        let (c1, c2, c3) = (captured.clone(), captured.clone(), captured.clone());
        let alarms = Arc::new(open_alarms);
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let alarms = alarms.clone();
                    async move {
                        assert_eq!(q.get("kind").map(String::as_str), Some("backlog-item"));
                        assert_eq!(q.get("status").map(String::as_str), Some("open"));
                        AxJson(json!({ "data": *alarms, "total": alarms.len() }))
                    }
                })
                .post(move |AxJson(body): AxJson<Json>| {
                    let c = c1.clone();
                    async move {
                        c.lock().unwrap().push(("POST /api/jobs".into(), body));
                        AxJson(json!({ "id": "alarm-new" }))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| async move {
                    AxJson(json!({
                        "id": id,
                        "kind": "dns-zone-observation",
                        "status": "open",
                        "subject": {"subject_kind": "custom", "id": "algedonic.dev"},
                        "steps": [
                            {"id": "step-due", "spec_slug": "due", "status": "completed", "metadata": {}},
                            {"id": "step-observe", "spec_slug": "observe", "status": observe_status,
                             "metadata": {"kept": "yes"}},
                        ],
                    }))
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(move |Path(id): Path<String>, AxJson(body): AxJson<Json>| {
                    let c = c2.clone();
                    async move {
                        c.lock()
                            .unwrap()
                            .push((format!("PATCH /api/jobs/{id}/metadata"), body));
                        AxJson(json!({ "ok": true }))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                put(move |Path((id, sid)): Path<(String, String)>, AxJson(body): AxJson<Json>| {
                    let c = c3.clone();
                    async move {
                        c.lock()
                            .unwrap()
                            .push((format!("PUT /api/jobs/{id}/steps/{sid}"), body));
                        AxJson(json!({ "ok": true }))
                    }
                }),
            )
            .route(
                "/api/credentials/{id}",
                get(move |Path(id): Path<String>| async move {
                    AxJson(json!({
                        "id": id,
                        "kind": "cloudflare-tunnel-credentials",
                        "storage_location": storage_location,
                    }))
                }),
            )
            .route("/api/credentials/{id}/rotation/{phase}", post(|| async { "" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), captured)
    }

    const LOCATION: &str = "k8s Secret boss/cloudflare-tunnel-credentials key credentials.json \
                            (cloudflared connection.Credentials: {AccountTag, TunnelSecret, TunnelID})";

    fn ctx() -> InvocationContext {
        InvocationContext {
            rule_name: "dns-observe-on-observe-ready".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "step.ready.task".into(),
            event_payload: json!({
                "job_id": "obs-1",
                "step_id": "step-observe",
                "kind": "task",
                "subject_kind": "custom",
                "subject_id": "algedonic.dev",
                "metadata": {},
            }),
        }
    }

    fn zone_args() -> Vec<(String, Value)> {
        vec![("zone".to_string(), Value::String("algedonic.dev".into()))]
    }

    /// The declarations directory the tree ships — the real comparator
    /// and the real algedonic.dev.toml, run by the handler.
    fn declarations() -> Option<String> {
        Some(
            boss_testing::repo_root()
                .join("infra/cluster/dns")
                .display()
                .to_string(),
        )
    }

    fn handler(
        jobs: String,
        zone: Arc<FakeZone>,
        secrets: Arc<FakeSecrets>,
        declarations: Option<String>,
    ) -> Arc<DnsObserve> {
        DnsObserve::new(jobs, zone, secrets, declarations)
    }

    fn writes(c: &Captured) -> Vec<(String, Json)> {
        c.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn the_zone_as_measured_completes_observe_as_match_and_raises_nothing() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_measured());
        let h = handler(jobs, zone.clone(), secrets(), declarations());
        h.invoke(&zone_args(), &ctx())
            .await
            .expect("observation succeeds");

        assert_eq!(*zone.reads.lock().unwrap(), 1);
        let w = writes(&captured);
        assert_eq!(
            w.len(),
            1,
            "one write: the step completion; no alarm: {w:?}"
        );
        let (path, body) = &w[0];
        assert_eq!(path, "PUT /api/jobs/obs-1/steps/step-observe");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["metadata"]["result"], "match");
        assert_eq!(
            body["metadata"]["kept"], "yes",
            "existing step metadata rides along"
        );
        let verdicts = body["metadata"]["verdicts"].as_array().unwrap();
        assert_eq!(verdicts.len(), 2);
        assert!(
            verdicts.iter().all(|v| v["verdict"] == "MATCH"),
            "{verdicts:?}"
        );
        // The tunnel reference was resolved from the Secret's TunnelID
        // and the packet shows the resolution — and never the secret.
        let pg = verdicts
            .iter()
            .find(|v| v["record"] == "playground.algedonic.dev CNAME")
            .unwrap();
        assert_eq!(
            pg["declared"]["content"],
            format!("{TUNNEL_ID}.cfargotunnel.com")
        );
        assert!(
            !body.to_string().contains("Zml4dHVyZS"),
            "tunnel secret leaked: {body}"
        );
        assert!(
            body["metadata"]["summary"]
                .as_str()
                .unwrap()
                .contains("2 match, 0 drift, 0 absent, 0 undeclared")
        );
    }

    #[tokio::test]
    async fn an_undeclared_record_is_recorded_on_the_packet_and_raises_nothing() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let mut live = as_measured();
        live.push(record("id.algedonic.dev", "A", "203.0.113.7", false, 300));
        let h = handler(jobs, FakeZone::with(live), secrets(), declarations());
        h.invoke(&zone_args(), &ctx()).await.unwrap();

        let w = writes(&captured);
        assert_eq!(w.len(), 1, "{w:?}");
        let body = &w[0].1;
        assert_eq!(
            body["metadata"]["result"], "match",
            "UNDECLARED is reported, not a failure"
        );
        let verdicts = body["metadata"]["verdicts"].as_array().unwrap();
        let id = verdicts
            .iter()
            .find(|v| v["record"] == "id.algedonic.dev A")
            .unwrap();
        assert_eq!(id["verdict"], "UNDECLARED");
        assert_eq!(id["live"]["content"], "203.0.113.7");
    }

    #[tokio::test]
    async fn drift_raises_the_alarm_before_completing_the_step_as_findings() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let mut live = as_measured();
        live[1] = record(
            "playground.algedonic.dev",
            "CNAME",
            "00000000-1111-4222-8333-444444444444.cfargotunnel.com",
            true,
            1,
        );
        let h = handler(jobs, FakeZone::with(live), secrets(), declarations());
        h.invoke(&zone_args(), &ctx()).await.unwrap();

        let w = writes(&captured);
        let order: Vec<&str> = w.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            order,
            vec!["POST /api/jobs", "PUT /api/jobs/obs-1/steps/step-observe"],
            "the alarm is filed BEFORE the step completes"
        );
        let alarm = &w[0].1;
        assert_eq!(alarm["kind"], "backlog-item");
        assert_eq!(alarm["priority"], "urgent");
        assert_eq!(
            alarm["metadata"]["estate_finding"],
            "dns_drift:algedonic.dev"
        );
        assert_eq!(alarm["metadata"]["latest_observation"], "obs-1");
        let findings = alarm["metadata"]["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["verdict"], "DRIFT");
        assert_eq!(
            findings[0]["live"]["content"],
            "00000000-1111-4222-8333-444444444444.cfargotunnel.com"
        );
        let step = &w[1].1;
        assert_eq!(step["metadata"]["result"], "findings");
    }

    #[tokio::test]
    async fn a_second_drifted_reading_refreshes_the_open_alarm_never_twins_it() {
        let open = vec![json!({
            "id": "alarm-1", "kind": "backlog-item", "status": "open",
            "metadata": {"estate_finding": "dns_drift:algedonic.dev"},
        })];
        let (jobs, captured) = stub_jobs_api("ready", open, LOCATION).await;
        let live = vec![as_measured()[1].clone()]; // boss. ABSENT
        let h = handler(jobs, FakeZone::with(live), secrets(), declarations());
        h.invoke(&zone_args(), &ctx()).await.unwrap();

        let w = writes(&captured);
        let order: Vec<&str> = w.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "PATCH /api/jobs/alarm-1/metadata",
                "PUT /api/jobs/obs-1/steps/step-observe"
            ]
        );
        let patch = &w[0].1;
        assert_eq!(patch["latest_observation"], "obs-1");
        assert_eq!(patch["findings"][0]["verdict"], "ABSENT");
        assert_eq!(patch["findings"][0]["record"], "boss.algedonic.dev A");
    }

    #[tokio::test]
    async fn a_zone_that_cannot_be_read_fails_the_firing_and_writes_nothing() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let h = handler(
            jobs,
            FakeZone::dark("GET /zones?name=algedonic.dev returned 403"),
            secrets(),
            declarations(),
        );
        let err = h.invoke(&zone_args(), &ctx()).await.unwrap_err();
        assert!(
            matches!(&err, HandlerError::Downstream(m) if m.contains("403")),
            "{err:?}"
        );
        assert!(
            writes(&captured).is_empty(),
            "the packet stays open at observe"
        );
    }

    #[tokio::test]
    async fn no_installed_tunnel_id_fails_the_firing_naming_the_secret() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_measured());
        let h = handler(
            jobs,
            zone.clone(),
            Arc::new(FakeSecrets::default()),
            declarations(),
        );
        let err = h.invoke(&zone_args(), &ctx()).await.unwrap_err();
        assert!(
            matches!(&err, HandlerError::Downstream(m)
                if m.contains("boss/cloudflare-tunnel-credentials") && m.contains("TunnelID")),
            "{err:?}"
        );
        assert_eq!(
            *zone.reads.lock().unwrap(),
            0,
            "no zone read spent on a declaration it cannot judge"
        );
        assert!(writes(&captured).is_empty());
    }

    #[tokio::test]
    async fn a_credential_row_that_names_no_secret_is_a_permanent_refusal() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], "a laptop, somewhere").await;
        let h = handler(
            jobs,
            FakeZone::with(as_measured()),
            secrets(),
            declarations(),
        );
        let err = h.invoke(&zone_args(), &ctx()).await.unwrap_err();
        assert!(
            matches!(&err, HandlerError::Permanent(m) if m.contains("storage_location")),
            "{err:?}"
        );
        assert!(writes(&captured).is_empty());
    }

    #[tokio::test]
    async fn unconfigured_declarations_refuse_permanently_naming_the_knob() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let h = handler(jobs, FakeZone::with(as_measured()), secrets(), None);
        let err = h.invoke(&zone_args(), &ctx()).await.unwrap_err();
        assert!(
            matches!(&err, HandlerError::Permanent(m) if m.contains("BOSS_DNS_DECLARATIONS")),
            "{err:?}"
        );
        assert!(writes(&captured).is_empty());
    }

    #[tokio::test]
    async fn a_directory_without_the_comparator_refuses_permanently_naming_it() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let empty = boss_testing::scratch_dir("dns-observe-no-comparator");
        let h = handler(
            jobs,
            FakeZone::with(as_measured()),
            secrets(),
            Some(empty.display().to_string()),
        );
        let err = h.invoke(&zone_args(), &ctx()).await.unwrap_err();
        assert!(
            matches!(&err, HandlerError::Permanent(m) if m.contains(COMPARATOR)),
            "{err:?}"
        );
        assert!(writes(&captured).is_empty());
    }

    #[tokio::test]
    async fn a_redelivery_on_a_completed_step_does_nothing() {
        let (jobs, captured) = stub_jobs_api("completed", vec![], LOCATION).await;
        let zone = FakeZone::with(as_measured());
        let h = handler(jobs, zone.clone(), secrets(), declarations());
        h.invoke(&zone_args(), &ctx()).await.unwrap();
        assert_eq!(*zone.reads.lock().unwrap(), 0);
        assert!(writes(&captured).is_empty());
    }

    #[tokio::test]
    async fn a_task_step_of_another_kind_of_packet_is_skipped() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_measured());
        let h = handler(jobs, zone.clone(), secrets(), declarations());
        let mut c = ctx();
        c.event_payload["kind"] = json!("credential-rotation");
        h.invoke(&zone_args(), &c).await.unwrap();
        assert_eq!(*zone.reads.lock().unwrap(), 0);
        assert!(writes(&captured).is_empty());
    }
}
