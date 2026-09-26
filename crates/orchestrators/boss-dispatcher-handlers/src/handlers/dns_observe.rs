//! `dns.observe` — the zone read the declaration was written for, and
//! the one apply it makes behind an interlock.
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
//! ONE COMPARATOR. The zone comparison lives in the tree as
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
//! WHAT STANDS IN FRONT (backlog 198c5fe9). `access.toml` beside the
//! zone file declares the Cloudflare Access applications the edge
//! consults before a request reaches the tunnel. This handler reads the
//! account's applications with the same root token, compares them in
//! the same vocabulary (the comparison is Rust, in this file — there is
//! no second definition of it anywhere, and its verdict gates a write,
//! which belongs where the token is), and APPLIES an ABSENT declared
//! application: creates it and its policies, idempotent by domain, then
//! READS THE ACCOUNT AGAIN so the packet records what is there, never
//! what was sent. DRIFT is reported and alarmed, never corrected.
//!
//! An application declaring `short_lived_ca = true` also gets its
//! short-lived-certificate CA: generated when the account holds none
//! for it, and its PUBLIC key recorded on the packet as `ssh_ca` either
//! way — the value the ssh server's `TrustedUserCAKeys` must hold. The
//! dashboard offers only the account-wide Access-for-Infrastructure CA
//! since 2026-09, which signs nothing `cloudflared access ssh` asks
//! for; without the per-application CA the client is refused "bad ca
//! application" before it reaches the server (incident 55d001b0).
//!
//! THE INTERLOCK. A zone record declaring `interlock = "access"` is
//! applied — created when ABSENT (replacing whatever other type the
//! name held), corrected when DRIFT — only once the application
//! declared for that name reads present with a policy whose decision is
//! `allow`, in the same firing. Otherwise the record stays as it is and
//! its verdict is rewritten HELD with `flip held — Access app absent`
//! (or `... has no allow policy`): reported on the packet, not counted
//! as a finding, because a held flip is a designed wait and not drift.
//! After an apply the zone is read and compared again, so the verdicts
//! on the packet are the zone AFTER the write; an apply that did not
//! take is then a plain ABSENT/DRIFT — a hard finding, alarmed.
//!
//! ORDER OF WRITES. On any hard finding (a zone DRIFT/ABSENT, or an
//! Access DRIFT/ABSENT) the estate alarm `dns_drift:<zone>` is filed —
//! or, when one is already open, REFRESHED with this reading (the
//! estate.alarm idiom: a persisting condition is one packet, never a
//! twin) — BEFORE the observe step is completed. A raise that fails
//! therefore NAKs the firing with the step still ready, and the
//! redelivery re-reads and retries; the opposite order would complete
//! the step, find it completed on redelivery, and lose the alarm.
//! UNDECLARED and HELD are recorded on the observation packet only —
//! the paperwork class, reported and not raised.
//!
//! REFUSALS ARE LOUD AND LEAVE THE PACKET OPEN. An unconfigured
//! declarations directory, a missing script or access.toml, an
//! unresolvable reference, a comparator refusal (exit 2) or an
//! unreadable zone or account each fails the firing with the cause in
//! the error; the dead letter lands on the `observe` step of a packet
//! that then LOOKS open, and the daily spawner files no twin while it
//! is.

use boss_jobs::channels::InputChannel;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tokio::io::AsyncWriteExt;

use super::common::{
    StepEvent, api_client, complete_step, dispatcher_reader_header, get_json, owner_for_filing,
    post_json, row_or_refuse, rows_or_refuse, sim_origin_value, write_json,
};
use super::credential_issuer::{
    AccessApp, AccessAppSpec, AccessApps, AccessPolicy, AccessPolicySpec, SecretStore,
    ZoneRecordSpec, ZoneRecords, installed_tunnel_id,
};

/// The packet kind this handler completes a step of.
pub const OBSERVATION_KIND: &str = "dns-zone-observation";
/// The slug of the machine step it completes.
pub const OBSERVE_SLUG: &str = "observe";
/// The comparator beside the declarations.
pub const COMPARATOR: &str = "check-declared.sh";
/// The Access declaration beside the zone files.
pub const ACCESS_DECLARATION: &str = "access.toml";
/// The two interlocks the handler honours (the comparator refuses any
/// other value, so a record can never declare one nothing reads).
pub const INTERLOCK_ACCESS: &str = "access";
/// Released once the latest cluster converge packet records the tunnel
/// routing the hostname (`tunnel_ingress`) with the connector
/// connected — for a hostname the tunnel serves that no Access
/// application fronts: the identity provider (fd75c641, 2026-09-16).
pub const INTERLOCK_TUNNEL: &str = "tunnel";
/// The chore whose packet carries the converge's `tunnel_ingress` and
/// `cloudflared` facts (infra/forge/cluster-deploy-runner.service).
pub const CONVERGE_KIND: &str = "maintenance-cluster-converge";
/// How many open estate packets (backlog-items carrying
/// `estate_finding`, c5ac71de) the dedup read is allowed to hold; a
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

// ----- the Access declaration ------------------------------------------

/// `access.toml`: the applications in front of the zone's proxied
/// hostnames and the OIDC redirects the gateway needs registered. The
/// redirects are not acted on here; they are parsed so the whole file
/// has ONE reader, and the check that judges a manifest's
/// BOSS_PUBLIC_URL against them
/// (`tests/a_public_url_names_a_registered_oidc_redirect.rs`) reads
/// this parse rather than a second one of its own (backlog 63a92827).
///
/// CLOSED KEY SET, every table (backlog 1c1591fa, 2026-09-19; the same
/// sweep as the rule registry's a2358e7c F3). This file is authored
/// tree data — a registry by any other name — and until this it was
/// read by an OPEN set, so a typo'd key was dropped in silence and the
/// sentence above was untrue of exactly the entries it spoke for. The
/// compounding part was the redirect lint: it would report green over
/// a `[[oidc_redirect]]` the observer never saw, a check passing on
/// absence. A key this struct set does not read is now a refusal at
/// the read, naming the key — and since 63a92827 that lint's awk is
/// gone, so there is no second reader left to pass on the absence.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessDeclaration {
    pub account_zone: String,
    #[serde(default)]
    pub application: Vec<DeclaredApp>,
    #[serde(default)]
    pub oidc_redirect: Vec<DeclaredRedirect>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredApp {
    pub name: String,
    pub domain: String,
    #[serde(rename = "type")]
    pub app_type: String,
    pub session_duration: String,
    pub why: String,
    /// Generate this application's short-lived-certificate CA when the
    /// account holds none, and record its public key (see the module
    /// doc). Only an ssh door needs one.
    #[serde(default)]
    pub short_lived_ca: bool,
    #[serde(default)]
    pub policy: Vec<DeclaredPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredPolicy {
    pub name: String,
    pub decision: String,
    pub include: DeclaredInclude,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredInclude {
    #[serde(default)]
    pub emails: Vec<String>,
    /// Cloudflare's `{"everyone": {}}` rule — the shape of a bypass
    /// policy on a callback path (`playground.algedonic.dev/auth`,
    /// read 2026-09-16, design 08b8b396 q2). A policy for everyone
    /// includes somebody, so it passes the includes-nobody check.
    #[serde(default)]
    pub everyone: bool,
    /// The policy's members are the DASHBOARD's, not the tree's: the
    /// declaration names the policy and its decision, and the include
    /// set is neither declared nor compared. For a policy that admits
    /// individuals by personal address — the playground's onboarding
    /// visitors — in a file the public mirror publishes (67e754cc,
    /// 2026-09-18). Such a policy is never created by the handler: a
    /// policy created with no include matches nobody.
    #[serde(default)]
    pub unmanaged: bool,
}

impl DeclaredInclude {
    /// The rules as the account lists them, in Cloudflare's own shape.
    pub fn rules(&self) -> Vec<Json> {
        let mut rules: Vec<Json> = self
            .emails
            .iter()
            .map(|e| json!({"email": {"email": e}}))
            .collect();
        if self.everyone {
            rules.push(json!({"everyone": {}}));
        }
        rules
    }
    /// The canonical set a comparison reads: `email:<addr>` and
    /// `everyone` — the same spelling [`include_rules`] gives a live list.
    fn canonical(&self) -> BTreeSet<String> {
        let mut set: BTreeSet<String> = self.emails.iter().map(|e| format!("email:{e}")).collect();
        if self.everyone {
            set.insert("everyone".into());
        }
        set
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredRedirect {
    pub hostname: String,
    pub redirect: String,
    pub registered: bool,
    pub measured: String,
}

/// Parse and validate the declaration for `zone`: the declaration
/// must be about this zone's account, every application must name a
/// domain in the zone, and no domain may be declared twice (identity
/// is the domain, so a twin would be applied twice).
pub fn parse_access_declaration(text: &str, zone: &str) -> Result<AccessDeclaration, String> {
    let dec: AccessDeclaration =
        toml::from_str(text).map_err(|e| format!("{ACCESS_DECLARATION}: {e}"))?;
    if dec.account_zone != zone {
        return Err(format!(
            "{ACCESS_DECLARATION} declares account_zone {:?}; observing {zone:?} — refusing to compare another account's declaration",
            dec.account_zone
        ));
    }
    let mut seen = BTreeSet::new();
    for app in &dec.application {
        // An application's domain may carry a path (`<host>/auth`, the
        // callback bypass read 2026-09-16); the zone check is on the host.
        let host = app.domain.split('/').next().unwrap_or_default();
        if host != zone && !host.ends_with(&format!(".{zone}")) {
            return Err(format!(
                "{ACCESS_DECLARATION}: application {:?} domain {:?} is not in zone {zone:?}",
                app.name, app.domain
            ));
        }
        if !seen.insert(app.domain.clone()) {
            return Err(format!(
                "{ACCESS_DECLARATION} names domain {:?} twice — an application's identity is its domain; fix the declaration",
                app.domain
            ));
        }
        for policy in &app.policy {
            let inc = &policy.include;
            if inc.unmanaged && (!inc.emails.is_empty() || inc.everyone) {
                return Err(format!(
                    "{ACCESS_DECLARATION}: application {:?} policy {:?} is unmanaged AND lists who it includes — the members are the dashboard's or the tree's, not both",
                    app.domain, policy.name
                ));
            }
            if inc.emails.is_empty() && !inc.everyone && !inc.unmanaged {
                return Err(format!(
                    "{ACCESS_DECLARATION}: application {:?} policy {:?} includes nobody (include.emails is empty) — a policy that matches nobody is not a declaration",
                    app.domain, policy.name
                ));
            }
        }
    }
    Ok(dec)
}

/// The e-mails a live include-rule list names, if EVERY rule is an
/// e-mail rule; `None` when any rule is of a kind the declaration has
/// no vocabulary for, so the comparison prints the raw rules instead of
/// silently ignoring one.
/// A live include list in the declaration's canonical spelling —
/// `email:<addr>` for an email rule, `everyone` for the everyone rule —
/// or `None` when a rule is of a kind the declaration has no word for
/// (then the policy reads DRIFT and the raw rule is on the verdict).
fn include_rules(include: &[Json]) -> Option<BTreeSet<String>> {
    include
        .iter()
        .map(|rule| {
            if let Some(e) = rule.pointer("/email/email").and_then(Json::as_str) {
                Some(format!("email:{e}"))
            } else if rule.get("everyone").is_some() {
                Some("everyone".to_string())
            } else {
                None
            }
        })
        .collect()
}

fn declared_policy_json(p: &DeclaredPolicy) -> Json {
    let mut j = json!({"name": p.name, "decision": p.decision, "emails": p.include.emails});
    if p.include.everyone {
        j["everyone"] = json!(true);
    }
    if p.include.unmanaged {
        j["unmanaged"] = json!(true);
    }
    j
}

/// A declared policy against a live one of the same name: the
/// decision, and — unless the members are the dashboard's — the
/// include set in the canonical spelling.
fn policy_matches(p: &DeclaredPolicy, l: &AccessPolicy) -> bool {
    l.name == p.name
        && l.decision == p.decision
        && (p.include.unmanaged
            || include_rules(&l.include).as_ref() == Some(&p.include.canonical()))
}

fn live_app_json(a: &AccessApp) -> Json {
    json!({
        "id": a.id,
        "name": a.name,
        "type": a.app_type,
        "session_duration": a.session_duration,
        "policies": a.policies.iter().map(|p| json!({
            "id": p.id, "name": p.name, "decision": p.decision, "include": p.include,
        })).collect::<Vec<_>>(),
    })
}

/// Compare the declared applications to the account's, one entry per
/// declared application (by domain) plus one UNDECLARED entry per live
/// application no declaration names. A DRIFT entry carries BOTH values
/// and, when the drift is a declared policy the application lacks,
/// `policies_absent` naming them — the one drift the handler applies
/// (by creating the policy); every other drift is corrected from the
/// read.
pub fn compare_access(declared: &[DeclaredApp], live: &[AccessApp]) -> Vec<Json> {
    let mut verdicts: Vec<Json> = declared
        .iter()
        .map(|d| {
            let mut entry = json!({
                "application": d.domain,
                "domain": d.domain,
                "why": d.why,
                "declared": {
                    "name": d.name,
                    "type": d.app_type,
                    "session_duration": d.session_duration,
                    "policies": d.policy.iter().map(declared_policy_json).collect::<Vec<_>>(),
                },
            });
            let Some(app) = live.iter().find(|a| a.domain == d.domain) else {
                entry["verdict"] = json!("ABSENT");
                return entry;
            };
            entry["live"] = live_app_json(app);
            let (unmanaged_absent, absent): (Vec<&DeclaredPolicy>, Vec<&DeclaredPolicy>) = d
                .policy
                .iter()
                .filter(|p| !app.policies.iter().any(|l| l.name == p.name))
                .partition(|p| p.include.unmanaged);
            let absent: Vec<&str> = absent.iter().map(|p| p.name.as_str()).collect();
            let unmanaged_absent: Vec<&str> =
                unmanaged_absent.iter().map(|p| p.name.as_str()).collect();
            let policies_match = absent.is_empty()
                && unmanaged_absent.is_empty()
                && app.policies.len() == d.policy.len()
                && d.policy
                    .iter()
                    .all(|p| app.policies.iter().any(|l| policy_matches(p, l)));
            let matches = app.app_type == d.app_type
                && app.session_duration == d.session_duration
                && policies_match;
            entry["verdict"] = json!(if matches { "MATCH" } else { "DRIFT" });
            if !absent.is_empty() {
                entry["policies_absent"] = json!(absent);
            }
            // Reported, never created: a policy whose members live in
            // the dashboard has to be made there, members and all.
            if !unmanaged_absent.is_empty() {
                entry["policies_unmanaged_absent"] = json!(unmanaged_absent);
            }
            entry
        })
        .collect();
    verdicts.extend(
        live.iter()
            .filter(|a| !declared.iter().any(|d| d.domain == a.domain))
            .map(|a| {
                json!({
                    "application": if a.domain.is_empty() { a.name.clone() } else { a.domain.clone() },
                    "domain": a.domain,
                    "verdict": "UNDECLARED",
                    "live": live_app_json(a),
                })
            }),
    );
    verdicts
}

/// What the interlock read for one hostname.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessGate {
    /// Read present with an allow policy before this firing wrote anything.
    Present,
    /// Present with an allow policy because THIS firing created it.
    Created,
    /// No application fronts the hostname.
    Absent,
    /// An application fronts it, but no policy on it allows anyone.
    NoAllowPolicy,
}

impl AccessGate {
    pub fn as_str(self) -> &'static str {
        match self {
            AccessGate::Present => "present",
            AccessGate::Created => "created",
            AccessGate::Absent => "absent",
            AccessGate::NoAllowPolicy => "no-allow-policy",
        }
    }
    pub fn allows(self) -> bool {
        matches!(self, AccessGate::Present | AccessGate::Created)
    }
    /// The phrase a held record carries.
    pub fn held_reason(self) -> Option<&'static str> {
        match self {
            AccessGate::Absent => Some("flip held — Access app absent"),
            AccessGate::NoAllowPolicy => Some("flip held — Access app has no allow policy"),
            AccessGate::Present | AccessGate::Created => None,
        }
    }
}

/// The interlock for `hostname`, read off the account's applications
/// as they are AFTER any create this firing made (`created` names the
/// domains it created).
pub fn access_gate(hostname: &str, live: &[AccessApp], created: &[String]) -> AccessGate {
    match live.iter().find(|a| a.domain == hostname) {
        None => AccessGate::Absent,
        Some(app) if app.policies.iter().any(|p| p.decision == "allow") => {
            if created.iter().any(|d| d == hostname) {
                AccessGate::Created
            } else {
                AccessGate::Present
            }
        }
        Some(_) => AccessGate::NoAllowPolicy,
    }
}

/// What the latest converge recorded about the tunnel: the ingress
/// line (`<host> → <origin>; …`) and the connector's reading.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TunnelFacts {
    pub ingress: String,
    pub connector: String,
}

impl TunnelFacts {
    /// Off the newest `maintenance-cluster-converge` packet's `run` step
    /// (the listing is newest-first). A listing with no such packet
    /// reads as no facts — the gate then holds, never releases. A
    /// listing with no `data` array is no answer and refuses; the caller
    /// states its own fallback (d4698bc2).
    pub fn from_listing(listing: &Json) -> Result<Self, String> {
        let rows: Vec<Json> = rows_or_refuse(listing, "the converge read (GET /api/jobs)")?;
        Ok(rows
            .iter()
            .flat_map(|j| {
                j.get("steps")
                    .and_then(Json::as_array)
                    .into_iter()
                    .flatten()
            })
            .find(|s| s.get("spec_slug").and_then(Json::as_str) == Some("run"))
            .map(|s| {
                let text = |k: &str| {
                    s.pointer(&format!("/metadata/{k}"))
                        .and_then(Json::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                TunnelFacts {
                    ingress: text("tunnel_ingress"),
                    connector: text("cloudflared"),
                }
            })
            .unwrap_or_default())
    }
}

/// What the tunnel interlock read for one hostname.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelGate {
    /// The converge routes the hostname and the connector is connected.
    Routed,
    /// No converge packet routes the hostname (the origin is not
    /// declared, or the declaration has not converged).
    NotRouted,
    /// Routed, but the connector did not read connected on that converge.
    NotConnected,
}

impl TunnelGate {
    pub fn as_str(self) -> &'static str {
        match self {
            TunnelGate::Routed => "routed",
            TunnelGate::NotRouted => "not-routed",
            TunnelGate::NotConnected => "not-connected",
        }
    }
    pub fn allows(self) -> bool {
        matches!(self, TunnelGate::Routed)
    }
    pub fn held_reason(self) -> Option<&'static str> {
        match self {
            TunnelGate::NotRouted => {
                Some("flip held — the converge does not route this hostname through the tunnel")
            }
            TunnelGate::NotConnected => Some(
                "flip held — the connector did not read connected on the converge that routes it",
            ),
            TunnelGate::Routed => None,
        }
    }
}

/// The tunnel interlock for `hostname`, read off the latest converge
/// packet: the `tunnel_ingress` line names `<host> → ` and the
/// connector read `connected`. A line that routes the host's PREFIX
/// (`id.` inside `xid.`) is not a route for it.
pub fn tunnel_gate(hostname: &str, facts: &TunnelFacts) -> TunnelGate {
    let routed = facts
        .ingress
        .split(';')
        .map(str::trim)
        .any(|entry| entry.starts_with(&format!("{hostname} → ")));
    if !routed {
        TunnelGate::NotRouted
    } else if facts.connector != "connected" {
        TunnelGate::NotConnected
    } else {
        TunnelGate::Routed
    }
}

/// One reading per interlock kind, for the verdict annotation and the
/// plan filter: `access` and `tunnel` each carry their own field and
/// their own held phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interlock {
    Access(AccessGate),
    Tunnel(TunnelGate),
    /// The verdict declares no interlock the handler honours: never applied.
    None,
}

impl Interlock {
    pub fn allows(self) -> bool {
        match self {
            Interlock::Access(g) => g.allows(),
            Interlock::Tunnel(g) => g.allows(),
            Interlock::None => false,
        }
    }
    fn held_reason(self) -> Option<&'static str> {
        match self {
            Interlock::Access(g) => g.held_reason(),
            Interlock::Tunnel(g) => g.held_reason(),
            Interlock::None => None,
        }
    }
}

/// The one write the interlock releases for a zone verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZoneApply {
    /// Delete the conflicting records (ids), then create the declared one.
    Create {
        spec: ZoneRecordSpec,
        delete_first: Vec<String>,
    },
    /// Correct the drifted record in place.
    Update {
        record_id: String,
        spec: ZoneRecordSpec,
    },
}

/// Cloudflare's ceiling on a DNS record's `comment` (error 9313,
/// "DNS record comment exceeds the maximum length of 100 characters").
pub const RECORD_COMMENT_MAX: usize = 100;

/// The comment every record this observer writes carries: where it is
/// declared, and which observation applied it (the packet's short id —
/// the full uuid put the first flip's comment at 114 characters).
///
/// MEASURED 2026-09-16 16:00Z (packet 95cf8740): the interlock released
/// the boss. flip, the handler DELETED the A record at that name, and
/// Cloudflare refused the CNAME create on the comment's length — so the
/// name was NXDOMAIN, on the internet and on the LAN, until the comment
/// fit. The write is delete-then-create because Cloudflare will not
/// hold a CNAME beside an A at one name; a create that can be refused
/// on its own shape must therefore be refused HERE, before the delete,
/// which the test below pins for every zone and packet id.
pub fn record_comment(zone: &str, observation_id: &str) -> String {
    let short = &observation_id[..8.min(observation_id.len())];
    let comment = format!("declared in infra/cluster/dns/{zone}.toml; dns.observe {short}");
    // A zone name long enough to breach the ceiling would be the first
    // of its kind; truncate on a char boundary rather than let the
    // delete run ahead of a create Cloudflare will refuse.
    comment.chars().take(RECORD_COMMENT_MAX).collect()
}

impl ZoneApply {
    /// The record name the write is for.
    pub fn name(&self) -> &str {
        match self {
            ZoneApply::Create { spec, .. } | ZoneApply::Update { spec, .. } => &spec.name,
        }
    }
}

/// Stamp the account's refusal onto the verdict for each name whose
/// write it refused: `refused` carries the answer verbatim, and a
/// verdict the re-read judged MATCH regardless (a write refused and a
/// zone that already agreed) is left as it is — the refusal is still
/// recorded, and the zone is still right. A name the re-read no longer
/// lists at all (deleted, then refused) gets an ABSENT verdict of its
/// own so the refusal is never dropped for want of a row to ride.
pub fn annotate_refused(verdicts: &[Json], refused: &[(String, String)]) -> Vec<Json> {
    let mut out: Vec<Json> = verdicts.to_vec();
    for (name, error) in refused {
        match out
            .iter_mut()
            .find(|v| v.get("name").and_then(Json::as_str) == Some(name.as_str()))
        {
            Some(v) => v["refused"] = json!(error),
            None => out.push(json!({
                "name": name,
                "verdict": "ABSENT",
                "refused": error,
            })),
        }
    }
    out
}

fn spec_from_verdict(v: &Json, comment: &str) -> Option<ZoneRecordSpec> {
    let declared = v.get("declared")?;
    Some(ZoneRecordSpec {
        name: v.get("name")?.as_str()?.to_string(),
        record_type: v.get("type")?.as_str()?.to_string(),
        content: declared.get("content")?.as_str()?.to_string(),
        proxied: declared.get("proxied")?.as_bool()?,
        ttl: u32::try_from(declared.get("ttl")?.as_u64()?).ok()?,
        comment: comment.to_string(),
    })
}

fn row_text<'a>(row: &'a Json, key: &str) -> &'a str {
    row.get(key).and_then(Json::as_str).unwrap_or_default()
}

/// The write an interlocked verdict asks for, or `None` when it is
/// MATCH (nothing to do), not interlocked (the observer never applies
/// those), or of a shape the comparator did not produce. `live_rows`
/// are the raw records the comparison was made from — the ids live
/// there. A declared CNAME shares its name with nothing (Cloudflare
/// refuses the create otherwise), so every other record at that name
/// is deleted first; a declared record of any other type coexists.
pub fn plan_zone_apply(v: &Json, live_rows: &[Json], comment: &str) -> Option<ZoneApply> {
    if !matches!(
        v.get("interlock").and_then(Json::as_str),
        Some(INTERLOCK_ACCESS | INTERLOCK_TUNNEL)
    ) {
        return None;
    }
    let spec = spec_from_verdict(v, comment)?;
    match v.get("verdict").and_then(Json::as_str)? {
        "ABSENT" => {
            let delete_first = live_rows
                .iter()
                .filter(|r| {
                    row_text(r, "name") == spec.name
                        && (spec.record_type == "CNAME"
                            || row_text(r, "type").eq_ignore_ascii_case("CNAME"))
                })
                .map(|r| row_text(r, "id").to_string())
                .filter(|id| !id.is_empty())
                .collect();
            Some(ZoneApply::Create { spec, delete_first })
        }
        "DRIFT" => {
            let live_content = v.pointer("/live/content").and_then(Json::as_str)?;
            let record_id = live_rows
                .iter()
                .find(|r| {
                    row_text(r, "name") == spec.name
                        && row_text(r, "type").eq_ignore_ascii_case(&spec.record_type)
                        && row_text(r, "content") == live_content
                })
                .map(|r| row_text(r, "id").to_string())
                .filter(|id| !id.is_empty())?;
            Some(ZoneApply::Update { record_id, spec })
        }
        _ => None,
    }
}

/// Stamp the interlock's reading on every interlocked verdict, and
/// rewrite an ABSENT/DRIFT the interlock did not release as HELD with
/// the reason. Everything else passes through untouched.
pub fn annotate_verdicts(verdicts: &[Json], gate_for: impl Fn(&Json) -> Interlock) -> Vec<Json> {
    verdicts
        .iter()
        .map(|v| {
            let gate = gate_for(v);
            let mut v = v.clone();
            match gate {
                Interlock::Access(g) => v["access"] = json!(g.as_str()),
                Interlock::Tunnel(g) => v["tunnel"] = json!(g.as_str()),
                Interlock::None => return v,
            }
            let hard = matches!(
                v.get("verdict").and_then(Json::as_str),
                Some("ABSENT" | "DRIFT")
            );
            if let (true, Some(reason)) = (hard, gate.held_reason()) {
                v["verdict"] = json!("HELD");
                v["held"] = json!(reason);
            }
            v
        })
        .collect()
}

fn is_hard(v: &Json) -> bool {
    matches!(
        v.get("verdict").and_then(Json::as_str),
        Some("DRIFT" | "ABSENT" | "REFUSED")
    )
}

/// The Access verdict a write the account would not take becomes:
/// the domain it was for, the write in words, and the account's own
/// answer verbatim (the Cloudflare error body names the rule broken —
/// `12130 policy precedences must be unique` was the first).
pub fn refused_verdict(domain: &str, write: &str, error: &str) -> Json {
    json!({
        "domain": domain,
        "verdict": "REFUSED",
        "write": write,
        "error": error,
    })
}

/// One observation, whole: the zone's verdicts (annotated), the
/// Access verdicts, and what this firing wrote. What the step records
/// and what the alarm carries are both functions of this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub zone: Comparison,
    pub access: Vec<Json>,
    pub applied: Vec<String>,
    /// `{domain, public_key}` for every declared short-lived CA the
    /// account holds after this firing.
    pub ssh_ca: Vec<Json>,
}

impl Reading {
    /// Findings only — DRIFT/ABSENT on either side. HELD and UNDECLARED
    /// are paperwork.
    pub fn hard(&self) -> usize {
        self.zone.verdicts.iter().filter(|v| is_hard(v)).count()
            + self.access.iter().filter(|v| is_hard(v)).count()
    }

    fn counts_of(verdicts: &[Json]) -> Json {
        let mut counts =
            json!({"MATCH": 0, "DRIFT": 0, "ABSENT": 0, "UNDECLARED": 0, "HELD": 0, "REFUSED": 0});
        for v in verdicts {
            if let Some(k) = v.get("verdict").and_then(Json::as_str)
                && let Some(n) = counts.get(k).and_then(Json::as_u64)
            {
                counts[k] = json!(n + 1);
            }
        }
        counts
    }

    pub fn counts(&self) -> Json {
        json!({"zone": Self::counts_of(&self.zone.verdicts), "access": Self::counts_of(&self.access)})
    }

    fn held(&self) -> Vec<String> {
        self.zone
            .verdicts
            .iter()
            .filter(|v| v.get("verdict").and_then(Json::as_str) == Some("HELD"))
            .map(|v| {
                format!(
                    "{}: {}",
                    v.get("name").and_then(Json::as_str).unwrap_or_default(),
                    v.get("held").and_then(Json::as_str).unwrap_or_default()
                )
            })
            .collect()
    }

    /// One line an operator reads first: the comparator's own zone
    /// line, the Access counts, what was applied, what was held.
    pub fn summary(&self) -> String {
        let a = Self::counts_of(&self.access);
        let mut s = format!(
            "{} · access: {} match, {} drift, {} absent, {} undeclared",
            self.zone.summary, a["MATCH"], a["DRIFT"], a["ABSENT"], a["UNDECLARED"]
        );
        if a["REFUSED"] != 0 {
            s.push_str(&format!(", {} refused", a["REFUSED"]));
        }
        let refused_zone: Vec<String> = self
            .zone
            .verdicts
            .iter()
            .filter_map(|v| {
                Some(format!(
                    "{}: {}",
                    v.get("name").and_then(Json::as_str)?,
                    v.get("refused").and_then(Json::as_str)?
                ))
            })
            .collect();
        if !refused_zone.is_empty() {
            s.push_str(" · refused by the zone: ");
            s.push_str(&refused_zone.join("; "));
        }
        if !self.applied.is_empty() {
            s.push_str(" · applied: ");
            s.push_str(&self.applied.join("; "));
        }
        let held = self.held();
        if !held.is_empty() {
            s.push_str(" · ");
            s.push_str(&held.join("; "));
        }
        s
    }

    /// Only the verdicts that are findings — what an alarm carries.
    /// Access findings are tagged so the reader can tell the two sides
    /// apart in one list.
    fn findings(&self) -> Vec<Json> {
        self.zone
            .verdicts
            .iter()
            .filter(|v| is_hard(v))
            .cloned()
            .chain(self.access.iter().filter(|v| is_hard(v)).map(|v| {
                let mut v = v.clone();
                v["scope"] = json!("access");
                v
            }))
            .collect()
    }
}

/// The fields the observe completion writes: what
/// dns-zone-observation.toml requires at done, with `access` and
/// `applied` beside them the way `counts` always has. ONLY these: they
/// ride the step merge door, which keeps the step's existing metadata,
/// and the status flips alone after them (backlog e39a9d2a) — this was
/// one PUT of the step's metadata as read plus these keys, which the
/// step PUT refuses once anything wrote the step in between, and
/// refuses outright under the decided end state.
pub fn observe_fields(r: &Reading) -> serde_json::Map<String, Json> {
    let mut metadata = serde_json::Map::new();
    metadata.insert("verdicts".into(), Json::Array(r.zone.verdicts.clone()));
    metadata.insert("access".into(), Json::Array(r.access.clone()));
    metadata.insert("applied".into(), json!(r.applied));
    metadata.insert("ssh_ca".into(), Json::Array(r.ssh_ca.clone()));
    metadata.insert("summary".into(), json!(r.summary()));
    metadata.insert(
        "result".into(),
        json!(if r.hard() == 0 { "match" } else { "findings" }),
    );
    metadata.insert("counts".into(), r.counts());
    metadata
}

/// The urgent packet a drifted zone becomes. Keyed like every estate
/// alarm (`estate_finding`), so the same dedup lens reads it; `area:
/// estate` so it sits with its siblings.
pub fn alarm_body(zone: &str, observation_id: &str, r: &Reading, owner: &str) -> Json {
    let findings = r.findings();
    json!({
        "kind": "backlog-item",
        "title": format!(
            "ESTATE ALARM: DNS zone {zone} disagrees with its declaration ({} finding(s))",
            findings.len()
        ),
        "subject": {"subject_kind": "custom", "id": zone},
        // The platform owner as the registry answers it, or nobody for
        // the jobs API to resolve from the kind's owner_role (3c23662d).
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": {
            "input_channel": super::common::lane_label(InputChannel::Telemetry),
            "area": "estate",
            "estate_finding": alarm_key(zone),
            "scope": "dns-zone",
            "zone": zone,
            "latest_observation": observation_id,
            "findings": findings,
            "summary": r.summary(),
            "detail": format!(
                "Raised by dns.observe (5e58922c, 198c5fe9): the zone {zone} and the Access \
                 applications in front of it were read with the credential broker's root token \
                 and compared to infra/cluster/dns/{zone}.toml and access.toml; {}. A DRIFT \
                 record or application says something other than the tree declares (both \
                 values in `findings`; an Access finding carries scope: access); an ABSENT one \
                 is declared and not there. Either fix the zone/account or change the \
                 declaration — the observer applies only an ABSENT declared Access application \
                 and, behind the Access interlock, a zone record declaring one (a record it \
                 could not apply is HELD on the observation, not here). The full per-record \
                 verdict list, UNDECLARED and HELD included, is on the observe step of packet \
                 {observation_id}. Refreshed on every later reading while open.",
                r.summary()
            ),
        },
    })
}

/// The metadata merge a later reading makes on an alarm already open:
/// the latest verdicts and the packet that carried them. A refresh, not
/// a twin.
pub fn alarm_refresh(observation_id: &str, r: &Reading) -> Json {
    json!({
        "latest_observation": observation_id,
        "findings": r.findings(),
        "summary": r.summary(),
    })
}

/// The open packet already carrying `key`, if any, and whether the page
/// that answered can be trusted to be complete.
pub fn already_open(listing: &Json, key: &str) -> Result<Option<String>, String> {
    // No `data` array refuses for what it is; it was held before only
    // by accident of the truncation check below (d4698bc2).
    let rows: Vec<Json> = rows_or_refuse(listing, "the dedup read (GET /api/jobs)")?;
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
    access: Arc<dyn AccessApps>,
    secrets: Arc<dyn SecretStore>,
    /// The directory holding `<zone>.toml`, `access.toml` and the
    /// comparator, or the reason none is configured.
    declarations: Result<PathBuf, String>,
    /// Who the packets this handler files are owned by — the platform
    /// owner through the port (backlog 3c23662d), resolved once per
    /// invocation by `common::owner_for_filing`; never a literal.
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl DnsObserve {
    pub fn new(
        jobs_base: impl Into<String>,
        zone: Arc<dyn ZoneRecords>,
        access: Arc<dyn AccessApps>,
        secrets: Arc<dyn SecretStore>,
        declarations: Option<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            zone,
            access,
            secrets,
            owner,
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

    fn declarations_dir(&self) -> Result<&PathBuf, HandlerError> {
        self.declarations
            .as_ref()
            .map_err(|e| HandlerError::Permanent(e.clone()))
    }

    /// Run the comparator with `args`, `stdin` on its stdin. Exit 0/1
    /// are verdicts (stdout returned); 2 and 78 are refusals the
    /// script explains on stderr and a redelivery cannot cure.
    async fn comparator(&self, args: &[String], stdin: &str) -> Result<String, HandlerError> {
        let dir = self.declarations_dir()?;
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

    /// The Access declaration beside the zone file, validated for this
    /// zone. Missing or malformed is permanent: nothing a redelivery
    /// cures, and an observer that cannot read the interlock's
    /// declaration must not judge the interlock.
    async fn access_declaration(&self, zone: &str) -> Result<AccessDeclaration, HandlerError> {
        let path = self.declarations_dir()?.join(ACCESS_DECLARATION);
        let text = tokio::fs::read_to_string(&path).await.map_err(|e| {
            HandlerError::Permanent(format!(
                "dns observer: {} is not readable ({e}) — the Access declaration is beside the zone file",
                path.display()
            ))
        })?;
        parse_access_declaration(&text, zone).map_err(HandlerError::Permanent)
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
        let row = row_or_refuse(row, &format!("GET /api/credentials/{cred}"))
            .map_err(HandlerError::Downstream)?;
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

    /// Read the zone and compare it.
    async fn compare_zone(
        &self,
        zone: &str,
        args: &[String],
    ) -> Result<(Vec<Json>, Comparison), HandlerError> {
        let records = self
            .zone
            .zone_records(zone)
            .await
            .map_err(HandlerError::Downstream)?;
        let stdout = self
            .comparator(args, &Json::Array(records.clone()).to_string())
            .await?;
        let comparison = parse_comparison(&stdout).map_err(HandlerError::Permanent)?;
        Ok((records, comparison))
    }

    /// Create every declared application the account lacks, and every
    /// declared policy a present application lacks. Returns what was
    /// written (for the packet), the domains created (for the gate),
    /// and every write the account REFUSED as a `REFUSED` verdict.
    ///
    /// A refusal is a finding, not an error. Until 2026-09-16 it was
    /// an error: the account refused the first policy on boss.
    /// (precedence, below), the handler failed, NATS redelivered it
    /// eight times against the same 400 and dead-lettered — and the
    /// packet recorded the dead letter and NOTHING it had read, the
    /// application it had created, or the interlock it therefore
    /// held. The zone was never read at all. What the account will not
    /// take is exactly what the observation exists to record: it rides
    /// the verdicts, the step completes with findings, the alarm names
    /// it, and the next day's reading retries with what has changed.
    async fn apply_access(
        &self,
        account_id: &str,
        declared: &[DeclaredApp],
        live: &[AccessApp],
        verdicts: &[Json],
    ) -> (Vec<String>, Vec<String>, Vec<Json>) {
        let mut applied = Vec::new();
        let mut created = Vec::new();
        let mut refused = Vec::new();
        // One above every precedence the ACCOUNT holds, counted up
        // across this firing's creates: the uniqueness Cloudflare
        // enforces reaches across applications (AccessPolicySpec).
        let mut next_precedence = live
            .iter()
            .flat_map(|a| a.policies.iter().map(|p| p.precedence))
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        for d in declared {
            let verdict = verdicts
                .iter()
                .find(|v| v.get("domain").and_then(Json::as_str) == Some(d.domain.as_str()));
            let kind = verdict
                .and_then(|v| v.get("verdict"))
                .and_then(Json::as_str)
                .unwrap_or_default();
            let policies: Vec<(&DeclaredPolicy, String)> = match kind {
                "ABSENT" => {
                    let id = match self
                        .access
                        .create_access_app(
                            account_id,
                            &AccessAppSpec {
                                name: d.name.clone(),
                                domain: d.domain.clone(),
                                app_type: d.app_type.clone(),
                                session_duration: d.session_duration.clone(),
                            },
                        )
                        .await
                    {
                        Ok(id) => id,
                        Err(e) => {
                            refused.push(refused_verdict(
                                &d.domain,
                                &format!("create Access application {}", d.domain),
                                &e,
                            ));
                            continue;
                        }
                    };
                    created.push(d.domain.clone());
                    applied.push(format!("Access application {} created", d.domain));
                    d.policy.iter().map(|p| (p, id.clone())).collect()
                }
                "DRIFT" => {
                    let absent: Vec<&str> = verdict
                        .and_then(|v| v.get("policies_absent"))
                        .and_then(Json::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Json::as_str)
                        .collect();
                    let Some(app) = live.iter().find(|a| a.domain == d.domain) else {
                        continue;
                    };
                    d.policy
                        .iter()
                        .filter(|p| absent.contains(&p.name.as_str()))
                        .map(|p| (p, app.id.clone()))
                        .collect()
                }
                _ => Vec::new(),
            };
            for (p, app_id) in &policies {
                let precedence = next_precedence;
                next_precedence = next_precedence.saturating_add(1);
                let written = self
                    .access
                    .create_access_policy(
                        account_id,
                        app_id,
                        &AccessPolicySpec {
                            name: p.name.clone(),
                            decision: p.decision.clone(),
                            include: p.include.rules(),
                            precedence,
                        },
                    )
                    .await;
                match written {
                    Ok(()) => applied.push(format!(
                        "Access policy {} ({}) created on {}",
                        p.name, p.decision, d.domain
                    )),
                    Err(e) => refused.push(refused_verdict(
                        &d.domain,
                        &format!(
                            "create Access policy {} ({}) on {} at precedence {}",
                            p.name, p.decision, d.domain, precedence
                        ),
                        &e,
                    )),
                }
            }
        }
        (applied, created, refused)
    }

    /// Every declared `short_lived_ca` application the account holds:
    /// its CA read, generated when absent, and its public key returned
    /// for the packet. An application the account does not hold is
    /// skipped — its ABSENT or REFUSED verdict already says so. A read
    /// or create the account refuses is a REFUSED finding, the same
    /// rule `apply_access` follows.
    async fn apply_short_lived_cas(
        &self,
        account_id: &str,
        declared: &[DeclaredApp],
        live: &[AccessApp],
    ) -> (Vec<String>, Vec<Json>, Vec<Json>) {
        let mut applied = Vec::new();
        let mut cas = Vec::new();
        let mut refused = Vec::new();
        for d in declared.iter().filter(|d| d.short_lived_ca) {
            let Some(app) = live.iter().find(|a| a.domain == d.domain) else {
                continue;
            };
            let key = match self.access.short_lived_ca(account_id, &app.id).await {
                Ok(Some(key)) => key,
                Ok(None) => match self.access.create_short_lived_ca(account_id, &app.id).await {
                    Ok(key) => {
                        applied.push(format!(
                            "Access short-lived certificate CA created on {}",
                            d.domain
                        ));
                        key
                    }
                    Err(e) => {
                        refused.push(refused_verdict(
                            &d.domain,
                            &format!("create short-lived certificate CA on {}", d.domain),
                            &e,
                        ));
                        continue;
                    }
                },
                Err(e) => {
                    refused.push(refused_verdict(
                        &d.domain,
                        &format!("read short-lived certificate CA on {}", d.domain),
                        &e,
                    ));
                    continue;
                }
            };
            cas.push(json!({"domain": d.domain, "public_key": key}));
        }
        (applied, cas, refused)
    }

    /// Execute one released zone write.
    ///
    /// The error is the account's own answer, verbatim: the caller
    /// records it as a `REFUSED` finding on the verdict, never as a
    /// failed firing (see the loop in `invoke`).
    async fn apply_zone(&self, zone_id: &str, plan: &ZoneApply) -> Result<String, String> {
        match plan {
            ZoneApply::Create { spec, delete_first } => {
                for id in delete_first {
                    self.zone.delete_record(zone_id, id).await?;
                }
                self.zone.create_record(zone_id, spec).await?;
                Ok(format!(
                    "{} {} created{}",
                    spec.name,
                    spec.record_type,
                    if delete_first.is_empty() {
                        String::new()
                    } else {
                        format!(" (replacing {} record(s) at that name)", delete_first.len())
                    }
                ))
            }
            ZoneApply::Update { record_id, spec } => {
                self.zone.update_record(zone_id, record_id, spec).await?;
                Ok(format!("{} {} corrected", spec.name, spec.record_type))
            }
        }
    }

    /// File or refresh the zone's alarm. Reads the open backlog-items
    /// first (a truncated page HOLDS, so a raise is never made blind).
    async fn raise_or_refresh(
        &self,
        rule: &str,
        zone: &str,
        observation_id: &str,
        r: &Reading,
    ) -> Result<&'static str, HandlerError> {
        let key = alarm_key(zone);
        // Only the packets carrying `estate_finding`, the one key the
        // dedup compares (backlog c5ac71de). Unfiltered, every open
        // backlog-item counted toward the 200-row page; open items passed
        // 200 in September 2026 (370 on 2026-09-26), so the first drift
        // after that would have been held as a truncated read.
        let listing = get_json(
            &self.client,
            &format!(
                "{}/api/jobs?kind=backlog-item&status=open&metadata_has=estate_finding&limit={DEDUP_PAGE}",
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
                    &alarm_refresh(observation_id, r),
                    rule,
                )
                .await?;
                tracing::info!(finding = %key, packet = %open_id, "dns.observe refreshed the open alarm");
                Ok("refreshed")
            }
            None => {
                let owner = owner_for_filing(self.owner.as_ref(), rule).await;
                post_json(
                    &self.client,
                    &format!("{}/api/jobs", self.base()),
                    &alarm_body(zone, observation_id, r, &owner),
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
        // A body that is not a job would fail the kind check below and
        // skip this ready step without a word (backlog f2eac973).
        let job = row_or_refuse(job, &format!("GET /api/jobs/{}", ev.job_id))
            .map_err(HandlerError::Downstream)?;
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
        // What the declarations reference, then each one resolved from
        // the system of record — before anything is read from
        // Cloudflare, so a declaration this observer cannot judge costs
        // no API call.
        let refs = self
            .comparator(&[zone.to_string(), "--list-references".to_string()], "")
            .await?;
        let mut args: Vec<String> = vec![zone.to_string(), "--json".to_string()];
        for reference in refs.lines().map(str::trim).filter(|l| !l.is_empty()) {
            args.extend(self.resolve_reference(reference).await?);
        }
        let access_decl = self.access_declaration(zone).await?;

        // What stands in front, first: the applications compared,
        // ABSENT ones created, and the account READ AGAIN so the gate
        // below judges what is there, not what was sent.
        let info = self
            .zone
            .zone_info(zone)
            .await
            .map_err(HandlerError::Downstream)?;
        let mut live_apps = self
            .access
            .access_apps(&info.account_id)
            .await
            .map_err(HandlerError::Downstream)?;
        let mut access_verdicts = compare_access(&access_decl.application, &live_apps);
        let (mut applied, created, refused) = self
            .apply_access(
                &info.account_id,
                &access_decl.application,
                &live_apps,
                &access_verdicts,
            )
            .await;
        if !applied.is_empty() {
            live_apps = self
                .access
                .access_apps(&info.account_id)
                .await
                .map_err(HandlerError::Downstream)?;
            access_verdicts = compare_access(&access_decl.application, &live_apps);
        }
        let (ca_applied, ssh_ca, ca_refused) = self
            .apply_short_lived_cas(&info.account_id, &access_decl.application, &live_apps)
            .await;
        applied.extend(ca_applied);
        // What the account refused rides beside what it holds: a
        // finding on the step, in the alarm, counted as hard.
        access_verdicts.extend(refused);
        access_verdicts.extend(ca_refused);
        // The tunnel interlock's facts: the newest converge packet's
        // ingress line and connector reading (fd75c641). Read once per
        // firing; a listing that cannot be read holds every tunnel-
        // interlocked record rather than releasing it blind.
        // The fallback is DELIBERATE and stated here, where it is chosen:
        // a failed read and an answer with no `data` array both yield no
        // facts, and no facts HOLDS every tunnel-interlocked record.
        let tunnel_facts = self
            .get(&format!(
                "/api/jobs?kind={CONVERGE_KIND}&status=closed&limit=1"
            ))
            .await
            .map_err(|e| e.to_string())
            .and_then(|listing| TunnelFacts::from_listing(&listing))
            .unwrap_or_else(|why| {
                tracing::warn!(%why, "dns.observe: no converge facts; every tunnel-interlocked record holds");
                TunnelFacts::default()
            });
        let gate_for = |v: &Json| -> Interlock {
            let hostname = v.get("name").and_then(Json::as_str).unwrap_or_default();
            match v.get("interlock").and_then(Json::as_str) {
                Some(INTERLOCK_ACCESS) => {
                    Interlock::Access(access_gate(hostname, &live_apps, &created))
                }
                Some(INTERLOCK_TUNNEL) => Interlock::Tunnel(tunnel_gate(hostname, &tunnel_facts)),
                _ => Interlock::None,
            }
        };

        // The zone, with the only token that can read it; then the
        // interlocked writes it releases; then, if anything was
        // written, the zone read and compared AGAIN so the packet
        // records the zone after the write.
        let comment = record_comment(zone, ev.job_id);
        let (records, mut comparison) = self.compare_zone(zone, &args).await?;
        let plans: Vec<ZoneApply> = comparison
            .verdicts
            .iter()
            .filter(|v| gate_for(v).allows())
            .filter_map(|v| plan_zone_apply(v, &records, &comment))
            .collect();
        // A zone write the account refuses is a finding, not a failed
        // firing — the same rule the Access half follows (bb604dc5).
        // Measured 2026-09-16 16:00Z (packet 95cf8740, item cfe12b0d):
        // the A record at boss. was deleted and the CNAME create was
        // refused on its comment's length; the handler failed, eight
        // redeliveries repeated the refusal, and the packet held a dead
        // letter and NOTHING it had read — not the zone it had just
        // changed, not the account's answer. The re-read below records
        // the zone as it now is (ABSENT at that name, which is hard),
        // and the answer rides that verdict as `refused`.
        let mut refused_zone: Vec<(String, String)> = Vec::new();
        for plan in &plans {
            match self.apply_zone(&info.zone_id, plan).await {
                Ok(line) => applied.push(line),
                Err(e) => refused_zone.push((plan.name().to_string(), e)),
            }
        }
        if !plans.is_empty() {
            comparison = self.compare_zone(zone, &args).await?.1;
        }
        comparison.verdicts = annotate_verdicts(&comparison.verdicts, gate_for);
        comparison.verdicts = annotate_refused(&comparison.verdicts, &refused_zone);
        let reading = Reading {
            zone: comparison,
            access: access_verdicts,
            applied,
            ssh_ca,
        };

        // The alarm FIRST (see the module doc for why), then the step.
        let alarm = if reading.hard() > 0 {
            self.raise_or_refresh(rule, zone, ev.job_id, &reading)
                .await?
        } else {
            "none"
        };

        complete_step(
            &self.client,
            self.base(),
            ev.job_id,
            ev.step_id,
            observe_fields(&reading),
            rule,
        )
        .await?;
        tracing::info!(
            zone,
            packet = ev.job_id,
            summary = %reading.summary(),
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
    use crate::handlers::credential_issuer::{AccessPolicy, ZoneInfo};
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

    fn reading(c: Comparison) -> Reading {
        Reading {
            zone: c,
            access: vec![],
            applied: vec![],
            ssh_ca: vec![],
        }
    }

    #[test]
    fn the_observe_body_completes_with_the_verdicts_and_forks_on_hard() {
        let clean = reading(comparison(
            0,
            json!([{"record": "boss.algedonic.dev CNAME", "verdict": "MATCH"}]),
        ));
        let b = observe_fields(&clean);
        assert_eq!(b["result"], "match");
        let mut keys: Vec<&str> = b.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "access", "applied", "counts", "result", "ssh_ca", "summary", "verdicts"
            ],
            "its own fields only — the merge door keeps the step's (e39a9d2a)"
        );
        assert_eq!(b["verdicts"][0]["verdict"], "MATCH");
        assert!(
            b["summary"]
                .as_str()
                .unwrap()
                .starts_with(&clean.zone.summary)
        );
        assert_eq!(b["counts"]["zone"]["MATCH"], 1);

        let drifted = reading(comparison(
            1,
            json!([{"record": "boss.algedonic.dev CNAME", "verdict": "DRIFT"}]),
        ));
        assert_eq!(observe_fields(&drifted)["result"], "findings");

        // HELD is paperwork: the zone disagrees with the declaration
        // by design until the interlock releases, and that is not a
        // finding.
        let held = reading(comparison(
            1,
            json!([{"record": "boss.algedonic.dev CNAME", "verdict": "HELD", "held": "flip held — Access app absent"}]),
        ));
        assert_eq!(held.hard(), 0);
        assert_eq!(observe_fields(&held)["result"], "match");
        assert_eq!(observe_fields(&held)["counts"]["zone"]["HELD"], 1);

        // An Access finding is a finding.
        let mut access_drift = reading(comparison(0, json!([])));
        access_drift.access = vec![json!({"application": "x", "verdict": "DRIFT"})];
        assert_eq!(access_drift.hard(), 1);
        assert_eq!(observe_fields(&access_drift)["result"], "findings");
    }

    #[test]
    fn the_alarm_carries_only_the_hard_verdicts_and_the_estate_key() {
        let mut r = reading(comparison(
            2,
            json!([
                {"record": "boss.algedonic.dev CNAME", "verdict": "DRIFT"},
                {"record": "playground.algedonic.dev CNAME", "verdict": "MATCH"},
                {"record": "id.algedonic.dev A", "verdict": "UNDECLARED"},
                {"record": "www.algedonic.dev CNAME", "verdict": "ABSENT"},
            ]),
        ));
        r.access = vec![
            json!({"application": "playground.algedonic.dev", "verdict": "DRIFT"}),
            json!({"application": "boss.algedonic.dev", "verdict": "MATCH"}),
        ];
        let b = alarm_body("algedonic.dev", "obs-1", &r, "emp-owner");
        assert_eq!(b["kind"], "backlog-item");
        assert_eq!(b["owner_id"], "emp-owner", "the owner is the one handed in");
        assert_eq!(b["priority"], "urgent");
        assert_eq!(b["metadata"]["estate_finding"], "dns_drift:algedonic.dev");
        assert_eq!(b["metadata"]["area"], "estate");
        assert_eq!(b["metadata"]["latest_observation"], "obs-1");
        let findings = b["metadata"]["findings"].as_array().unwrap();
        assert_eq!(
            findings.len(),
            3,
            "UNDECLARED and MATCH are not alarm material; an Access DRIFT is"
        );
        assert_eq!(findings[2]["scope"], "access");
        assert!(b["title"].as_str().unwrap().contains("3 finding(s)"));
        assert!(b["metadata"]["detail"].as_str().unwrap().contains("obs-1"));

        let refresh = alarm_refresh("obs-2", &r);
        assert_eq!(refresh["latest_observation"], "obs-2");
        assert_eq!(refresh["findings"].as_array().unwrap().len(), 3);
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

    // ----- the Access declaration and comparison -----

    const DAVID: &str = "david@algedonic.dev";

    fn declared_app(domain: &str, policy: &str, emails: &[&str]) -> DeclaredApp {
        DeclaredApp {
            name: domain.to_string(),
            domain: domain.to_string(),
            app_type: "self_hosted".into(),
            session_duration: "24h".into(),
            why: "test".into(),
            short_lived_ca: false,
            policy: vec![DeclaredPolicy {
                name: policy.into(),
                decision: "allow".into(),
                include: DeclaredInclude {
                    emails: emails.iter().map(|e| e.to_string()).collect(),
                    everyone: false,
                    unmanaged: false,
                },
            }],
        }
    }

    fn email_rules(emails: &[&str]) -> Vec<Json> {
        emails
            .iter()
            .map(|e| json!({"email": {"email": e}}))
            .collect()
    }

    fn live_app(domain: &str, policies: Vec<AccessPolicy>) -> AccessApp {
        AccessApp {
            id: format!("app-{domain}"),
            name: format!("{domain} (dashboard name)"),
            domain: domain.to_string(),
            app_type: "self_hosted".into(),
            session_duration: "24h".into(),
            policies,
        }
    }

    /// The dashboard's policies sit at precedence 1 — the shape the
    /// account had on 2026-09-16 when a second application's first
    /// policy at 1 was refused.
    fn allow(name: &str, emails: &[&str]) -> AccessPolicy {
        AccessPolicy {
            id: format!("pol-{name}"),
            name: name.to_string(),
            decision: "allow".into(),
            include: email_rules(emails),
            precedence: 1,
        }
    }

    /// An SSH application is the SAME vocabulary as a self_hosted one
    /// (design 5fc71f03; backlog e4cedb46 asked whether the handler
    /// knows only HTTP applications). `type` is a value carried
    /// verbatim from the declaration to the comparison and to the
    /// create body — never matched against a list of known kinds — so
    /// the dev door needed no handler change, and this test is what
    /// keeps that true when someone reaches for an enum.
    #[test]
    fn an_ssh_application_is_declared_compared_and_created_as_one() {
        let text = std::fs::read_to_string(
            boss_testing::repo_root().join("infra/cluster/dns/access.toml"),
        )
        .expect("access.toml ships beside the zone file");
        let dec = parse_access_declaration(&text, "algedonic.dev").expect("parses for the zone");
        let dev = dec
            .application
            .iter()
            .find(|a| a.domain == "dev.algedonic.dev")
            .expect("the dev workspace door is declared");
        assert_eq!(dev.app_type, "ssh");
        assert!(
            dev.short_lived_ca,
            "the ssh door declares its CA, or cloudflared is refused 'bad ca application'"
        );

        // ABSENT is what the first observation after this lands reads,
        // and what makes the handler create it — with the declared
        // type, not a default.
        let absent = compare_access(std::slice::from_ref(dev), &[]);
        assert_eq!(absent[0]["verdict"], json!("ABSENT"));
        assert_eq!(absent[0]["declared"]["type"], json!("ssh"));

        // And once the account holds it, the comparison MATCHes: an
        // ssh application does not read as permanent DRIFT against a
        // vocabulary that only knew self_hosted.
        let live = AccessApp {
            app_type: "ssh".into(),
            ..live_app("dev.algedonic.dev", vec![allow("operators", &[DAVID])])
        };
        let matched = compare_access(std::slice::from_ref(dev), &[live]);
        assert_eq!(matched[0]["verdict"], json!("MATCH"), "{matched:?}");
    }

    #[test]
    fn the_shipped_access_declaration_parses_and_fronts_both_doors() {
        let text = std::fs::read_to_string(
            boss_testing::repo_root().join("infra/cluster/dns/access.toml"),
        )
        .expect("access.toml ships beside the zone file");
        let dec = parse_access_declaration(&text, "algedonic.dev").expect("parses for the zone");
        let boss = dec
            .application
            .iter()
            .find(|a| a.domain == "boss.algedonic.dev")
            .expect("boss. is declared");
        assert_eq!(boss.app_type, "self_hosted");
        assert!(
            boss.policy
                .iter()
                .any(|p| p.decision == "allow" && p.include.emails == [DAVID.to_string()]),
            "boss. allows the operator by e-mail: {:?}",
            boss.policy
        );
        // The playground as READ on 2026-09-16 (08b8b396 q1/q2): the
        // dashboard's onboarding policy beside `visitors`, and the
        // callback bypass for everyone on the /auth path.
        let pg = dec
            .application
            .iter()
            .find(|a| a.domain == "playground.algedonic.dev")
            .expect("the playground application is declared");
        assert_eq!(pg.policy.len(), 2, "{:?}", pg.policy);
        let onboarding = pg
            .policy
            .iter()
            .find(|p| {
                p.name
                    .starts_with("Allow emails policy created by onboarding")
            })
            .expect("the onboarding policy is declared as the account holds it");
        // Its members are the dashboard's, not the tree's (67e754cc,
        // 2026-09-18): the four visitors admitted at onboarding are
        // individuals' personal addresses, and this file is public.
        assert!(onboarding.include.unmanaged, "{onboarding:?}");
        assert!(onboarding.include.emails.is_empty());
        // No declared address anywhere in the file belongs to a person
        // outside the company — the whole point of `unmanaged`.
        for app in &dec.application {
            for policy in &app.policy {
                for e in &policy.include.emails {
                    assert!(
                        e.ends_with("@algedonic.dev"),
                        "a personal address is declared in a public file: {e} ({}/{})",
                        app.domain,
                        policy.name
                    );
                }
            }
        }
        let auth = dec
            .application
            .iter()
            .find(|a| a.domain == "playground.algedonic.dev/auth")
            .expect("the callback bypass is declared");
        assert_eq!(auth.policy.len(), 1);
        assert_eq!(auth.policy[0].decision, "bypass");
        assert!(auth.policy[0].include.everyone);
        assert_eq!(
            auth.policy[0].include.rules(),
            vec![json!({"everyone": {}})]
        );
        assert!(
            include_rules(&[json!({"everyone": {}})]).as_ref()
                == Some(&auth.policy[0].include.canonical()),
            "a live everyone rule reads equal to the declared one"
        );
        assert!(
            include_rules(&[json!({"group": {"id": "x"}})]).is_none(),
            "a rule kind the declaration has no word for is not silently equal"
        );
        // The OIDC redirect the gateway will build from the flipped
        // BOSS_PUBLIC_URL is declared registered, with its provenance.
        let boss_redirect = dec
            .oidc_redirect
            .iter()
            .find(|r| r.hostname == "boss.algedonic.dev")
            .expect("boss. redirect declared");
        assert_eq!(
            boss_redirect.redirect,
            "https://boss.algedonic.dev/api/auth/oidc/callback"
        );
        assert!(boss_redirect.registered);
        assert!(boss_redirect.measured.contains("2026-08-12"));
        assert!(
            parse_access_declaration(&text, "example.org").is_err(),
            "another zone's observation must not read this declaration"
        );
    }

    #[test]
    fn a_declaration_naming_a_domain_twice_or_a_policy_for_nobody_is_refused() {
        let twice = r#"
account_zone = "z.dev"
[[application]]
name = "a"
domain = "a.z.dev"
type = "self_hosted"
session_duration = "24h"
why = "x"
[[application.policy]]
name = "p"
decision = "allow"
include.emails = ["a@z.dev"]
[[application]]
name = "b"
domain = "a.z.dev"
type = "self_hosted"
session_duration = "24h"
why = "x"
"#;
        let err = parse_access_declaration(twice, "z.dev").unwrap_err();
        assert!(err.contains("a.z.dev") && err.contains("twice"), "{err}");
        let nobody = r#"
account_zone = "z.dev"
[[application]]
name = "a"
domain = "a.z.dev"
type = "self_hosted"
session_duration = "24h"
why = "x"
[[application.policy]]
name = "p"
decision = "allow"
include.emails = []
"#;
        let err = parse_access_declaration(nobody, "z.dev").unwrap_err();
        assert!(err.contains("nobody"), "{err}");
        // `unmanaged` is a declaration that somebody is included and
        // the dashboard holds who — it passes the nobody check alone
        // and is refused beside an e-mail list (which of the two would
        // the comparison read?).
        let unmanaged = nobody.replace("include.emails = []", "include.unmanaged = true");
        let dec =
            parse_access_declaration(&unmanaged, "z.dev").expect("unmanaged includes somebody");
        assert!(dec.application[0].policy[0].include.unmanaged);
        assert!(dec.application[0].policy[0].include.rules().is_empty());
        let both = nobody.replace(
            "include.emails = []",
            "include.emails = [\"a@z.dev\"]\ninclude.unmanaged = true",
        );
        let err = parse_access_declaration(&both, "z.dev").unwrap_err();
        assert!(err.contains("unmanaged"), "{err}");
        let elsewhere = r#"
account_zone = "z.dev"
[[application]]
name = "a"
domain = "a.other.dev"
type = "self_hosted"
session_duration = "24h"
why = "x"
"#;
        let err = parse_access_declaration(elsewhere, "z.dev").unwrap_err();
        assert!(err.contains("a.other.dev"), "{err}");
    }
    /// CLOSED KEY SET (backlog 1c1591fa, 2026-09-19). access.toml is
    /// authored tree data, the way the dispatcher's rule registry is,
    /// and an open key set drops a typo in silence — worse here than
    /// in most places, because the redirect check (since 63a92827 the
    /// integration test beside this crate, before that an awk lint)
    /// would then report green over a redirect the observer never saw:
    /// a check passing on absence. Every table in the declaration is closed, so a
    /// misspelled key is refused where the file is read, by name.
    #[test]
    fn a_misspelled_key_in_the_access_declaration_is_refused_by_name() {
        let base = r#"
account_zone = "z.dev"
[[application]]
name = "a"
domain = "a.z.dev"
type = "self_hosted"
session_duration = "24h"
why = "x"
[[application.policy]]
name = "p"
decision = "allow"
include.emails = ["a@z.dev"]
[[oidc_redirect]]
hostname = "a.z.dev"
redirect = "https://a.z.dev/api/auth/oidc/callback"
registered = true
measured = "2026-09-19: read from the IdP"
"#;
        parse_access_declaration(base, "z.dev").expect("the base declaration is well formed");
        // One typo per table: the top-level document, an application,
        // a policy, a policy's include, and a redirect — the five
        // shapes access.toml is written in.
        for (typo, line, after) in [
            (
                "oidc_redirects",
                "oidc_redirects = []",
                "account_zone = \"z.dev\"",
            ),
            ("domian", "domian = \"a.z.dev\"", "domain = \"a.z.dev\""),
            ("decission", "decission = \"allow\"", "decision = \"allow\""),
            (
                "email",
                "include.email = [\"a@z.dev\"]",
                "include.emails = [\"a@z.dev\"]",
            ),
            ("registerd", "registerd = true", "registered = true"),
        ] {
            let text = base.replace(after, &format!("{after}\n{line}"));
            let err = parse_access_declaration(&text, "z.dev")
                .expect_err("a key the parser does not read is refused, never dropped");
            assert!(
                err.contains(typo) && err.contains(ACCESS_DECLARATION),
                "the refusal names the misspelled key and the file: {err}"
            );
        }
    }

    /// A TABLE-HEADER typo — `[[oidc_redirct]]` — is the case the key
    /// set above says nothing about on its own: an unknown TABLE is not
    /// an unknown key in a known table. It is refused only because the
    /// DOCUMENT's own table is closed too, so the misspelled header is
    /// an unknown key of the root. Asserted separately (backlog
    /// 63a92827) because it is the case that reads as a smaller truth
    /// rather than an error: one fewer redirect, not a refusal.
    #[test]
    fn a_misspelled_table_header_is_refused_rather_than_read_as_one_fewer() {
        let text = r#"
account_zone = "z.dev"
[[oidc_redirect]]
hostname = "a.z.dev"
redirect = "https://a.z.dev/api/auth/oidc/callback"
registered = true
measured = "2026-09-20: read from the IdP"
[[oidc_redirct]]
hostname = "b.z.dev"
redirect = "https://b.z.dev/api/auth/oidc/callback"
registered = true
measured = "2026-09-20: read from the IdP"
"#;
        let err = parse_access_declaration(text, "z.dev")
            .expect_err("a misspelled table header is refused, never read as one fewer redirect");
        assert!(
            err.contains("oidc_redirct") && err.contains(ACCESS_DECLARATION),
            "the refusal names the misspelled header and the file: {err}"
        );
    }

    #[test]
    fn access_is_compared_by_domain_in_the_four_verdicts() {
        let declared = vec![
            declared_app("boss.algedonic.dev", "operators", &[DAVID]),
            declared_app("playground.algedonic.dev", "visitors", &[DAVID]),
            declared_app("www.algedonic.dev", "everyone", &[DAVID]),
        ];
        let live = vec![
            live_app("boss.algedonic.dev", vec![allow("operators", &[DAVID])]),
            // the dashboard's playground app: another policy name and a
            // rule kind the declaration has no vocabulary for
            live_app(
                "playground.algedonic.dev",
                vec![AccessPolicy {
                    id: "pol-x".into(),
                    name: "Allow visitors".into(),
                    decision: "allow".into(),
                    include: vec![json!({"everyone": {}})],
                    precedence: 1,
                }],
            ),
            live_app("other.algedonic.dev", vec![]),
        ];
        let v = compare_access(&declared, &live);
        let by = |d: &str| {
            v.iter()
                .find(|e| e["application"] == d)
                .unwrap_or_else(|| panic!("no verdict for {d}: {v:?}"))
                .clone()
        };
        assert_eq!(by("boss.algedonic.dev")["verdict"], "MATCH");
        let pg = by("playground.algedonic.dev");
        assert_eq!(pg["verdict"], "DRIFT");
        assert_eq!(
            pg["live"]["policies"][0]["include"][0]["everyone"],
            json!({}),
            "the live rule is printed raw, so the next car learns the vocabulary: {pg}"
        );
        assert_eq!(pg["policies_absent"], json!(["visitors"]));
        assert_eq!(by("www.algedonic.dev")["verdict"], "ABSENT");
        let other = by("other.algedonic.dev");
        assert_eq!(other["verdict"], "UNDECLARED");
        assert_eq!(v.len(), 4);
    }

    #[test]
    fn a_policy_with_the_same_name_and_different_emails_or_decision_is_drift() {
        let declared = vec![declared_app("boss.algedonic.dev", "operators", &[DAVID])];
        let same = vec![live_app(
            "boss.algedonic.dev",
            vec![allow("operators", &[DAVID])],
        )];
        assert_eq!(compare_access(&declared, &same)[0]["verdict"], "MATCH");
        let wider = vec![live_app(
            "boss.algedonic.dev",
            vec![allow("operators", &[DAVID, "someone@else.dev"])],
        )];
        assert_eq!(compare_access(&declared, &wider)[0]["verdict"], "DRIFT");
        let mut deny = allow("operators", &[DAVID]);
        deny.decision = "deny".into();
        let denied = vec![live_app("boss.algedonic.dev", vec![deny])];
        assert_eq!(compare_access(&declared, &denied)[0]["verdict"], "DRIFT");
        let extra = vec![live_app(
            "boss.algedonic.dev",
            vec![
                allow("operators", &[DAVID]),
                allow("also everyone", &["x@y.dev"]),
            ],
        )];
        assert_eq!(
            compare_access(&declared, &extra)[0]["verdict"],
            "DRIFT",
            "an extra live policy widens who gets in; it is drift, printed"
        );
        let session = {
            let mut a = live_app("boss.algedonic.dev", vec![allow("operators", &[DAVID])]);
            a.session_duration = "720h".into();
            vec![a]
        };
        assert_eq!(compare_access(&declared, &session)[0]["verdict"], "DRIFT");
    }

    /// An `unmanaged` policy is compared by name and decision only: its
    /// members are the dashboard's, so any include set reads MATCH, and
    /// a group rule the declaration has no word for is not drift here.
    /// When the policy is absent the application reads DRIFT, but the
    /// policy is NOT in `policies_absent` — the one list the handler
    /// creates from — because a policy created with no include matches
    /// nobody; it is named under `policies_unmanaged_absent` instead.
    #[test]
    fn an_unmanaged_policy_is_matched_by_name_and_decision_and_never_created() {
        let mut d = declared_app("playground.algedonic.dev", "onboarding", &[]);
        d.policy[0].include.unmanaged = true;
        let declared = vec![d];
        let four = vec![live_app(
            "playground.algedonic.dev",
            vec![allow(
                "onboarding",
                &["a@x.dev", "b@y.dev", "c@z.dev", "d@w.dev"],
            )],
        )];
        let v = compare_access(&declared, &four);
        assert_eq!(v[0]["verdict"], "MATCH", "{v:?}");
        let mut group = allow("onboarding", &[]);
        group.include = vec![json!({"group": {"id": "g1"}})];
        let grouped = vec![live_app("playground.algedonic.dev", vec![group])];
        assert_eq!(compare_access(&declared, &grouped)[0]["verdict"], "MATCH");
        let mut deny = allow("onboarding", &["a@x.dev"]);
        deny.decision = "deny".into();
        let denied = vec![live_app("playground.algedonic.dev", vec![deny])];
        assert_eq!(compare_access(&declared, &denied)[0]["verdict"], "DRIFT");
        let none = vec![live_app("playground.algedonic.dev", vec![])];
        let v = compare_access(&declared, &none);
        assert_eq!(v[0]["verdict"], "DRIFT");
        assert!(v[0].get("policies_absent").is_none(), "{v:?}");
        assert_eq!(v[0]["policies_unmanaged_absent"], json!(["onboarding"]));
        assert_eq!(
            v[0]["declared"]["policies"][0]["unmanaged"],
            json!(true),
            "the verdict says why no e-mails are listed"
        );
    }

    #[test]
    fn the_gate_reads_present_created_absent_or_no_allow_policy() {
        let boss = "boss.algedonic.dev";
        assert_eq!(access_gate(boss, &[], &[]), AccessGate::Absent);
        let no_policy = vec![live_app(boss, vec![])];
        assert_eq!(
            access_gate(boss, &no_policy, &[]),
            AccessGate::NoAllowPolicy
        );
        let mut deny = allow("operators", &[DAVID]);
        deny.decision = "deny".into();
        let denied = vec![live_app(boss, vec![deny])];
        assert_eq!(access_gate(boss, &denied, &[]), AccessGate::NoAllowPolicy);
        let present = vec![live_app(boss, vec![allow("operators", &[DAVID])])];
        assert_eq!(access_gate(boss, &present, &[]), AccessGate::Present);
        assert_eq!(
            access_gate(boss, &present, &[boss.to_string()]),
            AccessGate::Created
        );
        assert!(AccessGate::Present.allows() && AccessGate::Created.allows());
        assert!(!AccessGate::Absent.allows() && !AccessGate::NoAllowPolicy.allows());
        assert_eq!(
            AccessGate::Absent.held_reason(),
            Some("flip held — Access app absent")
        );
    }

    fn zone_verdict(verdict: &str, interlock: bool) -> Json {
        let mut v = json!({
            "record": "boss.algedonic.dev CNAME",
            "name": "boss.algedonic.dev",
            "type": "CNAME",
            "verdict": verdict,
            "declared": {"content": "t.cfargotunnel.com", "proxied": true, "ttl": 1, "target": "tunnel:c"},
            "live": {"content": "old.cfargotunnel.com", "proxied": true, "ttl": 1},
        });
        if interlock {
            v["interlock"] = json!("access");
        }
        v
    }

    /// The comment the first flip sent was 114 characters; Cloudflare's
    /// ceiling is 100, and the refusal came AFTER the A record was
    /// deleted (2026-09-16 16:00Z, packet 95cf8740: boss. NXDOMAIN).
    #[test]
    fn the_record_comment_fits_cloudflares_ceiling_for_any_packet_id() {
        let c = record_comment("algedonic.dev", "95cf8740-c522-40e8-9f6d-8f166134507d");
        assert_eq!(
            c,
            "declared in infra/cluster/dns/algedonic.dev.toml; dns.observe 95cf8740"
        );
        assert!(c.chars().count() <= RECORD_COMMENT_MAX, "{}", c.len());
        let old = format!(
            "declared in infra/cluster/dns/algedonic.dev.toml; applied by dns.observe (observation packet {})",
            "95cf8740-c522-40e8-9f6d-8f166134507d"
        );
        assert!(old.len() > RECORD_COMMENT_MAX, "the shape that was refused");
        let long_zone = "a".repeat(120);
        assert_eq!(
            record_comment(&long_zone, "95cf8740").chars().count(),
            RECORD_COMMENT_MAX
        );
    }

    #[test]
    fn the_plan_replaces_what_conflicts_with_a_cname_and_corrects_drift_in_place() {
        let rows = vec![
            json!({"id": "rec-a", "name": "boss.algedonic.dev", "type": "A", "content": "10.20.0.33"}),
            json!({"id": "rec-txt", "name": "boss.algedonic.dev", "type": "TXT", "content": "v=spf1"}),
            json!({"id": "rec-pg", "name": "playground.algedonic.dev", "type": "CNAME", "content": "t.cfargotunnel.com"}),
        ];
        let plan = plan_zone_apply(&zone_verdict("ABSENT", true), &rows, "why").unwrap();
        match plan {
            ZoneApply::Create { spec, delete_first } => {
                assert_eq!(spec.record_type, "CNAME");
                assert_eq!(spec.content, "t.cfargotunnel.com");
                assert!(spec.proxied);
                assert_eq!(spec.ttl, 1);
                assert_eq!(spec.comment, "why");
                assert_eq!(
                    delete_first,
                    vec!["rec-a", "rec-txt"],
                    "every record at the name goes: a CNAME shares its name with nothing"
                );
            }
            other => panic!("{other:?}"),
        }
        let drifted = vec![json!({
            "id": "rec-old", "name": "boss.algedonic.dev", "type": "CNAME", "content": "old.cfargotunnel.com"
        })];
        let plan = plan_zone_apply(&zone_verdict("DRIFT", true), &drifted, "why").unwrap();
        assert_eq!(
            plan,
            ZoneApply::Update {
                record_id: "rec-old".into(),
                spec: ZoneRecordSpec {
                    name: "boss.algedonic.dev".into(),
                    record_type: "CNAME".into(),
                    content: "t.cfargotunnel.com".into(),
                    proxied: true,
                    ttl: 1,
                    comment: "why".into(),
                }
            }
        );
        assert!(plan_zone_apply(&zone_verdict("MATCH", true), &rows, "why").is_none());
        assert!(
            plan_zone_apply(&zone_verdict("ABSENT", false), &rows, "why").is_none(),
            "a record without an interlock is never applied by the observer"
        );
    }

    #[test]
    fn annotation_stamps_the_gate_and_holds_what_it_did_not_release() {
        let verdicts = vec![
            zone_verdict("ABSENT", true),
            json!({"record": "playground.algedonic.dev CNAME", "name": "playground.algedonic.dev", "verdict": "ABSENT"}),
        ];
        // The gate reads the verdict's own interlock: a record without
        // one is `Interlock::None` whatever the account says.
        let access = |g: AccessGate| {
            move |v: &Json| {
                if v.get("interlock").is_some() {
                    Interlock::Access(g)
                } else {
                    Interlock::None
                }
            }
        };
        let held = annotate_verdicts(&verdicts, access(AccessGate::Absent));
        assert_eq!(held[0]["verdict"], "HELD");
        assert_eq!(held[0]["access"], "absent");
        assert_eq!(held[0]["held"], "flip held — Access app absent");
        assert_eq!(
            held[1]["verdict"], "ABSENT",
            "no interlock: untouched, still a finding"
        );
        assert!(held[1].get("access").is_none());
        let released = annotate_verdicts(&verdicts, access(AccessGate::Created));
        assert_eq!(
            released[0]["verdict"], "ABSENT",
            "released but still absent after the apply = a real finding"
        );
        assert_eq!(released[0]["access"], "created");
        let matched =
            annotate_verdicts(&[zone_verdict("MATCH", true)], access(AccessGate::Present));
        assert_eq!(matched[0]["verdict"], "MATCH");
        assert_eq!(matched[0]["access"], "present");
    }

    /// The tunnel interlock (fd75c641): released only when the newest
    /// converge packet's ingress line routes the hostname AND the
    /// connector read connected; a prefix match is not a route.
    #[test]
    fn the_tunnel_gate_reads_the_converge_packets_ingress_and_connector() {
        let listing = json!({"data": [{
            "kind": "maintenance-cluster-converge",
            "steps": [
                {"spec_slug": "due", "status": "completed", "metadata": {}},
                {"spec_slug": "run", "status": "completed", "metadata": {
                    "cloudflared": "connected",
                    "tunnel_ingress": "boss.algedonic.dev → boss; playground.algedonic.dev → boss (boss-playground skipped: secrets absent); id.algedonic.dev → https://10.20.0.31:443 (origin)"
                }}
            ]
        }]});
        let facts = TunnelFacts::from_listing(&listing).expect("a listing");
        assert_eq!(tunnel_gate("id.algedonic.dev", &facts), TunnelGate::Routed);
        assert_eq!(
            tunnel_gate("boss.algedonic.dev", &facts),
            TunnelGate::Routed
        );
        assert_eq!(
            tunnel_gate("d.algedonic.dev", &facts),
            TunnelGate::NotRouted,
            "a suffix of a routed name is not routed"
        );
        assert_eq!(
            tunnel_gate("www.algedonic.dev", &facts),
            TunnelGate::NotRouted
        );
        let dark = TunnelFacts {
            connector: "not ready (0/2)".into(),
            ..facts.clone()
        };
        assert_eq!(
            tunnel_gate("id.algedonic.dev", &dark),
            TunnelGate::NotConnected
        );
        assert_eq!(
            TunnelFacts::from_listing(&json!({"data": []})),
            Ok(TunnelFacts::default()),
            "no converge packet: no facts, and every tunnel gate holds"
        );
        assert!(
            TunnelFacts::from_listing(&json!({"total": 1})).is_err(),
            "no `data` array is no answer — the caller states the hold (d4698bc2)"
        );
        assert_eq!(
            tunnel_gate("id.algedonic.dev", &TunnelFacts::default()),
            TunnelGate::NotRouted
        );

        // Through the annotation: a tunnel-interlocked ABSENT is HELD
        // with the tunnel's phrase and its own field, never `access`.
        let v = json!({"record": "id.algedonic.dev CNAME", "name": "id.algedonic.dev", "verdict": "DRIFT", "interlock": "tunnel"});
        let held = annotate_verdicts(std::slice::from_ref(&v), |_| {
            Interlock::Tunnel(TunnelGate::NotRouted)
        });
        assert_eq!(held[0]["verdict"], "HELD");
        assert_eq!(held[0]["tunnel"], "not-routed");
        assert!(held[0].get("access").is_none());
        assert!(
            held[0]["held"].as_str().unwrap().contains("does not route"),
            "{}",
            held[0]
        );
        let released = annotate_verdicts(&[v], |_| Interlock::Tunnel(TunnelGate::Routed));
        assert_eq!(
            released[0]["verdict"], "DRIFT",
            "released: the apply is what corrects it"
        );
        assert_eq!(released[0]["tunnel"], "routed");
        assert!(
            plan_zone_apply(
                &json!({"name": "id.algedonic.dev", "type": "CNAME", "verdict": "DRIFT", "interlock": "tunnel",
                        "declared": {"content": "d8a8.cfargotunnel.com", "proxied": true, "ttl": 1},
                        "live": {"content": "8bb0.cfargotunnel.com", "proxied": true, "ttl": 1}}),
                &[json!({"id": "rec-1", "name": "id.algedonic.dev", "type": "CNAME", "content": "8bb0.cfargotunnel.com"})],
                "why"
            )
            .is_some(),
            "a tunnel-interlocked DRIFT plans the in-place correction"
        );
    }

    // ----- in-memory fakes -----

    /// A zone that remembers its records and applies writes to them, so
    /// the re-read after an apply sees what was written.
    struct FakeZone {
        records: Mutex<Result<Vec<Json>, String>>,
        reads: Mutex<usize>,
        writes: Mutex<Vec<String>>,
        /// The account's answer to every create, when it refuses them
        /// — the 2026-09-16 16:00Z shape (a delete already done, the
        /// create refused on the record's own body).
        refuse_creates: Option<String>,
    }

    /// Cloudflare's answer to the first boss. CNAME create, verbatim
    /// (2026-09-16 15:58Z, code 9313).
    const COMMENT_TOO_LONG: &str = "POST /zones/zone-1/dns_records returned 400 Bad Request: DNS record comment exceeds the maximum length of 100 characters.";

    impl FakeZone {
        fn with(records: Vec<Json>) -> Arc<Self> {
            Arc::new(Self {
                records: Mutex::new(Ok(records)),
                reads: Mutex::new(0),
                writes: Mutex::new(vec![]),
                refuse_creates: None,
            })
        }
        fn refusing_creates(records: Vec<Json>, why: &str) -> Arc<Self> {
            Arc::new(Self {
                records: Mutex::new(Ok(records)),
                reads: Mutex::new(0),
                writes: Mutex::new(vec![]),
                refuse_creates: Some(why.to_string()),
            })
        }
        fn dark(msg: &str) -> Arc<Self> {
            Arc::new(Self {
                records: Mutex::new(Err(msg.to_string())),
                reads: Mutex::new(0),
                writes: Mutex::new(vec![]),
                refuse_creates: None,
            })
        }
        fn writes(&self) -> Vec<String> {
            self.writes.lock().unwrap().clone()
        }
        fn live(&self) -> Vec<Json> {
            self.records.lock().unwrap().clone().unwrap()
        }
    }

    #[async_trait]
    impl ZoneRecords for FakeZone {
        async fn zone_records(&self, zone_name: &str) -> Result<Vec<Json>, String> {
            assert_eq!(zone_name, "algedonic.dev");
            *self.reads.lock().unwrap() += 1;
            self.records.lock().unwrap().clone()
        }
        async fn zone_info(&self, zone_name: &str) -> Result<ZoneInfo, String> {
            assert_eq!(zone_name, "algedonic.dev");
            Ok(ZoneInfo {
                zone_id: "zone-1".into(),
                account_id: "acct-1".into(),
            })
        }
        async fn create_record(&self, zone_id: &str, spec: &ZoneRecordSpec) -> Result<(), String> {
            assert_eq!(zone_id, "zone-1");
            self.writes.lock().unwrap().push(format!(
                "create {} {} {} proxied={} ttl={}",
                spec.name, spec.record_type, spec.content, spec.proxied, spec.ttl
            ));
            if let Some(why) = &self.refuse_creates {
                return Err(why.clone());
            }
            if let Ok(rows) = &mut *self.records.lock().unwrap() {
                rows.push(json!({
                    "id": format!("rec-new-{}", spec.record_type), "name": spec.name,
                    "type": spec.record_type, "content": spec.content,
                    "proxied": spec.proxied, "ttl": spec.ttl, "comment": spec.comment,
                }));
            }
            Ok(())
        }
        async fn update_record(
            &self,
            zone_id: &str,
            record_id: &str,
            spec: &ZoneRecordSpec,
        ) -> Result<(), String> {
            assert_eq!(zone_id, "zone-1");
            self.writes.lock().unwrap().push(format!(
                "update {record_id} {} proxied={}",
                spec.content, spec.proxied
            ));
            if let Ok(rows) = &mut *self.records.lock().unwrap()
                && let Some(row) = rows.iter_mut().find(|r| r["id"] == record_id)
            {
                row["content"] = json!(spec.content);
                row["proxied"] = json!(spec.proxied);
                row["ttl"] = json!(spec.ttl);
            }
            Ok(())
        }
        async fn delete_record(&self, zone_id: &str, record_id: &str) -> Result<(), String> {
            assert_eq!(zone_id, "zone-1");
            self.writes
                .lock()
                .unwrap()
                .push(format!("delete {record_id}"));
            if let Ok(rows) = &mut *self.records.lock().unwrap() {
                rows.retain(|r| r["id"] != record_id);
            }
            Ok(())
        }
    }

    /// The account's answer to a policy whose precedence another
    /// policy in the account already holds — verbatim from the API,
    /// 2026-09-16 (code 12130).
    const PRECEDENCE_TAKEN: &str = "returned 400 Bad Request: access.api.error.invalid_request: policy precedences must be unique";

    /// An account that remembers its applications and honours creates —
    /// and refuses a policy whose precedence the account already holds,
    /// across applications, the way the real one did on 2026-09-16.
    /// `refuse_policies` refuses every policy create with that text,
    /// for the reading where the account will not take a write at all.
    struct FakeAccess {
        apps: Mutex<Result<Vec<AccessApp>, String>>,
        reads: Mutex<usize>,
        writes: Mutex<Vec<String>>,
        refuse_policies: Option<String>,
        /// Short-lived-certificate CA public keys, by application id.
        /// An account built `with` its applications holds a CA for each
        /// (the dev door's, as the account reads once the observer has
        /// run); `without_cas` holds none.
        cas: Mutex<HashMap<String, String>>,
        refuse_cas: Option<String>,
    }

    impl FakeAccess {
        fn ca_key(app_id: &str) -> String {
            format!("ecdsa-sha2-nistp256 CA-OF-{app_id}")
        }
        fn build(
            apps: Result<Vec<AccessApp>, String>,
            refuse_policies: Option<String>,
            cas: HashMap<String, String>,
            refuse_cas: Option<String>,
        ) -> Arc<Self> {
            Arc::new(Self {
                apps: Mutex::new(apps),
                reads: Mutex::new(0),
                writes: Mutex::new(vec![]),
                refuse_policies,
                cas: Mutex::new(cas),
                refuse_cas,
            })
        }
        fn all_cas(apps: &[AccessApp]) -> HashMap<String, String> {
            apps.iter()
                .map(|a| (a.id.clone(), Self::ca_key(&a.id)))
                .collect()
        }
        fn with(apps: Vec<AccessApp>) -> Arc<Self> {
            let cas = Self::all_cas(&apps);
            Self::build(Ok(apps), None, cas, None)
        }
        fn without_cas(apps: Vec<AccessApp>) -> Arc<Self> {
            Self::build(Ok(apps), None, HashMap::new(), None)
        }
        fn refusing_cas(apps: Vec<AccessApp>, why: &str) -> Arc<Self> {
            Self::build(Ok(apps), None, HashMap::new(), Some(why.to_string()))
        }
        fn refusing_policies(apps: Vec<AccessApp>, why: &str) -> Arc<Self> {
            let cas = Self::all_cas(&apps);
            Self::build(Ok(apps), Some(why.to_string()), cas, None)
        }
        fn dark(msg: &str) -> Arc<Self> {
            Self::build(Err(msg.to_string()), None, HashMap::new(), None)
        }
        fn writes(&self) -> Vec<String> {
            self.writes.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl AccessApps for FakeAccess {
        async fn access_apps(&self, account_id: &str) -> Result<Vec<AccessApp>, String> {
            assert_eq!(account_id, "acct-1");
            *self.reads.lock().unwrap() += 1;
            self.apps.lock().unwrap().clone()
        }
        async fn create_access_app(
            &self,
            account_id: &str,
            spec: &AccessAppSpec,
        ) -> Result<String, String> {
            assert_eq!(account_id, "acct-1");
            let id = format!("app-{}", spec.domain);
            self.writes.lock().unwrap().push(format!(
                "create app {} {} {} {}",
                spec.name, spec.domain, spec.app_type, spec.session_duration
            ));
            if let Ok(apps) = &mut *self.apps.lock().unwrap() {
                apps.push(AccessApp {
                    id: id.clone(),
                    name: spec.name.clone(),
                    domain: spec.domain.clone(),
                    app_type: spec.app_type.clone(),
                    session_duration: spec.session_duration.clone(),
                    policies: vec![],
                });
            }
            Ok(id)
        }
        async fn create_access_policy(
            &self,
            account_id: &str,
            app_id: &str,
            spec: &AccessPolicySpec,
        ) -> Result<(), String> {
            assert_eq!(account_id, "acct-1");
            self.writes.lock().unwrap().push(format!(
                "create policy {} {} {} on {app_id} precedence={}",
                spec.name,
                spec.decision,
                Json::Array(spec.include.clone()),
                spec.precedence
            ));
            if let Some(why) = &self.refuse_policies {
                return Err(why.clone());
            }
            if let Ok(apps) = &mut *self.apps.lock().unwrap() {
                if apps
                    .iter()
                    .flat_map(|a| a.policies.iter())
                    .any(|p| p.precedence == spec.precedence)
                {
                    return Err(PRECEDENCE_TAKEN.to_string());
                }
                if let Some(app) = apps.iter_mut().find(|a| a.id == app_id) {
                    app.policies.push(AccessPolicy {
                        id: format!("pol-{}", spec.name),
                        name: spec.name.clone(),
                        decision: spec.decision.clone(),
                        include: spec.include.clone(),
                        precedence: spec.precedence,
                    });
                }
            }
            Ok(())
        }
        async fn short_lived_ca(
            &self,
            account_id: &str,
            app_id: &str,
        ) -> Result<Option<String>, String> {
            assert_eq!(account_id, "acct-1");
            Ok(self.cas.lock().unwrap().get(app_id).cloned())
        }
        async fn create_short_lived_ca(
            &self,
            account_id: &str,
            app_id: &str,
        ) -> Result<String, String> {
            assert_eq!(account_id, "acct-1");
            self.writes
                .lock()
                .unwrap()
                .push(format!("create ca on {app_id}"));
            if let Some(why) = &self.refuse_cas {
                return Err(why.clone());
            }
            let key = Self::ca_key(app_id);
            self.cas
                .lock()
                .unwrap()
                .insert(app_id.to_string(), key.clone());
            Ok(key)
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

    fn tunnel_cname() -> String {
        format!("{TUNNEL_ID}.cfargotunnel.com")
    }

    /// The zone exactly as measured on 2026-09-16 — before the flip.
    /// The old tunnel the IdP's record still pointed at on 2026-09-16 —
    /// deleted that morning, 530 from then on (fd75c641).
    const OLD_TUNNEL_CNAME: &str = "8bb06ec8-a6d1-4796-8f51-df363798b48c.cfargotunnel.com";
    /// The converge packet's ingress line once tunnel-origins.toml routes
    /// the IdP (the sibling car) and the dev door (5fc71f03), as the
    /// runner records it: both are declared origins, so both appear
    /// here, and the tunnel interlock on either record reads it.
    const CONVERGE_ROUTES_IDP: &str = "boss.algedonic.dev → boss; playground.algedonic.dev → boss (boss-playground skipped: secrets absent); id.algedonic.dev → https://10.20.0.31:443 (origin); dev.algedonic.dev → ssh://boss-dev-ssh.boss-dev.svc.cluster.local:22 (origin)";

    fn converge_listing(ingress: &str) -> Json {
        json!({"data": [{
            "kind": CONVERGE_KIND, "status": "closed",
            "steps": [{"spec_slug": "run", "status": "completed",
                       "metadata": {"cloudflared": "connected", "tunnel_ingress": ingress}}]
        }], "total": 1})
    }

    /// The zone before the boss. flip (measured 2026-09-16), PLUS the
    /// company website's record (b64c4377, 2026-09-17) already in
    /// place: the cases below are about the boss. flip's mechanics,
    /// and www — the same interlock, one more record — would otherwise
    /// be applied in every one of them and turn each into a test of
    /// two flips. www's own first observation is
    /// `the_website_record_is_applied_behind_its_own_access_app`.
    fn as_measured() -> Vec<Json> {
        vec![
            record("boss.algedonic.dev", "A", "10.20.0.33", false, 300),
            record("id.algedonic.dev", "CNAME", &tunnel_cname(), true, 1),
            record(
                "playground.algedonic.dev",
                "CNAME",
                &tunnel_cname(),
                true,
                1,
            ),
            record("www.algedonic.dev", "CNAME", &tunnel_cname(), true, 1),
            // The dev workspace's ssh door (5fc71f03), present here
            // although it did not exist on 2026-09-16: each test below
            // isolates ONE record's flip, and an absent interlocked
            // record the test is not about would add a create to every
            // write list. Its own creation is the subject of
            // `the_dev_door_record_is_created_once_the_converge_routes_it`.
            record("dev.algedonic.dev", "CNAME", &tunnel_cname(), true, 1),
        ]
    }

    /// The zone as declared — after the flip.
    fn as_declared() -> Vec<Json> {
        vec![
            record("boss.algedonic.dev", "CNAME", &tunnel_cname(), true, 1),
            record("id.algedonic.dev", "CNAME", &tunnel_cname(), true, 1),
            record(
                "playground.algedonic.dev",
                "CNAME",
                &tunnel_cname(),
                true,
                1,
            ),
            record("www.algedonic.dev", "CNAME", &tunnel_cname(), true, 1),
            // The dev workspace's ssh door (5fc71f03). It is absent
            // from `as_measured` deliberately: it did not exist on
            // 2026-09-16, so every fixture of the zone BEFORE the flip
            // is now one record short of the declaration, and each
            // test below that observes such a zone sees the observer
            // create this one behind its tunnel interlock.
            record("dev.algedonic.dev", "CNAME", &tunnel_cname(), true, 1),
        ]
    }

    /// The account as the shipped access.toml declares it — both
    /// applications present and matching.
    /// The account as read on 2026-09-16 (observation f0f2767f) and as
    /// access.toml has declared it since 08b8b396: the dashboard's
    /// onboarding policy beside `visitors`, and the callback bypass.
    fn account_as_declared() -> Vec<AccessApp> {
        vec![
            live_app("boss.algedonic.dev", vec![allow("operators", &[DAVID])]),
            // The company website (b64c4377): the boss. shape, behind
            // Access until the public launch.
            live_app("www.algedonic.dev", vec![allow("operators", &[DAVID])]),
            live_app(
                "playground.algedonic.dev",
                vec![
                    allow(
                        "Allow emails policy created by onboarding: 4/25/2026",
                        &[
                            "dhauld@gmail.com",
                            "emilyhiskes@gmail.com",
                            "richard@saouma.ch",
                            "tommy@mrp.io",
                        ],
                    ),
                    allow("visitors", &[DAVID]),
                ],
            ),
            live_app(
                "playground.algedonic.dev/auth",
                vec![AccessPolicy {
                    id: "pol-bypass".into(),
                    name: "Authorization bypass".into(),
                    decision: "bypass".into(),
                    include: vec![json!({"everyone": {}})],
                    precedence: 1,
                }],
            ),
            // The dev workspace's ssh door (5fc71f03): the only
            // application of another `type`, which is why it is here
            // rather than only in its own test — every MATCH fixture
            // must carry it, or the type a live read returns is one
            // nothing compares.
            AccessApp {
                app_type: "ssh".into(),
                ..live_app("dev.algedonic.dev", vec![allow("operators", &[DAVID])])
            },
        ]
    }

    /// The account with the playground side as declared and boss. not
    /// yet fronted — the state before the first boss. observation.
    fn account_without_boss() -> Vec<AccessApp> {
        account_as_declared()
            .into_iter()
            .filter(|a| a.domain != "boss.algedonic.dev")
            .collect()
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
        stub_jobs_api_with_converge(
            observe_status,
            open_alarms,
            storage_location,
            CONVERGE_ROUTES_IDP,
        )
        .await
    }

    async fn stub_jobs_api_with_converge(
        observe_status: &'static str,
        open_alarms: Vec<Json>,
        storage_location: &'static str,
        converge_ingress: &'static str,
    ) -> (String, Captured) {
        use axum::extract::{Path, Query};
        use axum::response::IntoResponse;
        use axum::{Json as AxJson, Router, routing::get, routing::post, routing::put};

        let captured: Captured = Default::default();
        let (c1, c2, c3, c4) = (
            captured.clone(),
            captured.clone(),
            captured.clone(),
            captured.clone(),
        );
        let alarms = Arc::new(open_alarms);
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let alarms = alarms.clone();
                    async move {
                        // The tunnel interlock's read (fd75c641): the
                        // newest converge routes the IdP, connector
                        // connected — the released state.
                        if q.get("kind").map(String::as_str) == Some(CONVERGE_KIND) {
                            return AxJson(converge_listing(converge_ingress));
                        }
                        assert_eq!(q.get("kind").map(String::as_str), Some("backlog-item"));
                        assert_eq!(q.get("status").map(String::as_str), Some("open"));
                        // Answered as the jobs API answers it, behind more
                        // unrelated open items than one page (c5ac71de).
                        AxJson(crate::handlers::listing_stub::backlog_listing(
                            &alarms,
                            q.get("metadata_has").map(String::as_str),
                            q.get("limit").and_then(|l| l.parse().ok()),
                        ))
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
                        // The decided end state (e39a9d2a).
                        if let Some(refused) =
                            super::super::listing_stub::end_state_step_put(&id, &sid, &body)
                        {
                            c.lock()
                                .unwrap()
                                .push((format!("PUT /api/jobs/{id}/steps/{sid} (409)"), body));
                            return refused;
                        }
                        c.lock()
                            .unwrap()
                            .push((format!("PUT /api/jobs/{id}/steps/{sid}"), body));
                        AxJson(json!({ "ok": true })).into_response()
                    }
                }),
            )
            // The step merge door, recorded in order with the PUT.
            .route(
                "/api/jobs/{id}/steps/{sid}/metadata",
                axum::routing::patch(
                    move |Path((id, sid)): Path<(String, String)>, AxJson(body): AxJson<Json>| {
                        let c = c4.clone();
                        async move {
                            c.lock().unwrap().push((
                                format!("PATCH /api/jobs/{id}/steps/{sid}/metadata"),
                                body,
                            ));
                            AxJson(json!({ "ok": true }))
                        }
                    },
                ),
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

    /// The declarations directory the tree ships — the real comparator,
    /// the real algedonic.dev.toml and the real access.toml, run by the
    /// handler.
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
        access: Arc<FakeAccess>,
        secrets: Arc<FakeSecrets>,
        declarations: Option<String>,
    ) -> Arc<DnsObserve> {
        DnsObserve::new(
            jobs,
            zone,
            access,
            secrets,
            declarations,
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    fn writes(c: &Captured) -> Vec<(String, Json)> {
        c.lock().unwrap().clone()
    }

    /// The observe step's completion as the stub saw it: the fields
    /// through the step merge door, then a PUT carrying the status and
    /// nothing else (e39a9d2a). Answers the merged fields.
    fn step_put(w: &[(String, Json)]) -> Json {
        let merged = w
            .iter()
            .position(|(p, _)| p == MERGE_OBSERVE)
            .unwrap_or_else(|| panic!("no merge onto the observe step among {w:?}"));
        assert_eq!(
            w.get(merged + 1),
            Some(&(PUT_OBSERVE.to_string(), json!({"status": "completed"}))),
            "the merge is followed by the flip, and the flip carries the status alone: {w:?}"
        );
        w[merged].1.clone()
    }

    /// The two writes that complete the observe step, in order.
    const MERGE_OBSERVE: &str = "PATCH /api/jobs/obs-1/steps/step-observe/metadata";
    const PUT_OBSERVE: &str = "PUT /api/jobs/obs-1/steps/step-observe";

    fn boss_verdict(body: &Json) -> Json {
        body["verdicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == "boss.algedonic.dev")
            .cloned()
            .unwrap_or_else(|| panic!("no boss. verdict: {body}"))
    }

    // ----- the dev door's short-lived-certificate CA (incident 55d001b0) -----

    /// 2026-09-23: the account held the dev door's application and no
    /// CA for it — the dashboard offers only the account-wide
    /// Access-for-Infrastructure CA — so `cloudflared access ssh` was
    /// refused "bad ca application". The observer generates it and
    /// records the public key the pod's sshd must trust.
    #[tokio::test]
    async fn the_dev_door_ca_is_generated_and_its_public_key_recorded() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_declared());
        let access = FakeAccess::without_cas(account_as_declared());
        let h = handler(
            jobs,
            zone.clone(),
            access.clone(),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();

        assert_eq!(
            access.writes(),
            vec!["create ca on app-dev.algedonic.dev".to_string()],
            "only the application that declares a CA gets one"
        );
        let w = writes(&captured);
        assert_eq!(w.len(), 2, "the merge and the flip, no alarm: {w:?}");
        let body = step_put(&w);
        assert_eq!(body["result"], "match");
        assert_eq!(
            body["applied"],
            json!(["Access short-lived certificate CA created on dev.algedonic.dev"])
        );
        assert_eq!(
            body["ssh_ca"],
            json!([{
                "domain": "dev.algedonic.dev",
                "public_key": "ecdsa-sha2-nistp256 CA-OF-app-dev.algedonic.dev",
            }])
        );
    }

    #[tokio::test]
    async fn a_present_ca_is_recorded_and_not_generated_again() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_declared());
        let access = FakeAccess::with(account_as_declared());
        let h = handler(
            jobs,
            zone.clone(),
            access.clone(),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();

        assert!(access.writes().is_empty(), "{:?}", access.writes());
        let body = step_put(&writes(&captured));
        assert_eq!(
            body["ssh_ca"],
            json!([{
                "domain": "dev.algedonic.dev",
                "public_key": "ecdsa-sha2-nistp256 CA-OF-app-dev.algedonic.dev",
            }])
        );
    }

    #[tokio::test]
    async fn a_refused_ca_is_a_finding_that_names_the_account_answer() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_declared());
        let why = "POST /accounts/acct-1/access/apps/app-dev.algedonic.dev/ca returned 403";
        let access = FakeAccess::refusing_cas(account_as_declared(), why);
        let h = handler(
            jobs,
            zone.clone(),
            access.clone(),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();

        let w = writes(&captured);
        assert!(
            w.iter().any(|(p, _)| p == "POST /api/jobs"),
            "a refused CA raises the alarm: {w:?}"
        );
        let body = step_put(&w);
        assert_eq!(body["result"], "findings");
        assert_eq!(body["ssh_ca"], json!([]));
        let refused: Vec<&Json> = body["access"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["verdict"] == "REFUSED")
            .collect();
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(
            refused[0]["write"],
            "create short-lived certificate CA on dev.algedonic.dev"
        );
        assert_eq!(refused[0]["error"], why);
    }

    // ----- steady state -----

    #[tokio::test]
    async fn the_zone_and_account_as_declared_complete_observe_as_match_and_write_nothing() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_declared());
        let access = FakeAccess::with(account_as_declared());
        let h = handler(
            jobs,
            zone.clone(),
            access.clone(),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx())
            .await
            .expect("observation succeeds");

        assert_eq!(*zone.reads.lock().unwrap(), 1, "no apply, no re-read");
        assert_eq!(*access.reads.lock().unwrap(), 1);
        assert!(zone.writes().is_empty() && access.writes().is_empty());
        let w = writes(&captured);
        assert_eq!(
            w.len(),
            2,
            "the step completion — merge, then flip; no alarm: {w:?}"
        );
        let body = step_put(&w);
        assert_eq!(body["result"], "match");
        assert!(
            body.get("kept").is_none(),
            "the step's own keys are not re-sent: the merge door keeps them (e39a9d2a)"
        );
        let verdicts = body["verdicts"].as_array().unwrap();
        assert_eq!(verdicts.len(), 5, "boss., id., playground., www. and dev.");
        assert!(
            verdicts.iter().all(|v| v["verdict"] == "MATCH"),
            "{verdicts:?}"
        );
        let boss = boss_verdict(&body);
        assert_eq!(
            boss["access"], "present",
            "the interlock's reading rides the verdict"
        );
        assert_eq!(
            boss["declared"]["content"],
            tunnel_cname(),
            "the tunnel reference was resolved from the Secret's TunnelID"
        );
        assert!(
            !body.to_string().contains("Zml4dHVyZS"),
            "tunnel secret leaked: {body}"
        );
        let access_v = body["access"].as_array().unwrap();
        assert_eq!(
            access_v.len(),
            5,
            "boss., www., dev., the playground and its callback bypass"
        );
        assert!(
            access_v.iter().all(|v| v["verdict"] == "MATCH"),
            "{access_v:?}"
        );
        assert_eq!(body["applied"], json!([]));
        let summary = body["summary"].as_str().unwrap();
        assert!(
            summary.contains(
                "5 match, 0 drift, 0 absent, 0 undeclared — every declared record matches"
            ) && summary.contains("· access: 5 match, 0 drift, 0 absent, 0 undeclared"),
            "{summary}"
        );
    }

    // ----- the first observation after 198c5fe9 lands -----

    #[tokio::test]
    async fn the_first_observation_creates_the_access_app_and_flips_boss_behind_the_tunnel() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_measured());
        // The account before: only the dashboard's playground app.
        let access = FakeAccess::with(account_without_boss());
        let h = handler(
            jobs,
            zone.clone(),
            access.clone(),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();

        // Access: the app and its policy created, then the account
        // read again (2 reads).
        assert_eq!(
            access.writes(),
            vec![
                "create app BOSS boss.algedonic.dev self_hosted 24h".to_string(),
                // 2, not 1: the dashboard's visitors policy holds 1 and
                // the account refuses a second 1 on ANY application.
                format!(
                    "create policy operators allow [{{\"email\":{{\"email\":\"{DAVID}\"}}}}] on app-boss.algedonic.dev precedence=2"
                ),
            ]
        );
        assert_eq!(
            *access.reads.lock().unwrap(),
            2,
            "read back after the create"
        );
        // The zone: the A record replaced by the CNAME, then re-read.
        assert_eq!(
            zone.writes(),
            vec![
                "delete rec-boss.algedonic.dev-A".to_string(),
                format!(
                    "create boss.algedonic.dev CNAME {} proxied=true ttl=1",
                    tunnel_cname()
                ),
            ]
        );
        assert_eq!(*zone.reads.lock().unwrap(), 2, "read back after the apply");
        assert_eq!(zone.live().len(), 5, "no A left beside the CNAME");

        let w = writes(&captured);
        assert_eq!(
            w.len(),
            2,
            "no alarm: the reading after the writes matches: {w:?}"
        );
        let body = step_put(&w);
        assert_eq!(body["result"], "match");
        let boss = boss_verdict(&body);
        assert_eq!(boss["verdict"], "MATCH", "{boss}");
        assert_eq!(boss["access"], "created");
        assert_eq!(boss["type"], "CNAME");
        assert_eq!(
            body["applied"],
            json!([
                "Access application boss.algedonic.dev created",
                "Access policy operators (allow) created on boss.algedonic.dev",
                "boss.algedonic.dev CNAME created (replacing 1 record(s) at that name)",
            ])
        );
        let summary = body["summary"].as_str().unwrap();
        assert!(
            summary.contains("applied: Access application boss.algedonic.dev created"),
            "{summary}"
        );
        assert!(!summary.contains("flip held"), "{summary}");
    }

    /// The company website's first observation (b64c4377, 2026-09-17):
    /// the zone holds boss., id. and playground. as declared and no www
    /// at all; the account fronts boss. and the playground and not www.
    /// The observer creates the www Access application with its
    /// operators policy, reads the account back, and only then creates
    /// the www CNAME behind the released interlock — the boss. shape,
    /// so the website is never reachable ahead of the door in front of
    /// it. Nothing else is written.
    #[tokio::test]
    async fn the_website_record_is_applied_behind_its_own_access_app() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let before: Vec<Json> = as_declared()
            .into_iter()
            .filter(|r| r["name"] != "www.algedonic.dev")
            .collect();
        let zone = FakeZone::with(before);
        let access = FakeAccess::with(
            account_as_declared()
                .into_iter()
                .filter(|a| a.domain != "www.algedonic.dev")
                .collect(),
        );
        let h = handler(
            jobs,
            zone.clone(),
            access.clone(),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();
        assert_eq!(
            access.writes(),
            vec![
                "create app BOSS site www.algedonic.dev self_hosted 24h".to_string(),
                format!(
                    "create policy operators allow [{{\"email\":{{\"email\":\"{DAVID}\"}}}}] on app-www.algedonic.dev precedence=2"
                ),
            ]
        );
        assert_eq!(
            zone.writes(),
            vec![format!(
                "create www.algedonic.dev CNAME {} proxied=true ttl=1",
                tunnel_cname()
            )],
            "the record is created once the application reads present; nothing else is touched"
        );
        let w = writes(&captured);
        assert_eq!(w.len(), 2, "the merge and the flip, no alarm: {w:?}");
        let body = step_put(&w);
        assert_eq!(body["result"], "match");
        let www = body["verdicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == "www.algedonic.dev")
            .cloned()
            .unwrap_or_else(|| panic!("no www verdict: {body}"));
        assert_eq!(www["verdict"], "MATCH", "{www}");
        assert_eq!(www["access"], "created");
        assert_eq!(
            body["applied"],
            json!([
                "Access application www.algedonic.dev created",
                "Access policy operators (allow) created on www.algedonic.dev",
                "www.algedonic.dev CNAME created",
            ])
        );
    }

    /// The measured 2026-09-16 16:00Z firing (packet 95cf8740): the
    /// interlock released, the A record was deleted, the CNAME create
    /// was refused. Until this car the handler failed and the packet
    /// held a dead letter and nothing it had read. Now: the zone is
    /// re-read as it IS (boss. ABSENT — hard), the account's answer
    /// rides that verdict as `refused`, the alarm carries it, the step
    /// completes with findings, and the next reading creates the record
    /// with no delete in front of it.
    #[tokio::test]
    async fn a_refused_zone_write_is_a_finding_on_the_re_read_verdict() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::refusing_creates(as_measured(), COMMENT_TOO_LONG);
        let access = FakeAccess::with(account_as_declared());
        let h = handler(
            jobs,
            zone.clone(),
            access.clone(),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx())
            .await
            .expect("a refused write is a finding, not a failed firing");
        assert_eq!(
            zone.writes(),
            vec![
                "delete rec-boss.algedonic.dev-A".to_string(),
                format!(
                    "create boss.algedonic.dev CNAME {} proxied=true ttl=1",
                    tunnel_cname()
                ),
            ]
        );
        assert_eq!(*zone.reads.lock().unwrap(), 2, "re-read after the write");
        let w = writes(&captured);
        assert_eq!(
            w.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["POST /api/jobs", MERGE_OBSERVE, PUT_OBSERVE],
            "the alarm, then the step: {w:?}"
        );
        let body = step_put(&w);
        assert_eq!(body["result"], "findings");
        let boss = boss_verdict(&body);
        assert_eq!(boss["verdict"], "ABSENT", "the zone as it is now: {boss}");
        assert_eq!(
            boss["refused"], COMMENT_TOO_LONG,
            "the account's answer, verbatim"
        );
        assert_eq!(
            body["applied"],
            json!([]),
            "nothing was written that the account took"
        );
        let summary = body["summary"].as_str().unwrap();
        assert!(
            summary.contains("refused by the zone: boss.algedonic.dev: POST /zones/zone-1/dns_records returned 400"),
            "{summary}"
        );
        let alarm = &w[0].1;
        assert!(
            alarm["metadata"]["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["name"] == "boss.algedonic.dev" && f["refused"] == COMMENT_TOO_LONG),
            "the alarm carries the refusal: {}",
            alarm["metadata"]["findings"]
        );
    }

    #[test]
    fn a_refusal_for_a_name_the_re_read_no_longer_lists_gets_its_own_absent_verdict() {
        let out = annotate_refused(
            &[json!({"name": "www.algedonic.dev", "verdict": "MATCH"})],
            &[("boss.algedonic.dev".to_string(), "no".to_string())],
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[1]["verdict"], "ABSENT");
        assert_eq!(out[1]["refused"], "no");
        let kept = annotate_refused(&out, &[("www.algedonic.dev".to_string(), "x".to_string())]);
        assert_eq!(kept[0]["verdict"], "MATCH", "a right zone stays right");
        assert_eq!(kept[0]["refused"], "x", "and the refusal is still recorded");
    }

    /// The dev workspace's ssh door (design 5fc71f03; backlog
    /// e4cedb46). Its record rides the SAME interlock the IdP's does —
    /// applied only once the converge reports the tunnel routing it —
    /// because a CNAME to a tunnel with no rule for the name is the
    /// 530 of 2026-09-16, and an ssh door that answers 530 looks
    /// exactly like an ssh door that is down.
    #[tokio::test]
    async fn the_dev_door_record_is_created_once_the_converge_routes_it() {
        let without_dev: Vec<Json> = as_declared()
            .into_iter()
            .filter(|r| r["name"] != "dev.algedonic.dev")
            .collect();
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(without_dev.clone());
        let access = FakeAccess::with(account_as_declared());
        let h = handler(jobs, zone.clone(), access, secrets(), declarations());
        h.invoke(&zone_args(), &ctx()).await.unwrap();
        assert_eq!(
            zone.writes(),
            vec![format!(
                "create dev.algedonic.dev CNAME {} proxied=true ttl=1",
                tunnel_cname()
            )],
            "the door's record created behind the tunnel interlock, nothing else touched"
        );
        let body = step_put(&writes(&captured));
        let dev = body["verdicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == "dev.algedonic.dev")
            .cloned()
            .unwrap_or_else(|| panic!("no dev. verdict: {body}"));
        assert_eq!(dev["tunnel"], "routed");

        // With a converge that does not name it, nothing is written:
        // the record waits for the route rather than answering 530.
        let (jobs, _captured) = stub_jobs_api_with_converge(
            "ready",
            vec![],
            LOCATION,
            "boss.algedonic.dev → boss; id.algedonic.dev → https://10.20.0.31:443 (origin)",
        )
        .await;
        let zone = FakeZone::with(without_dev);
        let access = FakeAccess::with(account_as_declared());
        let h = handler(jobs, zone.clone(), access, secrets(), declarations());
        h.invoke(&zone_args(), &ctx()).await.unwrap();
        assert!(zone.writes().is_empty(), "held: {:?}", zone.writes());
    }

    /// The 2026-09-16 outage (fd75c641): the IdP's record pointed at
    /// the deleted tunnel. With the converge routing id. (the sibling
    /// car landed) the observer corrects the CNAME in place behind the
    /// tunnel interlock; with a converge that does not route it, the
    /// DRIFT is HELD with the tunnel's phrase and nothing is written.
    #[tokio::test]
    async fn the_idp_record_follows_the_tunnel_once_the_converge_routes_it() {
        let stale = vec![
            record("boss.algedonic.dev", "CNAME", &tunnel_cname(), true, 1),
            record("id.algedonic.dev", "CNAME", OLD_TUNNEL_CNAME, true, 1),
            record(
                "playground.algedonic.dev",
                "CNAME",
                &tunnel_cname(),
                true,
                1,
            ),
            record("www.algedonic.dev", "CNAME", &tunnel_cname(), true, 1),
            // Already applied, so the id. correction is the only write
            // this test has to account for (see `as_measured`).
            record("dev.algedonic.dev", "CNAME", &tunnel_cname(), true, 1),
        ];
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(stale.clone());
        let access = FakeAccess::with(account_as_declared());
        let h = handler(jobs, zone.clone(), access, secrets(), declarations());
        h.invoke(&zone_args(), &ctx()).await.unwrap();
        assert_eq!(
            zone.writes(),
            vec![format!(
                "update rec-id.algedonic.dev-CNAME {} proxied=true",
                tunnel_cname()
            )],
            "the stale CNAME corrected in place, nothing else touched"
        );
        let body = step_put(&writes(&captured));
        assert_eq!(body["result"], "match", "{body}");
        let idp = body["verdicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == "id.algedonic.dev")
            .cloned()
            .unwrap();
        assert_eq!(idp["verdict"], "MATCH");
        assert_eq!(idp["tunnel"], "routed");
        assert!(idp.get("access").is_none(), "{idp}");
        assert_eq!(body["applied"], json!(["id.algedonic.dev CNAME corrected"]));

        // Before the sibling car converges: the converge's ingress line
        // does not name id. — HELD, alarm raised, nothing written.
        let (jobs, captured) = stub_jobs_api_with_converge(
            "ready",
            vec![],
            LOCATION,
            "boss.algedonic.dev → boss; playground.algedonic.dev → boss (boss-playground skipped: secrets absent)",
        )
        .await;
        let zone = FakeZone::with(stale);
        let access = FakeAccess::with(account_as_declared());
        let h = handler(jobs, zone.clone(), access, secrets(), declarations());
        h.invoke(&zone_args(), &ctx()).await.unwrap();
        assert!(zone.writes().is_empty(), "held: {:?}", zone.writes());
        // HELD is paperwork, as it is for the Access half: the step
        // completes, the hold is on the verdict and in the summary, and
        // no alarm is raised for a write the interlock is still holding.
        let w = writes(&captured);
        assert_eq!(
            w.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec![MERGE_OBSERVE, PUT_OBSERVE],
            "{w:?}"
        );
        let body = step_put(&w);
        assert!(
            body["summary"]
                .as_str()
                .unwrap()
                .contains("id.algedonic.dev: flip held — the converge does not route"),
            "{}",
            body["summary"]
        );
        let idp = body["verdicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == "id.algedonic.dev")
            .cloned()
            .unwrap();
        assert_eq!(idp["verdict"], "HELD", "{idp}");
        assert_eq!(idp["tunnel"], "not-routed");
        assert!(
            idp["held"].as_str().unwrap().contains("does not route"),
            "{idp}"
        );
    }

    #[tokio::test]
    async fn a_present_access_app_releases_the_flip_without_a_create() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_measured());
        let access = FakeAccess::with(account_as_declared());
        let h = handler(
            jobs,
            zone.clone(),
            access.clone(),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();
        assert!(access.writes().is_empty());
        assert_eq!(*access.reads.lock().unwrap(), 1);
        assert_eq!(zone.writes().len(), 2, "{:?}", zone.writes());
        let body = step_put(&writes(&captured));
        let boss = boss_verdict(&body);
        assert_eq!(boss["verdict"], "MATCH");
        assert_eq!(boss["access"], "present");
    }

    // ----- the interlock holding -----

    #[tokio::test]
    async fn an_access_app_that_cannot_be_created_holds_the_flip_and_says_so() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_measured());
        // The account answers reads but the create is refused (the
        // token without Access: Edit, say). Until 2026-09-16 this
        // failed the firing: eight redeliveries against the same
        // answer, a dead letter, and a packet that recorded neither
        // the zone nor the refusal. The refusal is what the reading
        // is FOR: a REFUSED finding, the zone still read, the flip
        // held on the verdict, the alarm naming the account's answer.
        struct RefusingAccess(Arc<FakeAccess>);
        #[async_trait]
        impl AccessApps for RefusingAccess {
            async fn short_lived_ca(&self, a: &str, i: &str) -> Result<Option<String>, String> {
                self.0.short_lived_ca(a, i).await
            }
            async fn create_short_lived_ca(&self, a: &str, i: &str) -> Result<String, String> {
                self.0.create_short_lived_ca(a, i).await
            }
            async fn access_apps(&self, a: &str) -> Result<Vec<AccessApp>, String> {
                self.0.access_apps(a).await
            }
            async fn create_access_app(
                &self,
                _a: &str,
                _s: &AccessAppSpec,
            ) -> Result<String, String> {
                Err("POST /accounts/acct-1/access/apps returned 403: token lacks Access: Apps and Policies: Edit".into())
            }
            async fn create_access_policy(
                &self,
                _a: &str,
                _i: &str,
                _s: &AccessPolicySpec,
            ) -> Result<(), String> {
                unreachable!()
            }
        }
        let inner = FakeAccess::with(account_without_boss());
        let h = DnsObserve::new(
            jobs,
            zone.clone(),
            Arc::new(RefusingAccess(inner)),
            secrets(),
            declarations(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        h.invoke(&zone_args(), &ctx())
            .await
            .expect("a refused write is a finding, not a failed firing");
        assert!(zone.writes().is_empty(), "nothing touches the zone");
        assert_eq!(*zone.reads.lock().unwrap(), 1, "the zone is still read");

        let w = writes(&captured);
        assert_eq!(
            w.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["POST /api/jobs", MERGE_OBSERVE, PUT_OBSERVE],
            "the alarm, then the step: {w:?}"
        );
        let body = step_put(&w);
        assert_eq!(body["result"], "findings");
        let boss = boss_verdict(&body);
        assert_eq!(boss["verdict"], "HELD", "{boss}");
        assert_eq!(boss["access"], "absent");
        let access_v = body["access"].as_array().unwrap();
        let refused: Vec<&Json> = access_v
            .iter()
            .filter(|v| v["verdict"] == "REFUSED")
            .collect();
        assert_eq!(refused.len(), 1, "{access_v:?}");
        assert_eq!(refused[0]["domain"], "boss.algedonic.dev");
        assert_eq!(
            refused[0]["write"],
            "create Access application boss.algedonic.dev"
        );
        assert!(
            refused[0]["error"].as_str().unwrap().contains("403"),
            "the account's own answer, verbatim: {}",
            refused[0]
        );
        assert_eq!(body["counts"]["access"]["REFUSED"], 1);
        let summary = body["summary"].as_str().unwrap();
        assert!(
            summary.contains("1 absent, 0 undeclared, 1 refused"),
            "{summary}"
        );
        let alarm = &w[0].1;
        let findings = alarm["metadata"]["findings"].as_array().unwrap();
        assert!(
            findings
                .iter()
                .any(|f| f["verdict"] == "REFUSED" && f["scope"] == "access"),
            "the alarm carries the refusal: {findings:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_policy_is_a_finding_and_the_next_reading_takes_the_next_precedence() {
        // The measured 2026-09-16 firing (packet ca4dd287): the boss.
        // application was created, then its first policy — sent at
        // precedence 1 while the dashboard's playground policy held 1
        // — was refused `policy precedences must be unique`. Day one:
        // the app stands without an allow policy, the flip is HELD,
        // the refusal is on the packet and in the alarm.
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_measured());
        let access = FakeAccess::refusing_policies(account_without_boss(), PRECEDENCE_TAKEN);
        let h = handler(
            jobs,
            zone.clone(),
            access.clone(),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();
        assert_eq!(
            access.writes(),
            vec![
                "create app BOSS boss.algedonic.dev self_hosted 24h".to_string(),
                format!(
                    "create policy operators allow [{{\"email\":{{\"email\":\"{DAVID}\"}}}}] on app-boss.algedonic.dev precedence=2"
                ),
            ],
            "the app created, the policy sent (and refused)"
        );
        assert!(zone.writes().is_empty(), "the interlock holds the flip");
        let w = writes(&captured);
        assert_eq!(
            w.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["POST /api/jobs", MERGE_OBSERVE, PUT_OBSERVE],
            "{w:?}"
        );
        let body = step_put(&w);
        assert_eq!(body["result"], "findings");
        let boss = boss_verdict(&body);
        assert_eq!(boss["verdict"], "HELD", "{boss}");
        assert_eq!(
            boss["access"], "no-allow-policy",
            "the app is there, its allow policy is not: {boss}"
        );
        let refused: Vec<Json> = body["access"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["verdict"] == "REFUSED")
            .cloned()
            .collect();
        assert_eq!(refused.len(), 1, "{body}");
        assert_eq!(
            refused[0]["write"],
            "create Access policy operators (allow) on boss.algedonic.dev at precedence 2"
        );
        assert!(
            refused[0]["error"]
                .as_str()
                .unwrap()
                .contains("policy precedences must be unique"),
            "{}",
            refused[0]
        );
        assert_eq!(
            body["applied"],
            json!(["Access application boss.algedonic.dev created"]),
            "what WAS written is still recorded"
        );

        // Day two: the account holds the app (no policy) and the
        // dashboard's policy at 1. The reading finds `operators`
        // absent, creates it one above every precedence the account
        // lists — 2 — and the flip releases.
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_measured());
        let mut apps = account_without_boss();
        apps.push(live_app("boss.algedonic.dev", vec![]));
        let access = FakeAccess::with(apps);
        let h = handler(
            jobs,
            zone.clone(),
            access.clone(),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();
        assert_eq!(
            access.writes(),
            vec![format!(
                "create policy operators allow [{{\"email\":{{\"email\":\"{DAVID}\"}}}}] on app-boss.algedonic.dev precedence=2"
            )]
        );
        assert_eq!(zone.writes().len(), 2, "{:?}", zone.writes());
        let body = step_put(&writes(&captured));
        assert_eq!(body["result"], "match", "{body}");
        let boss = boss_verdict(&body);
        assert_eq!(boss["verdict"], "MATCH");
        assert_eq!(boss["access"], "present");
    }

    #[tokio::test]
    async fn an_access_app_without_an_allow_policy_holds_the_flip_on_the_packet() {
        // An app somebody made in the dashboard for boss. with no
        // policy at all: the declared policy is absent, so it is
        // created — and the gate then releases. Force the HELD leg
        // instead with an app whose only policy denies: the declared
        // `operators` policy is created (DRIFT with policies_absent),
        // but the re-read here refuses to grow it, modelling a policy
        // create the API accepted and did not apply.
        struct StubbornAccess(Arc<FakeAccess>);
        #[async_trait]
        impl AccessApps for StubbornAccess {
            async fn short_lived_ca(&self, a: &str, i: &str) -> Result<Option<String>, String> {
                self.0.short_lived_ca(a, i).await
            }
            async fn create_short_lived_ca(&self, a: &str, i: &str) -> Result<String, String> {
                self.0.create_short_lived_ca(a, i).await
            }
            async fn access_apps(&self, a: &str) -> Result<Vec<AccessApp>, String> {
                self.0.access_apps(a).await
            }
            async fn create_access_app(
                &self,
                a: &str,
                s: &AccessAppSpec,
            ) -> Result<String, String> {
                self.0.create_access_app(a, s).await
            }
            async fn create_access_policy(
                &self,
                _a: &str,
                _i: &str,
                _s: &AccessPolicySpec,
            ) -> Result<(), String> {
                Ok(()) // accepted, never applied
            }
        }
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_measured());
        let mut deny = allow("block", &[DAVID]);
        deny.decision = "deny".into();
        let mut apps = vec![live_app("boss.algedonic.dev", vec![deny])];
        apps.extend(account_without_boss());
        let inner = FakeAccess::with(apps);
        let h = DnsObserve::new(
            jobs,
            zone.clone(),
            Arc::new(StubbornAccess(inner)),
            secrets(),
            declarations(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();

        assert!(
            zone.writes().is_empty(),
            "the flip is held: {:?}",
            zone.writes()
        );
        assert_eq!(*zone.reads.lock().unwrap(), 1);
        let w = writes(&captured);
        let order: Vec<&str> = w.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            order,
            vec!["POST /api/jobs", MERGE_OBSERVE, PUT_OBSERVE],
            "the Access DRIFT (a deny policy the declaration does not name) is a finding; the held flip is not"
        );
        let body = step_put(&w);
        let boss = boss_verdict(&body);
        assert_eq!(boss["verdict"], "HELD", "{boss}");
        assert_eq!(boss["access"], "no-allow-policy");
        assert_eq!(boss["held"], "flip held — Access app has no allow policy");
        let summary = body["summary"].as_str().unwrap();
        assert!(
            summary.contains("boss.algedonic.dev: flip held — Access app has no allow policy"),
            "{summary}"
        );
        let alarm = &w[0].1;
        let findings = alarm["metadata"]["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0]["scope"], "access");
        assert_eq!(findings[0]["application"], "boss.algedonic.dev");
        assert!(
            findings.iter().all(|f| f["verdict"] != "HELD"),
            "a held flip never rides an alarm: {findings:?}"
        );
    }

    #[tokio::test]
    async fn an_app_created_but_not_yet_readable_holds_the_flip_as_absent_and_alarms() {
        // The create is accepted and the account read again — and the
        // re-read does not list the application yet (eventual
        // consistency, or a create the API acknowledged and dropped).
        // The gate judges the RE-READ, never the send: the record is
        // held with `flip held — Access app absent`, and the ABSENT
        // application is a finding the alarm carries. The next daily
        // reading sees the app and releases the flip.
        struct LaggingAccess(Arc<FakeAccess>);
        #[async_trait]
        impl AccessApps for LaggingAccess {
            async fn short_lived_ca(&self, a: &str, i: &str) -> Result<Option<String>, String> {
                self.0.short_lived_ca(a, i).await
            }
            async fn create_short_lived_ca(&self, a: &str, i: &str) -> Result<String, String> {
                self.0.create_short_lived_ca(a, i).await
            }
            async fn access_apps(&self, a: &str) -> Result<Vec<AccessApp>, String> {
                let apps = self.0.access_apps(a).await?;
                Ok(apps
                    .into_iter()
                    .filter(|app| app.domain != "boss.algedonic.dev")
                    .collect())
            }
            async fn create_access_app(
                &self,
                a: &str,
                s: &AccessAppSpec,
            ) -> Result<String, String> {
                self.0.create_access_app(a, s).await
            }
            async fn create_access_policy(
                &self,
                a: &str,
                i: &str,
                s: &AccessPolicySpec,
            ) -> Result<(), String> {
                self.0.create_access_policy(a, i, s).await
            }
        }
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_measured());
        let inner = FakeAccess::with(account_without_boss());
        let h = DnsObserve::new(
            jobs,
            zone.clone(),
            Arc::new(LaggingAccess(inner.clone())),
            secrets(),
            declarations(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();
        assert_eq!(
            inner.writes().len(),
            2,
            "the app and its policy were created: {:?}",
            inner.writes()
        );
        assert!(zone.writes().is_empty(), "held: {:?}", zone.writes());
        let w = writes(&captured);
        let order: Vec<&str> = w.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            order,
            vec!["POST /api/jobs", MERGE_OBSERVE, PUT_OBSERVE],
            "the application ABSENT on the re-read is a finding, alarmed"
        );
        let body = step_put(&w);
        let boss = boss_verdict(&body);
        assert_eq!(boss["verdict"], "HELD", "{boss}");
        assert_eq!(boss["access"], "absent");
        assert_eq!(boss["held"], "flip held — Access app absent");
        assert_eq!(body["result"], "findings");
        assert!(
            body["summary"]
                .as_str()
                .unwrap()
                .contains("boss.algedonic.dev: flip held — Access app absent")
        );
        let findings = w[0].1["metadata"]["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0]["verdict"], "ABSENT");
        assert_eq!(findings[0]["scope"], "access");
    }

    // ----- findings -----

    #[tokio::test]
    async fn an_undeclared_record_is_recorded_on_the_packet_and_raises_nothing() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let mut live = as_declared();
        live.push(record("id.algedonic.dev", "A", "203.0.113.7", false, 300));
        let h = handler(
            jobs,
            FakeZone::with(live),
            FakeAccess::with(account_as_declared()),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();

        let w = writes(&captured);
        assert_eq!(w.len(), 2, "the merge and the flip: {w:?}");
        let body = &w[0].1;
        assert_eq!(
            body["result"], "match",
            "UNDECLARED is reported, not a failure"
        );
        let verdicts = body["verdicts"].as_array().unwrap();
        let id = verdicts
            .iter()
            .find(|v| v["record"] == "id.algedonic.dev A")
            .unwrap();
        assert_eq!(id["verdict"], "UNDECLARED");
        assert_eq!(id["live"]["content"], "203.0.113.7");
    }

    #[tokio::test]
    async fn drift_on_the_rotation_owned_record_raises_the_alarm_before_completing_the_step() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let mut live = as_declared();
        let pg = live
            .iter()
            .position(|r| r["name"] == "playground.algedonic.dev")
            .unwrap();
        live[pg] = record(
            "playground.algedonic.dev",
            "CNAME",
            "00000000-1111-4222-8333-444444444444.cfargotunnel.com",
            true,
            1,
        );
        let zone = FakeZone::with(live);
        let h = handler(
            jobs,
            zone.clone(),
            FakeAccess::with(account_as_declared()),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();

        assert!(
            zone.writes().is_empty(),
            "playground. carries no interlock: the observer never writes it"
        );
        let w = writes(&captured);
        let order: Vec<&str> = w.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            order,
            vec!["POST /api/jobs", MERGE_OBSERVE, PUT_OBSERVE],
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
        assert_eq!(step_put(&w)["result"], "findings");
    }

    #[tokio::test]
    async fn drift_on_the_interlocked_record_is_corrected_in_place_behind_the_gate() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let mut live = as_declared();
        live[0] = record("boss.algedonic.dev", "CNAME", &tunnel_cname(), false, 1); // turned grey
        let zone = FakeZone::with(live);
        let h = handler(
            jobs,
            zone.clone(),
            FakeAccess::with(account_as_declared()),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();
        assert_eq!(
            zone.writes(),
            vec![format!(
                "update rec-boss.algedonic.dev-CNAME {} proxied=true",
                tunnel_cname()
            )]
        );
        let w = writes(&captured);
        assert_eq!(
            w.len(),
            2,
            "corrected, re-read, matched: the merge and the flip, no alarm: {w:?}"
        );
        let body = step_put(&w);
        assert_eq!(body["result"], "match");
        assert_eq!(boss_verdict(&body)["verdict"], "MATCH");
        assert_eq!(
            body["applied"],
            json!(["boss.algedonic.dev CNAME corrected"])
        );
    }

    #[tokio::test]
    async fn a_second_drifted_reading_refreshes_the_open_alarm_never_twins_it() {
        let open = vec![json!({
            "id": "alarm-1", "kind": "backlog-item", "status": "open",
            "metadata": {"estate_finding": "dns_drift:algedonic.dev"},
        })];
        let (jobs, captured) = stub_jobs_api("ready", open, LOCATION).await;
        let live = vec![as_declared()[0].clone()]; // playground. ABSENT
        let h = handler(
            jobs,
            FakeZone::with(live),
            FakeAccess::with(account_as_declared()),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();

        let w = writes(&captured);
        let order: Vec<&str> = w.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "PATCH /api/jobs/alarm-1/metadata",
                MERGE_OBSERVE,
                PUT_OBSERVE
            ]
        );
        let patch = &w[0].1;
        assert_eq!(patch["latest_observation"], "obs-1");
        assert_eq!(patch["findings"][0]["verdict"], "ABSENT");
        assert_eq!(
            patch["findings"][0]["record"],
            "playground.algedonic.dev CNAME"
        );
    }

    /// THE DEDUP READS ONLY ESTATE PACKETS (backlog c5ac71de). Unfiltered,
    /// the read counted every open backlog-item against a 200-row page;
    /// open items passed 200 in September 2026 (370 on 2026-09-26), so
    /// the first drift after that would have been HELD as a truncated
    /// dedup, never raised. The stub answers as the jobs API does, behind
    /// more unrelated open items than the page holds, with the zone's
    /// standing alarm last: a drift must find it and refresh it — neither
    /// held nor twinned.
    #[tokio::test]
    async fn a_backlog_past_one_page_does_not_hold_a_dns_drift() {
        use crate::handlers::listing_stub::UNRELATED_BACKLOG;
        const {
            assert!(
                UNRELATED_BACKLOG > DEDUP_PAGE,
                "the fixture outgrows a page"
            )
        };
        let open = vec![
            json!({"id": "alarm-other", "kind": "backlog-item", "status": "open",
                   "metadata": {"estate_finding": "sensor_unreadable:stripe-sponsorships"}}),
            json!({"id": "alarm-1", "kind": "backlog-item", "status": "open",
                   "metadata": {"estate_finding": "dns_drift:algedonic.dev"}}),
        ];
        let (jobs, captured) = stub_jobs_api("ready", open, LOCATION).await;
        let live = vec![as_declared()[0].clone()]; // playground. ABSENT
        let h = handler(
            jobs,
            FakeZone::with(live),
            FakeAccess::with(account_as_declared()),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx())
            .await
            .expect("a drift must reach its alarm, not be held");
        let w = writes(&captured);
        let order: Vec<&str> = w.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "PATCH /api/jobs/alarm-1/metadata",
                MERGE_OBSERVE,
                PUT_OBSERVE
            ]
        );
    }

    #[tokio::test]
    async fn an_access_drift_alone_raises_the_alarm_with_both_values() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let mut apps = account_as_declared();
        let pg = apps
            .iter()
            .position(|a| a.domain == "playground.algedonic.dev")
            .unwrap();
        apps[pg].session_duration = "720h".into();
        let h = handler(
            jobs,
            FakeZone::with(as_declared()),
            FakeAccess::with(apps),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();
        let w = writes(&captured);
        let order: Vec<&str> = w.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(order, vec!["POST /api/jobs", MERGE_OBSERVE, PUT_OBSERVE]);
        let findings = w[0].1["metadata"]["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["application"], "playground.algedonic.dev");
        assert_eq!(findings[0]["declared"]["session_duration"], "24h");
        assert_eq!(findings[0]["live"]["session_duration"], "720h");
        assert_eq!(step_put(&w)["result"], "findings");
    }

    // ----- refusals -----

    #[tokio::test]
    async fn a_zone_that_cannot_be_read_fails_the_firing_and_writes_nothing() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let h = handler(
            jobs,
            FakeZone::dark("GET /zones?name=algedonic.dev returned 403"),
            FakeAccess::with(account_as_declared()),
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
    async fn an_account_that_cannot_be_read_fails_the_firing_before_the_zone_is_read() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_measured());
        let h = handler(
            jobs,
            zone.clone(),
            FakeAccess::dark("GET /accounts/acct-1/access/apps returned 403"),
            secrets(),
            declarations(),
        );
        let err = h.invoke(&zone_args(), &ctx()).await.unwrap_err();
        assert!(
            matches!(&err, HandlerError::Downstream(m) if m.contains("access/apps")),
            "{err:?}"
        );
        assert_eq!(*zone.reads.lock().unwrap(), 0);
        assert!(writes(&captured).is_empty());
    }

    #[tokio::test]
    async fn no_installed_tunnel_id_fails_the_firing_naming_the_secret() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_declared());
        let access = FakeAccess::with(account_as_declared());
        let h = handler(
            jobs,
            zone.clone(),
            access.clone(),
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
            *zone.reads.lock().unwrap() + *access.reads.lock().unwrap(),
            0,
            "no Cloudflare read spent on a declaration it cannot judge"
        );
        assert!(writes(&captured).is_empty());
    }

    #[tokio::test]
    async fn a_credential_row_that_names_no_secret_is_a_permanent_refusal() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], "a laptop, somewhere").await;
        let h = handler(
            jobs,
            FakeZone::with(as_declared()),
            FakeAccess::with(account_as_declared()),
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
        let h = handler(
            jobs,
            FakeZone::with(as_declared()),
            FakeAccess::with(account_as_declared()),
            secrets(),
            None,
        );
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
            FakeZone::with(as_declared()),
            FakeAccess::with(account_as_declared()),
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
    async fn a_directory_without_the_access_declaration_refuses_permanently_naming_it() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let dir = boss_testing::scratch_dir("dns-observe-no-access-toml");
        let shipped = boss_testing::repo_root().join("infra/cluster/dns");
        for f in [COMPARATOR, "algedonic.dev.toml"] {
            std::fs::copy(shipped.join(f), dir.join(f)).unwrap();
        }
        let zone = FakeZone::with(as_declared());
        let access = FakeAccess::with(account_as_declared());
        let h = handler(
            jobs,
            zone.clone(),
            access.clone(),
            secrets(),
            Some(dir.display().to_string()),
        );
        let err = h.invoke(&zone_args(), &ctx()).await.unwrap_err();
        assert!(
            matches!(&err, HandlerError::Permanent(m) if m.contains(ACCESS_DECLARATION)),
            "{err:?}"
        );
        assert_eq!(
            *zone.reads.lock().unwrap() + *access.reads.lock().unwrap(),
            0
        );
        assert!(writes(&captured).is_empty());
    }

    #[tokio::test]
    async fn a_redelivery_on_a_completed_step_does_nothing() {
        let (jobs, captured) = stub_jobs_api("completed", vec![], LOCATION).await;
        let zone = FakeZone::with(as_declared());
        let h = handler(
            jobs,
            zone.clone(),
            FakeAccess::with(account_as_declared()),
            secrets(),
            declarations(),
        );
        h.invoke(&zone_args(), &ctx()).await.unwrap();
        assert_eq!(*zone.reads.lock().unwrap(), 0);
        assert!(writes(&captured).is_empty());
    }

    #[tokio::test]
    async fn a_task_step_of_another_kind_of_packet_is_skipped() {
        let (jobs, captured) = stub_jobs_api("ready", vec![], LOCATION).await;
        let zone = FakeZone::with(as_declared());
        let h = handler(
            jobs,
            zone.clone(),
            FakeAccess::with(account_as_declared()),
            secrets(),
            declarations(),
        );
        let mut c = ctx();
        c.event_payload["kind"] = json!("credential-rotation");
        h.invoke(&zone_args(), &c).await.unwrap();
        assert_eq!(*zone.reads.lock().unwrap(), 0);
        assert!(writes(&captured).is_empty());
    }
}
