//! `credential.rotate.forgejo` — the credential broker's first
//! rotation handler (packet 7ee101aa, first leg).
//!
//! Rotates a Forgejo access token end-to-end as MACHINE steps of a
//! `rotate-a-credential` packet: mint a replacement via the issuer's
//! admin API, install it into the named k8s Secret its consumers
//! mount, verify by effect (the new token can read the repo it
//! exists for), revoke the old token, and record each phase as the
//! completion of the packet's own `issue` / `install` / `verify` /
//! `revoke` steps — so the audit trail IS the packet, with the rule
//! as actor. THE SECRET VALUE NEVER ENTERS A PACKET — OR AN EVENT:
//! every evidence field records an identifier (token name/id, secret
//! path, value length) or an observed effect, never the value.
//!
//! ## The four rotation events
//!
//! Each phase also lands a domain event at the moment it becomes
//! true — `credential.minted` / `.installed` / `.verified` /
//! `.revoked` — via the registry's rotation door
//! (`POST /api/credentials/{id}/rotation/{phase}`, the census-door
//! precedent: handlers own no database). The maiden rotation left NO
//! events; its provenance existed only as step metadata, findable by
//! archaeology rather than by kind, and a never-emitted kind is
//! invisible to the audit-integrity checker — only design review
//! caught it. The install phase is also what stamps the registry
//! row's `rotated_at`, in the door's own transaction.
//!
//! Emission is guarded so a redelivery after a FINISHED rotation
//! emits nothing (see Idempotence below): `minted` and `installed`
//! fire when the mint/install ran in THIS invocation; the converge
//! path re-emits only `installed` (marked `converged: true`) and only
//! while the packet's `install` step is still unrecorded — a death
//! before the mint event forces a re-mint (the Secret cannot match),
//! so a lost `minted` is structurally impossible. `verified` and
//! `revoked` are gated on their steps the same way. The one residue:
//! a run that died between recording an event and completing its step
//! may re-emit that phase on replay, marked as convergence — an
//! at-least-once trace, never a lost one.
//!
//! Fired by a rule on `step.done.credential-rotation` — a dedicated
//! StepType (the `gate-verdict` precedent) so the rule targets
//! exactly the scope step of a rotation packet and never fires on
//! an ordinary `task`. The rule row carries the credential's
//! consumer declaration as args (which Secret, which user, which
//! scopes, which repo proves it) — per-credential registry data,
//! not code.
//!
//! ## Idempotence (per rotation packet)
//!
//! JetStream is at-least-once, so the whole flow re-runs safely.
//! The minted token's name is derived from the packet id
//! (`rotation_token_name`), which makes the issuer the idempotence
//! ledger:
//!   - name absent            → mint fresh.
//!   - name present AND the installed Secret's last-eight matches
//!     that token → the rotation already happened; converge the
//!     remaining phases (verify / revoke / step evidence) only.
//!   - name present but the Secret does NOT match → a previous
//!     attempt minted and then died before installing; the value is
//!     unrecoverable (Forgejo returns the sha1 exactly once), so
//!     delete the orphan by its ledger id and mint again.
//!
//! Order is the protocol: issue, install, verify, THEN revoke — the
//! destructive call runs last and only after the new credential is
//! proven working, exactly as the workflow's own description demands.
//!
//! ## Off-host delivery, and the old token named by its last eight
//!
//! Design 1c90d183 (David, 2026-09-26) adds a credential whose consumer
//! is NOT a mount: the forge host's checkout token, which the host's
//! deposit (`infra/forge/credential-deposit.sh`) takes from the Secret on
//! its next converge pass. The rule declares that as `delivery =
//! "off-host"`, and then "proven working" is not enough to revoke: the
//! old value is still what every consumer on the host reads until the
//! deposit runs. So the scope firing stops after verify, and the host
//! completes the packet's `delivered` step (`credential-delivery`) with
//! the last eight of the value it installed; a second rule fires this
//! handler on that step, which re-plans (the Secret already holds this
//! packet's token — a delivery firing never mints), checks the host's
//! last eight against the Secret's, and only then revokes.
//!
//! The scoper may name the old token by `old_token_last_eight` — the
//! identifier the forge shows beside each token, computed on the host so
//! nobody has to hold the name or the value. It is resolved against the
//! issuer's ledger ONCE, and refused when it matches no token, several,
//! this rotation's own replacement, or a token the rule's `spare`
//! declares to another consumer — before anything is minted. The name it
//! resolved to is recorded on the packet (`old_token_resolved`), and
//! every later firing judges the old token by that name: absent from the
//! ledger is a token already revoked, so a retry after the DELETE lands
//! its event and its step, and a redelivered finished rotation is a
//! no-op (review F1 of car 85b7b55f — re-resolving on every firing made
//! the retry refuse a last eight that no longer matched anything).
//! That record is ordinary job metadata, so it is never trusted as
//! written: every firing judges it against the ledger at the resolve
//! point and again before the DELETE, and an absent name reads as
//! "already revoked" only while no live token but the replacement ends
//! in its eight (`judge_recorded`, review F1b of the re-review of
//! 5ef6db0b). Forging or deleting it can make a rotation refuse; it
//! cannot make one record a revoke of a token that is still live.
//!
//! Round 3 of that review (of 3ee1bb0c) found the three ways the judgement
//! could still be fooled, and each is now closed where it lived:
//!   - F1c: the ledger was one page of fifty. `list_tokens` now reads the
//!     forge's listing to an empty page and refuses a partial one, so every
//!     judgement — the resolve, the record's, the after-DELETE absence —
//!     reads the whole ledger.
//!   - F1d: the DELETE put a string from metadata into the URL path, where
//!     `../../../../repos/david/boss` reached the repository with the
//!     broker's admin root token — and on main the scope step's `old_token`
//!     did the same. The revoke now deletes a LIVE LEDGER ROW by its
//!     numeric id (`revoke_target`), and the issuer port takes nothing
//!     else; a reference no row answers to is simply absent.
//!   - F1e: a last eight shared by two live tokens named whichever the
//!     record said. It now names exactly one, or nothing is revoked by it.

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use boss_jobs::credentials::RotationPhase;
use serde_json::{Value as JsonValue, json};
use std::sync::Arc;

use super::common::{StepEvent, dispatcher_reader_header};
use super::credential_issuer::{ForgeTokenIssuer, SecretStore, TokenInfo};

// ---------------------------------------------------------------------------
// Pure planning — the decision under test
// ---------------------------------------------------------------------------

/// The minted token's name: `{secret_name}-{first 8 of the packet id}`.
/// The packet id is the rotation's identity, so the name is the
/// idempotence key; the secret-name prefix keeps the issuer's token
/// list legible ("what is this token for" answers itself).
pub fn rotation_token_name(secret_name: &str, job_id: &str) -> String {
    let short: String = job_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    format!("{secret_name}-{short}")
}

/// Last eight characters, the granularity Forgejo's ledger exposes.
pub fn last_eight(s: &str) -> &str {
    let n = s.chars().count();
    if n <= 8 {
        s
    } else {
        let start = s
            .char_indices()
            .nth(n - 8)
            .map(|(i, _)| i)
            .unwrap_or_default();
        &s[start..]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationPlan {
    /// This packet's token exists and the Secret holds it: the
    /// mint+install already happened. Converge the rest only.
    AlreadyInstalled { token_id: i64 },
    /// This packet's token exists but the Secret does not hold it: a
    /// prior attempt lost the value. Delete the orphan (by the id its
    /// ledger row carries), mint again.
    ReplaceStale { orphan_id: i64 },
    /// Nothing from this packet on the issuer yet.
    MintFresh,
}

pub fn plan_rotation(
    existing: &[TokenInfo],
    installed_last8: Option<&str>,
    token_name: &str,
) -> RotationPlan {
    match existing.iter().find(|t| t.name == token_name) {
        None => RotationPlan::MintFresh,
        Some(t) => match installed_last8 {
            Some(l8) if l8 == t.token_last_eight => {
                RotationPlan::AlreadyInstalled { token_id: t.id }
            }
            _ => RotationPlan::ReplaceStale { orphan_id: t.id },
        },
    }
}

/// How the credential reaches its consumers once it is in the Secret —
/// the rule's optional `delivery` arg (design 1c90d183, D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Consumers read the Secret itself (a mount, `boss credential
    /// pull`): the value is delivered the moment it is installed, so
    /// the revoke follows the verify in the same firing. Today's order,
    /// and the default when the rule says nothing.
    Mount,
    /// A consumer OFF the cluster takes the value from the Secret on
    /// its own schedule — the forge host's deposit, up to one converge
    /// period later. Revoking in that gap breaks every consumer until
    /// the next pass, so the revoke waits for the host to complete the
    /// packet's `delivered` step with the last eight it installed.
    OffHost,
}

impl Delivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mount => "mount",
            Self::OffHost => "off-host",
        }
    }
}

/// The rule's `delivery` arg: absent or `mount` is today's order,
/// `off-host` waits for delivery; anything else is an authoring fault.
fn delivery_arg(args: &[(String, Value)]) -> Result<Delivery, HandlerError> {
    match arg_string(args, "delivery") {
        Err(HandlerError::MissingArg(_)) => Ok(Delivery::Mount),
        Err(e) => Err(e),
        Ok("mount") => Ok(Delivery::Mount),
        Ok("off-host") => Ok(Delivery::OffHost),
        Ok(other) => Err(HandlerError::Permanent(format!(
            "rule arg delivery = {other:?} is neither \"mount\" nor \"off-host\""
        ))),
    }
}

/// The old token named by its LAST EIGHT — the identifier Forgejo shows
/// beside each token's name, computed on the host that holds the value
/// so the value never travels (design 1c90d183, D3). Exactly one token
/// in the issuer's ledger must end in it, and it must not be this
/// rotation's own replacement: zero matches is a typo or a token already
/// gone, several is a guess, and the replacement is the one token that
/// must survive. Each refusal names what it saw, never a value.
pub fn resolve_last_eight<'a>(
    tokens: &'a [TokenInfo],
    last8: &str,
    replacement: &str,
) -> Result<&'a TokenInfo, String> {
    let matches: Vec<&TokenInfo> = tokens
        .iter()
        .filter(|t| t.token_last_eight == last8)
        .collect();
    match matches.as_slice() {
        [] => Err(format!(
            "no token in the issuer's ledger ends in {last8}; nothing to revoke by that name"
        )),
        [one] if one.name == replacement => Err(format!(
            "the token ending in {last8} is {replacement}, this rotation's own replacement"
        )),
        [one] => Ok(one),
        many => Err(format!(
            "{} tokens end in {last8} ({}); name the old one by old_token instead",
            many.len(),
            many.iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The packet-level metadata key that records what the scope step's
/// `old_token_last_eight` resolved to — `{name, id, last_eight}`,
/// identifiers only. Written once, before anything is minted; read by
/// every later firing, which then judges the old token by NAME (review
/// F1 of car 85b7b55f, 2026-09-26).
pub const OLD_TOKEN_RESOLVED_KEY: &str = "old_token_resolved";

/// What a last eight resolved to, as the packet records it under
/// [`OLD_TOKEN_RESOLVED_KEY`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOld {
    pub name: String,
    pub id: i64,
    pub last_eight: String,
}

impl ResolvedOld {
    pub fn from_json(v: &JsonValue) -> Option<Self> {
        Some(Self {
            name: v.get("name")?.as_str()?.to_string(),
            id: v.get("id")?.as_i64()?,
            last_eight: v.get("last_eight")?.as_str()?.to_string(),
        })
    }

    pub fn to_json(&self) -> JsonValue {
        json!({ "name": self.name, "id": self.id, "last_eight": self.last_eight })
    }
}

/// A recorded resolution judged against the issuer's ledger as it
/// stands NOW. The record is ordinary job metadata — the filer, or
/// anyone with job write, can set or delete it — so it is a claim, never
/// a fact (review F1b of car 85b7b55f, re-review of 5ef6db0b,
/// 2026-09-26: a pre-seeded `{name: "no-such-token", last_eight:
/// "1eaked01"}` recorded a revoke that never happened while the token
/// ending 1eaked01 stayed live). What the ledger itself says decides:
///
/// - the recorded name is LISTED: it must still end in the recorded
///   eight, or the name has come to mean some other token;
/// - the recorded name is ABSENT: that reads as "already revoked" only
///   while no live token but this rotation's replacement ends in the
///   recorded eight. The honest case is a retry after the DELETE, where
///   that holds; a stale or forged record naming an absent token while
///   the token the scoper named is live is refused, naming both.
///
/// So forging or deleting the record can only make a rotation refuse;
/// it can never make one record a revoke of a token that is still live.
///
/// And a last eight names ONE token or none (round-3 review, F1e, of
/// 3ee1bb0c): if more than one live token other than the replacement
/// ends in it, nothing is revoked by it, whichever of them the record
/// names — the scoper named the eight, not the name, so a record picking
/// one of two is a guess. A listed name must also still carry the id it
/// was recorded with, or the name has been reissued to another token.
///
/// Returns the live ledger row the record resolves to — the only thing
/// the revoke may DELETE, by its numeric id (F1d) — or `None` when the
/// named token is already gone.
pub fn judge_recorded<'a>(
    ledger: &'a [TokenInfo],
    recorded: &ResolvedOld,
    replacement: &str,
) -> Result<Option<&'a TokenInfo>, String> {
    let l8 = &recorded.last_eight;
    let carriers: Vec<&TokenInfo> = ledger
        .iter()
        .filter(|t| t.name != replacement && t.token_last_eight == *l8)
        .collect();
    let carrier_names = || {
        carriers
            .iter()
            .map(|t| format!("{} (id {})", t.name, t.id))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if carriers.len() > 1 {
        return Err(format!(
            "{} live tokens end in {l8} ({}); a last eight that names several tokens names \
             none, and the record's {} is a guess among them — old token NOT revoked. Rotate \
             by old_token naming the one meant",
            carriers.len(),
            carrier_names(),
            recorded.name
        ));
    }
    let Some(t) = ledger.iter().find(|t| t.name == recorded.name) else {
        if carriers.is_empty() {
            return Ok(None);
        }
        return Err(format!(
            "the packet records last eight {l8} as resolved to {}, which the issuer's ledger \
             does not list, yet {} still ends in {l8}; a record that names an absent token \
             while a live one carries its eight is stale or forged, and nothing is revoked by \
             it. Set job metadata {OLD_TOKEN_RESOLVED_KEY} = null to have the next firing \
             resolve the last eight afresh",
            recorded.name,
            carrier_names()
        ));
    };
    if t.name == replacement {
        return Err(format!(
            "the packet records last eight {l8} as resolved to {replacement}, this rotation's \
             own replacement; old token NOT revoked"
        ));
    }
    if t.token_last_eight != *l8 {
        return Err(format!(
            "token {} now ends in {}, not the {l8} the scope step named; old token NOT revoked",
            recorded.name, t.token_last_eight
        ));
    }
    if t.id != recorded.id {
        return Err(format!(
            "token {} is listed as id {}, not the id {} it was recorded with; the name now \
             means another token, and nothing is revoked by it",
            recorded.name, t.id, recorded.id
        ));
    }
    // Not the replacement and ends in the eight, so it is the one carrier.
    Ok(Some(t))
}

/// The rule's optional `spare` arg: token names this rotation must never
/// revoke, because another consumer is DECLARED to hold them — an exact
/// name, or a prefix ending in `*` for a broker credential whose
/// instances carry packet-derived suffixes. David, Q2 of design
/// 1c90d183: a leaked token no declared holder matches is revoked; one
/// declared to another consumer is not, and gets its own rotation.
pub fn parse_spare(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn is_spared(name: &str, spare: &[String]) -> bool {
    spare.iter().any(|s| match s.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == s,
    })
}

/// May the revoke run now?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevokeGate {
    Open,
    /// Off-host and the host has not recorded delivery yet.
    AwaitDelivery,
    /// The host recorded a delivery of something the Secret does not
    /// hold — a stale or foreign value; revoking would strand it.
    Refused(String),
}

pub fn revoke_gate(
    delivery: Delivery,
    delivered_l8: Option<&str>,
    installed_l8: &str,
) -> RevokeGate {
    match (delivery, delivered_l8) {
        (Delivery::Mount, _) => RevokeGate::Open,
        (Delivery::OffHost, None) => RevokeGate::AwaitDelivery,
        (Delivery::OffHost, Some(d)) if d == installed_l8 => RevokeGate::Open,
        // A completed step cannot be re-recorded, so this packet cannot
        // finish: the way out is its `abandoned` terminal and a fresh
        // rotation, and the refusal says so rather than leave the reader
        // to derive it. The deposit re-reads the Secret after it finds
        // the packet and records nothing when the value moved under it
        // (review F3), so a mismatch here is not the ordinary race.
        (Delivery::OffHost, Some(d)) => RevokeGate::Refused(format!(
            "the host recorded delivery of a value ending in {d}, but the Secret holds one \
             ending in {installed_l8}; old token NOT revoked. A completed `delivered` step \
             cannot be recorded again: set job metadata abandoned = \"true\" to close this \
             packet on its `abandoned` terminal, and open a new rotation"
        )),
    }
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

pub struct CredentialRotateForgejo {
    client: reqwest::Client,
    jobs_base: String,
    issuer: Arc<dyn ForgeTokenIssuer>,
    secrets: Arc<dyn SecretStore>,
}

/// One step of the rotation packet as the jobs-api lists it. Its
/// metadata is READ, never written back: a completion merges its own
/// fields through the step merge door (e39a9d2a). The delivery firing
/// reads the scope step's fields and the `delivered` step's evidence
/// from here, because the event that fired it is the delivered step's.
struct StepView {
    id: String,
    status: String,
    metadata: serde_json::Map<String, JsonValue>,
}

/// A non-empty trimmed string field of a step's metadata.
fn meta_str<'a>(m: &'a serde_json::Map<String, JsonValue>, key: &str) -> Option<&'a str> {
    m.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// What the scope step names for revocation, judged against the issuer's
/// WHOLE ledger as it stands: by `old_token` (a name or numeric id), by
/// `old_token_last_eight` (already resolved: [`ResolvedOld`], judged by
/// [`judge_recorded`]), or both, which must then agree.
///
/// Returns the LIVE ROW to revoke, or `None` when there is nothing live to
/// revoke — nothing named, or the named token already gone. The revoke
/// deletes that row by its numeric id and by nothing else: a string from
/// the scope step or the packet's metadata never reaches the forge (round-3
/// review of car 85b7b55f, F1d — on main the scope step's `old_token` went
/// into the DELETE path as spelled, so `../../../../repos/david/boss`
/// there was a DELETE of the repository, signed with the broker's admin
/// root token). A reference no row answers to is simply absent.
///
/// Called before any side effect — a self-naming, spared or ambiguous
/// target refuses the whole rotation, so a typo costs nothing rather than
/// a minted token that can never finish — and again on a fresh listing
/// right before the DELETE.
fn revoke_target<'a>(
    ledger: &'a [TokenInfo],
    old_token: Option<&str>,
    by_last8: Option<&ResolvedOld>,
    replacement: &str,
    spare: &[String],
) -> Result<Option<&'a TokenInfo>, String> {
    // By name, before the replacement exists to be listed: the first
    // firing refuses it before the mint.
    if old_token == Some(replacement) || by_last8.is_some_and(|r| r.name == replacement) {
        return Err(format!(
            "the old token is named as {replacement}, this rotation's own replacement"
        ));
    }
    let row = match (old_token, by_last8) {
        (None, None) => return Ok(None),
        (Some(n), Some(r)) if n != r.name && n != r.id.to_string() => {
            return Err(format!(
                "old_token {n:?} and old_token_last_eight {} name different tokens ({})",
                r.last_eight, r.name
            ));
        }
        (_, Some(r)) => judge_recorded(ledger, r, replacement)?,
        (Some(n), None) => {
            let rows: Vec<&TokenInfo> = ledger
                .iter()
                .filter(|t| t.name == n || t.id.to_string() == n)
                .collect();
            match rows.as_slice() {
                [] => None,
                [one] => Some(*one),
                many => {
                    return Err(format!(
                        "old_token {n:?} answers to {} tokens ({}): a name of one and the id \
                         of another; name the old token unambiguously",
                        many.len(),
                        many.iter()
                            .map(|t| format!("{} (id {})", t.name, t.id))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }
    };
    let Some(t) = row else {
        return Ok(None);
    };
    if t.name == replacement {
        return Err(format!(
            "old token {} (id {}) is this rotation's own replacement",
            t.name, t.id
        ));
    }
    if is_spared(&t.name, spare) {
        return Err(format!(
            "old token {} is declared to another consumer (the rule's `spare`); \
             this rotation does not revoke it — rotate that credential first",
            t.name
        ));
    }
    Ok(Some(t))
}

impl CredentialRotateForgejo {
    pub fn new(
        jobs_base: impl Into<String>,
        issuer: Arc<dyn ForgeTokenIssuer>,
        secrets: Arc<dyn SecretStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: super::common::api_client(),
            jobs_base: jobs_base.into(),
            issuer,
            secrets,
        })
    }

    fn jobs(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// The packet's steps keyed by spec slug, and its own metadata. One
    /// read serves every completion below.
    async fn fetch_packet(
        &self,
        job_id: &str,
    ) -> Result<
        (
            std::collections::HashMap<String, StepView>,
            serde_json::Map<String, JsonValue>,
        ),
        HandlerError,
    > {
        let url = format!("{}/api/jobs/{job_id}", self.jobs());
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", super::common::sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status}"
            )));
        }
        let body: JsonValue = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("{url}: {e}")))?;
        let mut out = std::collections::HashMap::new();
        for s in body
            .get("steps")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let (Some(slug), Some(id), Some(status)) = (
                s.get("spec_slug").and_then(|v| v.as_str()),
                s.get("id").and_then(|v| v.as_str()),
                s.get("status").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            out.insert(
                slug.to_string(),
                StepView {
                    id: id.to_string(),
                    status: status.to_string(),
                    metadata: s
                        .get("metadata")
                        .and_then(|v| v.as_object())
                        .cloned()
                        .unwrap_or_default(),
                },
            );
        }
        let job_meta = body
            .get("metadata")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        Ok((out, job_meta))
    }

    /// Record on the packet what the scope's last eight resolved to,
    /// through the job merge door — once, before anything is minted, so
    /// every later firing (a retry after the forge DELETE, a delivery
    /// firing, a redelivery of a finished rotation) judges the old token
    /// by this NAME and reads its absence as "already revoked" rather
    /// than as a last eight that matches nothing (review F1).
    async fn record_resolution(
        &self,
        rule_name: &str,
        job_id: &str,
        resolved: &ResolvedOld,
    ) -> Result<(), HandlerError> {
        super::common::write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{}/api/jobs/{job_id}/metadata", self.jobs()),
            &json!({ OLD_TOKEN_RESOLVED_KEY: resolved.to_json() }),
            rule_name,
        )
        .await
    }

    /// Complete one packet step with evidence fields: the fields
    /// through the step merge door, then the status alone
    /// (`common::complete_step`, backlog e39a9d2a) — this merged them
    /// into the step's metadata as read and PUT the whole map back,
    /// which the step PUT refuses once anything wrote the step in
    /// between, and refuses outright in the decided end state.
    /// Already-completed steps are left alone — that is the redelivery
    /// path. A slug the packet lacks is skipped: the packet's workflow
    /// version decides which phases it records.
    async fn complete_step(
        &self,
        rule_name: &str,
        job_id: &str,
        steps: &std::collections::HashMap<String, StepView>,
        slug: &str,
        evidence: &[(&str, String)],
    ) -> Result<(), HandlerError> {
        let Some(step) = steps.get(slug) else {
            tracing::warn!(job_id, slug, "rotation packet has no such step; skipping");
            return Ok(());
        };
        if step.status == "completed" {
            return Ok(());
        }
        let fields = evidence
            .iter()
            .map(|(k, v)| ((*k).to_string(), json!(v)))
            .collect();
        super::common::complete_step(
            &self.client,
            self.jobs(),
            job_id,
            &step.id,
            fields,
            rule_name,
        )
        .await
    }

    /// Land one rotation phase on the log through the registry's
    /// rotation door. The door injects `credential_id` from the path,
    /// records the `credential.<phase>` event, and on the install
    /// phase stamps the row's `rotated_at`. Evidence is identifiers
    /// and observed effects only — never a value.
    async fn record_phase(
        &self,
        rule_name: &str,
        credential_id: &str,
        phase: RotationPhase,
        evidence: JsonValue,
    ) -> Result<(), HandlerError> {
        let url = format!(
            "{}/api/credentials/{credential_id}/rotation/{}",
            self.jobs(),
            phase.as_str()
        );
        super::common::post_json(&self.client, &url, &evidence, rule_name).await
    }
}

#[async_trait]
impl Handler for CredentialRotateForgejo {
    fn name(&self) -> &'static str {
        "credential.rotate.forgejo"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let ev = StepEvent::from_payload(&ctx.event_payload)?;

        // The credential's consumer declaration — rule-row data.
        let forge_user = arg_string(args, "forge_user")?;
        let secret_namespace = arg_string(args, "secret_namespace")?;
        let secret_name = arg_string(args, "secret_name")?;
        let secret_key = arg_string(args, "secret_key")?;
        let scopes: Vec<String> = arg_string(args, "scopes")?
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let verify_repo = arg_string(args, "verify_repo")?;
        let delivery = delivery_arg(args)?;
        let spare = match arg_string(args, "spare") {
            Err(HandlerError::MissingArg(_)) => Vec::new(),
            Err(e) => return Err(e),
            Ok(raw) => parse_spare(raw),
        };

        // WHICH STEP FIRED. The scope step (`credential-rotation`)
        // starts a rotation; for an off-host credential the host's
        // `delivered` step (`credential-delivery`) finishes it — the
        // second firing re-plans, finds its token installed, and runs
        // only what is left: the revoke (design 1c90d183, D1).
        let delivery_firing = ev.kind == "credential-delivery";
        if delivery_firing && delivery == Delivery::Mount {
            return Err(HandlerError::Permanent(format!(
                "a `credential-delivery` step fired {}, whose credential is delivered by \
                 mount — there is no delivery to wait for; declare delivery = \"off-host\" \
                 on the rule or remove the step",
                ctx.rule_name
            )));
        }

        let token_name = rotation_token_name(secret_name, ev.job_id);

        // One read serves the event-emission guards here AND the step
        // completions at the end. A slug's `completed` status is the
        // recording ledger both share: a phase whose step is recorded
        // had its event recorded first (events land before steps), so
        // a completed step means "already on the log — do not re-emit".
        let (steps, job_meta) = self.fetch_packet(ev.job_id).await?;
        let step_done = |slug: &str| steps.get(slug).is_some_and(|s| s.status == "completed");

        // The scope step's fields: off the event when the scope step
        // fired, off the packet when the delivered step did.
        let no_metadata = serde_json::Map::new();
        let scope_meta = if delivery_firing {
            steps.get("scope").map_or(&no_metadata, |s| &s.metadata)
        } else {
            ev.metadata
        };
        // Per-rotation facts off the scope step: the old token's NAME
        // OR ID, or its LAST EIGHT — never its value.
        let old_token = meta_str(scope_meta, "old_token");
        let old_last8 = meta_str(scope_meta, "old_token_last_eight");

        // The registry id the rotation events annotate — the scope
        // step's `credential` field (required at its completion), with
        // the packet Subject as the fallback (the rule's own `when`
        // matches on it). Resolved BEFORE any side effect: a rotation
        // that cannot name its credential cannot record what it did,
        // and that is an authoring fault, not a retryable one.
        let credential_id = meta_str(scope_meta, "credential").unwrap_or(ev.subject_id);
        if credential_id.is_empty() {
            return Err(HandlerError::Permanent(
                "rotation packet names no credential: neither scope metadata \
                 `credential` nor subject_id is set"
                    .into(),
            ));
        }

        // The host's delivery evidence: the last eight it installed,
        // read off its own file (identifier, never the value).
        //
        // WHO MAY RECORD IT is, today, whoever the step API admits to
        // complete a step on this packet: `credential-delivery` declares
        // no roles, and the deposit signs with a self-asserted
        // x-boss-user (review F4 of car 85b7b55f). What bounds a forged
        // record is this comparison — a value the Secret does not hold
        // revokes nothing — so its worst cost is a packet that must be
        // abandoned, or (a forger who knew the new last eight) the old
        // token revoked up to one converge pass early, which the next
        // deposit repairs because it owes nothing to the forge token.
        // Never a wrong token revoked: the target is the live ledger row
        // the scope's naming resolves to, deleted by its numeric id.
        // Binding the record to the host's machine identity is the step
        // API's work, not this handler's.
        let delivered_l8 = if delivery_firing {
            let l8 = meta_str(ev.metadata, "delivered_last_eight");
            if l8.is_none() {
                return Err(HandlerError::Permanent(
                    "the delivered step carries no delivered_last_eight; a delivery \
                     without its evidence is a claim, not a record"
                        .into(),
                ));
            }
            l8
        } else {
            steps
                .get("delivered")
                .filter(|s| s.status == "completed")
                .and_then(|s| meta_str(&s.metadata, "delivered_last_eight"))
        };

        // Plan against the issuer's ledger + the installed value.
        let existing = self
            .issuer
            .list_tokens(forge_user)
            .await
            .map_err(HandlerError::Downstream)?;
        let installed = self
            .secrets
            .read_key(secret_namespace, secret_name, secret_key)
            .await
            .map_err(HandlerError::Downstream)?;

        // The old token's last eight, resolved to a NAME exactly once.
        // The first firing resolves it against the ledger (refusing zero
        // matches, several, or the replacement) and records the name on
        // the packet; every later firing reads that record instead,
        // because once the DELETE has run the ledger no longer holds a
        // token ending in those eight — and a retry that re-resolved
        // then refused would strand a revoke that already happened
        // without its event or its step (review F1 of car 85b7b55f).
        let recorded = job_meta
            .get(OLD_TOKEN_RESOLVED_KEY)
            .and_then(ResolvedOld::from_json);
        let (resolved, newly_resolved) = match (old_last8, recorded) {
            (None, _) => (None, false),
            // A record is a claim anyone with job write can make: judged
            // against the ledger by `revoke_target` below, before anything
            // is minted, and again before the DELETE (review F1b).
            (Some(l8), Some(r)) if r.last_eight == l8 => (Some(r), false),
            (Some(l8), Some(r)) => {
                return Err(HandlerError::Permanent(format!(
                    "packet {} records last eight {} as resolved to {}, but its scope step \
                     names {l8}; nothing is revoked on a disagreement",
                    ev.job_id, r.last_eight, r.name
                )));
            }
            (Some(l8), None) => {
                let t = resolve_last_eight(&existing, l8, &token_name)
                    .map_err(HandlerError::Permanent)?;
                (
                    Some(ResolvedOld {
                        name: t.name.clone(),
                        id: t.id,
                        last_eight: l8.to_string(),
                    }),
                    true,
                )
            }
        };

        // The revoke target, judged before anything is minted. The row it
        // finds here is not kept: the DELETE is decided on a fresh listing.
        revoke_target(&existing, old_token, resolved.as_ref(), &token_name, &spare)
            .map_err(HandlerError::Permanent)?;
        let names_an_old_token = old_token.is_some() || resolved.is_some();
        if newly_resolved && let Some(r) = &resolved {
            self.record_resolution(&ctx.rule_name, ev.job_id, r).await?;
        }

        let plan = plan_rotation(&existing, installed.as_deref().map(last_eight), &token_name);
        if delivery_firing && !matches!(plan, RotationPlan::AlreadyInstalled { .. }) {
            return Err(HandlerError::Permanent(format!(
                "a delivery was recorded on packet {}, but Secret {secret_namespace}/\
                 {secret_name} does not hold its token {token_name}; a delivery firing \
                 never mints, and nothing is revoked",
                ev.job_id
            )));
        }

        // issue + install (or converge if a prior run already did),
        // each phase's event recorded at the moment it becomes true.
        let (token_id, token_value) = match plan {
            RotationPlan::AlreadyInstalled { token_id } => {
                // The Secret provably holds this packet's token; it
                // is the only remaining copy of the value. The mint
                // event cannot be missing (a death before it leaves
                // the Secret unmatched, which re-mints instead of
                // landing here), but a death between the Secret write
                // and the install event loses that record — the step
                // ledger says whether the recording tail ever ran.
                let value = installed.unwrap_or_default();
                if !step_done("install") {
                    self.record_phase(
                        &ctx.rule_name,
                        credential_id,
                        RotationPhase::Installed,
                        json!({
                            "job_id": ev.job_id,
                            "token_name": token_name,
                            "secret_namespace": secret_namespace,
                            "secret_name": secret_name,
                            "secret_key": secret_key,
                            "value_length": value.len(),
                            "converged": true,
                        }),
                    )
                    .await?;
                }
                (token_id, value)
            }
            RotationPlan::ReplaceStale { .. } | RotationPlan::MintFresh => {
                let replaced_orphan = matches!(plan, RotationPlan::ReplaceStale { .. });
                if let RotationPlan::ReplaceStale { orphan_id } = plan {
                    // Orphan from a died attempt; its value is gone
                    // for good, so retire it — by the id its ledger row
                    // carries — before re-minting.
                    self.issuer
                        .delete_token(forge_user, orphan_id)
                        .await
                        .map_err(HandlerError::Downstream)?;
                }
                let minted = self
                    .issuer
                    .create_token(forge_user, &token_name, &scopes)
                    .await
                    .map_err(HandlerError::Downstream)?;
                self.record_phase(
                    &ctx.rule_name,
                    credential_id,
                    RotationPhase::Minted,
                    json!({
                        "job_id": ev.job_id,
                        "token_name": token_name,
                        "token_id": minted.id,
                        "forge_user": forge_user,
                        "scopes": scopes.clone(),
                        "replaced_orphan": replaced_orphan,
                    }),
                )
                .await?;
                self.secrets
                    .write_key(secret_namespace, secret_name, secret_key, &minted.sha1)
                    .await
                    .map_err(HandlerError::Downstream)?;
                self.record_phase(
                    &ctx.rule_name,
                    credential_id,
                    RotationPhase::Installed,
                    json!({
                        "job_id": ev.job_id,
                        "token_name": token_name,
                        "secret_namespace": secret_namespace,
                        "secret_name": secret_name,
                        "secret_key": secret_key,
                        "value_length": minted.sha1.len(),
                    }),
                )
                .await?;
                (minted.id, minted.sha1)
            }
        };

        // Verify by effect BEFORE anything destructive: the new
        // token must actually work at the thing it exists for.
        let readable = self
            .issuer
            .repo_readable_with(&token_value, verify_repo)
            .await
            .map_err(HandlerError::Downstream)?;
        if !readable {
            return Err(HandlerError::Downstream(format!(
                "verify-by-effect failed: token {token_name} cannot read {verify_repo}; \
                 old token NOT revoked"
            )));
        }
        if !step_done("verify") {
            self.record_phase(
                &ctx.rule_name,
                credential_id,
                RotationPhase::Verified,
                json!({
                    "job_id": ev.job_id,
                    "token_name": token_name,
                    "verify_repo": verify_repo,
                    "method": "api",
                }),
            )
            .await?;
        }

        // Revoke the old token — last, only what the scoper named, and
        // for an off-host credential only once the host has recorded
        // delivery of exactly the value the Secret holds.
        let mut revoke_evidence: Option<(String, String)> = None;
        let gate = revoke_gate(delivery, delivered_l8, last_eight(&token_value));
        if let RevokeGate::Refused(why) = &gate {
            return Err(HandlerError::Permanent(why.clone()));
        }
        let revoke_now = if gate == RevokeGate::AwaitDelivery {
            tracing::info!(
                job_id = ev.job_id,
                credential_id,
                "off-host delivery: the revoke waits for the host's `delivered` step"
            );
            false
        } else {
            names_an_old_token
        };
        if revoke_now {
            // Judged again against the WHOLE ledger as it stands NOW
            // (`revoke_target`): a recorded name that is absent, with no
            // live token but the replacement ending in the named eight, is
            // a token already revoked (a retry, a redelivery); present, it
            // must be the one live carrier of that eight under the id it
            // was recorded with (review F1b, round-3 F1e). What comes back
            // is a LEDGER ROW, deleted by its numeric id and nothing else
            // (round-3 F1d).
            let now = self
                .issuer
                .list_tokens(forge_user)
                .await
                .map_err(HandlerError::Downstream)?;
            let target = revoke_target(&now, old_token, resolved.as_ref(), &token_name, &spare)
                .map_err(HandlerError::Permanent)?
                .cloned();
            // What the scope named, for the record: the live row's name
            // when there is one, else the reference that is now absent.
            let label = match (&target, &resolved, old_token) {
                (Some(t), _, _) => t.name.clone(),
                (None, Some(r), _) => r.name.clone(),
                (None, None, Some(n)) => n.to_string(),
                (None, None, None) => String::new(),
            };
            let deleted = match &target {
                Some(t) => {
                    if t.id == token_id {
                        return Err(HandlerError::Permanent(format!(
                            "old token id {} is this rotation's own replacement",
                            t.id
                        )));
                    }
                    let deleted = self
                        .issuer
                        .delete_token(forge_user, t.id)
                        .await
                        .map_err(HandlerError::Downstream)?;
                    let after = self
                        .issuer
                        .list_tokens(forge_user)
                        .await
                        .map_err(HandlerError::Downstream)?;
                    if after.iter().any(|a| a.id == t.id) {
                        return Err(HandlerError::Downstream(format!(
                            "old token {} (id {}) still present after delete",
                            t.name, t.id
                        )));
                    }
                    deleted
                }
                None => false,
            };
            let confirmed_dead = match (&target, &resolved) {
                (Some(t), _) => format!(
                    "issuer token list for {forge_user} no longer contains {} (id {})",
                    t.name, t.id
                ),
                (None, Some(r)) => format!(
                    "issuer token list for {forge_user} no longer contains {label}, and no \
                     token but the replacement ends in {}",
                    r.last_eight
                ),
                (None, None) => {
                    format!("issuer token list for {forge_user} no longer contains {label}")
                }
            };
            // The event records the confirmed absence, whichever run
            // performed the deletion — `deleted_now` says which kind
            // of observation this record is.
            if !step_done("revoke") {
                self.record_phase(
                    &ctx.rule_name,
                    credential_id,
                    RotationPhase::Revoked,
                    json!({
                        "job_id": ev.job_id,
                        "old_token": label,
                        "old_token_id": target.as_ref().map(|t| t.id),
                        "old_token_last_eight": old_last8,
                        "delivery": delivery.as_str(),
                        "delivered_last_eight": delivered_l8,
                        "deleted_now": deleted,
                        "confirmed_dead": confirmed_dead,
                    }),
                )
                .await?;
            }
            let named_by = old_last8
                .map(|l8| format!(" (named by its last eight, {l8})"))
                .unwrap_or_default();
            revoke_evidence = Some((
                match &target {
                    Some(t) if deleted => format!(
                        "forgejo token {label} (id {}){named_by} deleted via admin API",
                        t.id
                    ),
                    _ => format!("forgejo token {label}{named_by} was already absent"),
                },
                confirmed_dead,
            ));
        }

        // Record each phase as the packet's own steps (fetched once,
        // above — the same read the emission guards used). The step
        // PUTs carry the human-facing evidence: actor = this rule,
        // identifiers and observed effects, never a value.
        self.complete_step(
            &ctx.rule_name,
            ev.job_id,
            &steps,
            "issue",
            &[
                (
                    "issued",
                    format!(
                        "forgejo token {token_name} (id {token_id}) minted via \
                         POST /api/v1/admin/users/{forge_user}/tokens"
                    ),
                ),
                (
                    "issuer",
                    format!(
                        "credential-broker ({}), root credential boss-credential-broker-root",
                        ctx.rule_name
                    ),
                ),
            ],
        )
        .await?;
        self.complete_step(
            &ctx.rule_name,
            ev.job_id,
            &steps,
            "install",
            &[
                (
                    "installed",
                    format!(
                        "k8s Secret {secret_namespace}/{secret_name} key {secret_key} \
                         updated ({} bytes); {}",
                        token_value.len(),
                        match delivery {
                            Delivery::Mount => "consumers pick it up from their mounts",
                            Delivery::OffHost =>
                                "the host's deposit takes it on its next pass and records \
                                 delivery on this packet; the revoke waits for that",
                        }
                    ),
                ),
                ("permissions", scopes.join(",")),
                // What the protocol's `delivered` step gates on.
                ("delivery", delivery.as_str().to_string()),
            ],
        )
        .await?;
        self.complete_step(
            &ctx.rule_name,
            ev.job_id,
            &steps,
            "verify",
            &[
                (
                    "verified",
                    format!("GET /api/v1/repos/{verify_repo} authenticated with the new token"),
                ),
                ("method", "api".to_string()),
            ],
        )
        .await?;
        if let Some((revoked, confirmed_dead)) = revoke_evidence {
            self.complete_step(
                &ctx.rule_name,
                ev.job_id,
                &steps,
                "revoke",
                &[("revoked", revoked), ("confirmed_dead", confirmed_dead)],
            )
            .await?;
        } else if gate == RevokeGate::Open {
            tracing::info!(
                job_id = ev.job_id,
                "no old_token named on the scope step; revoke left to its assignee"
            );
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::credential_issuer::MintedToken;
    use std::collections::HashMap;
    use std::sync::Mutex;

    // ----- pure planning -----

    #[test]
    fn token_name_derives_from_secret_and_packet_id() {
        assert_eq!(
            rotation_token_name("boss-dev-forge-token", "7ee101aa-3267-4745"),
            "boss-dev-forge-token-7ee101aa"
        );
    }

    #[test]
    fn last_eight_handles_short_and_long() {
        assert_eq!(last_eight("abc"), "abc");
        assert_eq!(last_eight("0123456789abcdef"), "89abcdef");
    }

    fn tok(id: i64, name: &str, last8: &str) -> TokenInfo {
        TokenInfo {
            id,
            name: name.into(),
            token_last_eight: last8.into(),
        }
    }

    #[test]
    fn plan_mints_fresh_when_issuer_has_no_rotation_token() {
        let plan = plan_rotation(&[tok(1, "other", "aaaa1111")], None, "sec-7ee101aa");
        assert_eq!(plan, RotationPlan::MintFresh);
    }

    #[test]
    fn plan_recognizes_a_completed_install() {
        let plan = plan_rotation(
            &[tok(9, "sec-7ee101aa", "89abcdef")],
            Some("89abcdef"),
            "sec-7ee101aa",
        );
        assert_eq!(plan, RotationPlan::AlreadyInstalled { token_id: 9 });
    }

    #[test]
    fn plan_replaces_an_orphan_whose_value_was_lost() {
        // Token minted, process died before the Secret write: the
        // installed value (or its absence) does not match.
        let existing = [tok(9, "sec-7ee101aa", "89abcdef")];
        assert_eq!(
            plan_rotation(&existing, Some("00000000"), "sec-7ee101aa"),
            RotationPlan::ReplaceStale { orphan_id: 9 }
        );
        assert_eq!(
            plan_rotation(&existing, None, "sec-7ee101aa"),
            RotationPlan::ReplaceStale { orphan_id: 9 }
        );
    }

    // ----- in-memory fakes -----

    #[derive(Default)]
    struct FakeIssuer {
        tokens: Mutex<Vec<TokenInfo>>,
        /// sha1 by token name, so verification can check "the minted
        /// value authenticates".
        values: Mutex<HashMap<String, String>>,
        minted: Mutex<Vec<(String, Vec<String>)>>,
        next_id: Mutex<i64>,
    }

    impl FakeIssuer {
        fn with_tokens(tokens: Vec<TokenInfo>) -> Arc<Self> {
            let f = Self::default();
            *f.tokens.lock().unwrap() = tokens;
            *f.next_id.lock().unwrap() = 100;
            Arc::new(f)
        }
    }

    #[async_trait]
    impl ForgeTokenIssuer for FakeIssuer {
        async fn list_tokens(&self, _user: &str) -> Result<Vec<TokenInfo>, String> {
            Ok(self.tokens.lock().unwrap().clone())
        }
        async fn create_token(
            &self,
            _user: &str,
            name: &str,
            scopes: &[String],
        ) -> Result<MintedToken, String> {
            let mut id = self.next_id.lock().unwrap();
            *id += 1;
            let sha1 = format!("sha1-of-{name}-{}", *id);
            self.tokens.lock().unwrap().push(TokenInfo {
                id: *id,
                name: name.to_string(),
                token_last_eight: last_eight(&sha1).to_string(),
            });
            self.values
                .lock()
                .unwrap()
                .insert(name.to_string(), sha1.clone());
            self.minted
                .lock()
                .unwrap()
                .push((name.to_string(), scopes.to_vec()));
            Ok(MintedToken { id: *id, sha1 })
        }
        async fn delete_token(&self, _user: &str, token_id: i64) -> Result<bool, String> {
            let mut toks = self.tokens.lock().unwrap();
            let before = toks.len();
            toks.retain(|t| t.id != token_id);
            Ok(toks.len() < before)
        }
        async fn repo_readable_with(&self, token: &str, _repo: &str) -> Result<bool, String> {
            Ok(self.values.lock().unwrap().values().any(|v| v == token))
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
        fn get(&self, ns: &str, name: &str, key: &str) -> Option<String> {
            self.map
                .lock()
                .unwrap()
                .get(&format!("{ns}/{name}/{key}"))
                .cloned()
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
            Ok(self.get(ns, name, key))
        }
        async fn write_key(
            &self,
            ns: &str,
            name: &str,
            key: &str,
            value: &str,
        ) -> Result<(), String> {
            self.map
                .lock()
                .unwrap()
                .insert(format!("{ns}/{name}/{key}"), value.to_string());
            Ok(())
        }
    }

    // ----- jobs-api stub (the house axum idiom) -----

    type Captured = std::sync::Arc<Mutex<Vec<(String, JsonValue)>>>;

    /// A rotation packet with the machine phases pending. Returns the
    /// stub's base URL + captured step PUTs as (step_id, body) +
    /// captured rotation-door POSTs as ("{credential_id}/{phase}", body).
    async fn stub_jobs_api(
        step_statuses: &'static [(&'static str, &'static str)],
    ) -> (String, Captured, Captured) {
        stub_jobs_api_with(
            step_statuses
                .iter()
                .map(|(slug, status)| (*slug, *status, json!({ "kept": "yes" })))
                .collect(),
        )
        .await
    }

    /// The same stub, each step carrying the metadata the test names —
    /// what the delivery trigger reads the scope step's fields from.
    async fn stub_jobs_api_with(
        step_rows: Vec<(&'static str, &'static str, JsonValue)>,
    ) -> (String, Captured, Captured) {
        let api = stub_api(step_rows).await;
        (api.url, api.captured, api.rotations)
    }

    /// A STATEFUL jobs API: a completion flips the step it names and a
    /// merge lands its keys, so a second firing reads what the first one
    /// wrote — the only way a retry or a redelivery is tested as the
    /// broker meets it. `fail_once` names writes the API refuses 503
    /// exactly once (`<credential>/<phase>` at the rotation door,
    /// `step-<slug>` for a step's status PUT): the jobs-API blip the
    /// review of car 85b7b55f (F1) put between the forge DELETE and its
    /// record.
    struct StubApi {
        url: String,
        captured: Captured,
        rotations: Captured,
        job_patches: Captured,
        job_meta: Arc<Mutex<serde_json::Map<String, JsonValue>>>,
        steps: Arc<Mutex<Vec<(String, String, JsonValue)>>>,
        fail_once: Arc<Mutex<Vec<String>>>,
    }

    impl StubApi {
        fn status(&self, slug: &str) -> String {
            self.steps
                .lock()
                .unwrap()
                .iter()
                .find(|(s, _, _)| s == slug)
                .map(|(_, st, _)| st.clone())
                .unwrap_or_default()
        }
        fn step_meta(&self, slug: &str) -> JsonValue {
            self.steps
                .lock()
                .unwrap()
                .iter()
                .find(|(s, _, _)| s == slug)
                .map(|(_, _, m)| m.clone())
                .unwrap_or_default()
        }
        /// A step completed by someone other than the broker — the
        /// host's deposit recording `delivered`.
        fn complete(&self, slug: &str, fields: JsonValue) {
            let mut steps = self.steps.lock().unwrap();
            let row = steps.iter_mut().find(|(s, _, _)| s == slug).unwrap();
            row.1 = "completed".into();
            for (k, v) in fields.as_object().unwrap() {
                row.2[k] = v.clone();
            }
        }
        fn fail_once(&self, key: &str) {
            self.fail_once.lock().unwrap().push(key.to_string());
        }
    }

    fn take_failure(fail: &Mutex<Vec<String>>, key: &str) -> bool {
        let mut f = fail.lock().unwrap();
        match f.iter().position(|k| k == key) {
            Some(i) => {
                f.remove(i);
                true
            }
            None => false,
        }
    }

    async fn stub_api(step_rows: Vec<(&'static str, &'static str, JsonValue)>) -> StubApi {
        use axum::extract::Path;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::{Json, Router, routing::get, routing::post, routing::put};

        let captured: Captured = Default::default();
        let rotations: Captured = Default::default();
        let job_patches: Captured = Default::default();
        let job_meta: Arc<Mutex<serde_json::Map<String, JsonValue>>> = Default::default();
        let fail_once: Arc<Mutex<Vec<String>>> = Default::default();
        let steps = Arc::new(Mutex::new(
            step_rows
                .into_iter()
                .map(|(slug, status, m)| (slug.to_string(), status.to_string(), m))
                .collect::<Vec<_>>(),
        ));
        let slug_of = |sid: &str| sid.trim_start_matches("step-").to_string();

        let (st, jm) = (steps.clone(), job_meta.clone());
        let (cap, st_put, fail_put) = (captured.clone(), steps.clone(), fail_once.clone());
        let (merge_cap, st_merge) = (captured.clone(), steps.clone());
        let (rot, fail_rot) = (rotations.clone(), fail_once.clone());
        let (jp, jm_patch) = (job_patches.clone(), job_meta.clone());
        let jobs = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let (st, jm) = (st.clone(), jm.clone());
                    async move {
                        let steps: Vec<JsonValue> = st
                            .lock()
                            .unwrap()
                            .iter()
                            .map(|(slug, status, metadata)| {
                                json!({
                                    "id": format!("step-{slug}"),
                                    "spec_slug": slug,
                                    "status": status,
                                    "metadata": metadata,
                                })
                            })
                            .collect();
                        let metadata = JsonValue::Object(jm.lock().unwrap().clone());
                        Json(json!({ "id": id, "metadata": metadata, "steps": steps }))
                    }
                }),
            )
            // The job merge door: top-level keys merge, `null` deletes.
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(move |Json(body): Json<JsonValue>| {
                    let (jp, jm) = (jp.clone(), jm_patch.clone());
                    async move {
                        let mut m = jm.lock().unwrap();
                        for (k, v) in body.as_object().cloned().unwrap_or_default() {
                            if v.is_null() {
                                m.remove(&k);
                            } else {
                                m.insert(k, v);
                            }
                        }
                        jp.lock().unwrap().push(("job/metadata".into(), body));
                        StatusCode::NO_CONTENT
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}",
                put(
                    move |Path((id, sid)): Path<(String, String)>, Json(body): Json<JsonValue>| {
                        let (cap, st, fail) = (cap.clone(), st_put.clone(), fail_put.clone());
                        async move {
                            // The decided end state (e39a9d2a).
                            if let Some(refused) =
                                super::super::listing_stub::end_state_step_put(&id, &sid, &body)
                            {
                                return refused;
                            }
                            if take_failure(&fail, &sid) {
                                return (StatusCode::SERVICE_UNAVAILABLE, "blip").into_response();
                            }
                            if let Some(status) = body.get("status").and_then(|v| v.as_str()) {
                                let slug = slug_of(&sid);
                                if let Some(row) =
                                    st.lock().unwrap().iter_mut().find(|(s, _, _)| *s == slug)
                                {
                                    row.1 = status.to_string();
                                }
                            }
                            cap.lock().unwrap().push((sid, body));
                            Json(json!({ "ok": true })).into_response()
                        }
                    },
                ),
            )
            // The step merge door, recorded in order with the PUTs as
            // `<step>/metadata`.
            .route(
                "/api/jobs/{id}/steps/{step_id}/metadata",
                axum::routing::patch(
                    move |Path((_id, sid)): Path<(String, String)>, Json(body): Json<JsonValue>| {
                        let (cap, st) = (merge_cap.clone(), st_merge.clone());
                        async move {
                            let slug = slug_of(&sid);
                            if let Some(row) =
                                st.lock().unwrap().iter_mut().find(|(s, _, _)| *s == slug)
                            {
                                for (k, v) in body.as_object().cloned().unwrap_or_default() {
                                    row.2[k] = v;
                                }
                            }
                            cap.lock().unwrap().push((format!("{sid}/metadata"), body));
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            )
            .route(
                "/api/credentials/{id}/rotation/{phase}",
                post(
                    move |Path((id, phase)): Path<(String, String)>,
                          Json(body): Json<JsonValue>| {
                        let (rot, fail) = (rot.clone(), fail_rot.clone());
                        async move {
                            let key = format!("{id}/{phase}");
                            if take_failure(&fail, &key) {
                                return (StatusCode::SERVICE_UNAVAILABLE, "blip").into_response();
                            }
                            rot.lock().unwrap().push((key, body));
                            Json(json!({ "recorded": true })).into_response()
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, jobs).await.unwrap() });
        StubApi {
            url: format!("http://{addr}"),
            captured,
            rotations,
            job_patches,
            job_meta,
            steps,
            fail_once,
        }
    }

    fn rotation_args() -> Vec<(String, Value)> {
        [
            ("forge_user", "david"),
            ("secret_namespace", "boss-dev"),
            ("secret_name", "boss-dev-forge-token"),
            ("secret_key", "token"),
            ("scopes", "write:repository"),
            ("verify_repo", "david/boss"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.into())))
        .collect()
    }

    fn scope_done_ctx(old_token: Option<&str>) -> InvocationContext {
        let mut metadata = json!({
            "credential": "boss-dev-forge-token",
            "reason": "transcript exposure",
            "locations": "k8s secret boss-dev/boss-dev-forge-token",
            "consumers": "/etc/boss-train/forge.token, git credential helper",
        });
        if let Some(old) = old_token {
            metadata["old_token"] = json!(old);
        }
        InvocationContext {
            rule_name: "broker-rotates-the-boss-dev-forge-token".into(),
            triggering_event_id: "evt-rot-1".into(),
            triggering_topic: "step.done.credential-rotation".into(),
            event_payload: json!({
                "job_id": "7ee101aa-3267-4745-8096-06d07df7e144",
                "step_id": "step-scope",
                "kind": "credential-rotation",
                "subject_kind": "custom",
                "subject_id": "boss-dev-forge-token",
                "metadata": metadata,
            }),
        }
    }

    const PENDING_PHASES: &[(&str, &str)] = &[
        ("scope", "completed"),
        ("issue", "ready"),
        ("install", "pending"),
        ("verify", "pending"),
        ("revoke", "pending"),
    ];

    #[tokio::test]
    async fn full_rotation_mints_installs_verifies_revokes_and_records() {
        let issuer = FakeIssuer::with_tokens(vec![tok(7, "the-old-write-token", "deadbeef")]);
        // Old token's value known to the issuer so verification-by-
        // value distinguishes old from new.
        issuer
            .values
            .lock()
            .unwrap()
            .insert("the-old-write-token".into(), "old-value".into());
        let secrets = Arc::new(FakeSecrets::default());
        let (jobs_url, captured, rotations) = stub_jobs_api(PENDING_PHASES).await;

        let h = CredentialRotateForgejo::new(jobs_url, issuer.clone(), secrets.clone());
        h.invoke(
            &rotation_args(),
            &scope_done_ctx(Some("the-old-write-token")),
        )
        .await
        .expect("rotation succeeds");

        // Minted once, with the declared scopes, under the packet-derived name.
        let minted = issuer.minted.lock().unwrap().clone();
        assert_eq!(minted.len(), 1);
        assert_eq!(minted[0].0, "boss-dev-forge-token-7ee101aa");
        assert_eq!(minted[0].1, vec!["write:repository".to_string()]);

        // Installed: the Secret holds the minted value.
        let installed = secrets.get("boss-dev", "boss-dev-forge-token", "token");
        assert_eq!(
            installed.as_deref(),
            issuer
                .values
                .lock()
                .unwrap()
                .get("boss-dev-forge-token-7ee101aa")
                .map(String::as_str)
        );

        // Revoked: the old token is gone from the issuer.
        assert!(
            !issuer
                .tokens
                .lock()
                .unwrap()
                .iter()
                .any(|t| t.name == "the-old-write-token")
        );

        // Recorded: issue, install, verify, revoke completed in order
        // with required-at-done evidence, existing metadata kept (by the
        // merge door: it is never re-sent), and no secret value anywhere
        // in any body.
        let puts = super::super::listing_stub::fold_step_writes(&captured.lock().unwrap());
        let order: Vec<&str> = puts.iter().map(|(sid, _)| sid.as_str()).collect();
        assert_eq!(
            order,
            vec!["step-issue", "step-install", "step-verify", "step-revoke"]
        );
        for (_, body) in &puts {
            assert_eq!(body["status"], "completed");
            assert!(
                body["metadata"].get("kept").is_none(),
                "the step's own keys are not re-sent: the merge door keeps them (e39a9d2a)"
            );
            let flat = body.to_string();
            let value = issuer.values.lock().unwrap()["boss-dev-forge-token-7ee101aa"].clone();
            assert!(
                !flat.contains(&value),
                "secret value leaked into a step body: {flat}"
            );
        }
        let issue_body = &puts[0].1;
        assert!(
            issue_body["metadata"]["issued"]
                .as_str()
                .unwrap()
                .contains("boss-dev-forge-token-7ee101aa")
        );
        assert!(issue_body["metadata"]["issuer"].as_str().is_some());
        let revoke_body = &puts[3].1;
        assert!(
            revoke_body["metadata"]["confirmed_dead"]
                .as_str()
                .unwrap()
                .contains("no longer contains")
        );

        // Evented: one credential.* event per phase, in protocol
        // order, through the registry's rotation door, addressed to
        // the credential the scope step named — and no secret value
        // in any of them.
        let events = rotations.lock().unwrap().clone();
        let order: Vec<&str> = events.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "boss-dev-forge-token/minted",
                "boss-dev-forge-token/installed",
                "boss-dev-forge-token/verified",
                "boss-dev-forge-token/revoked",
            ]
        );
        let value = issuer.values.lock().unwrap()["boss-dev-forge-token-7ee101aa"].clone();
        for (path, body) in &events {
            let flat = body.to_string();
            assert!(
                !flat.contains(&value),
                "secret value leaked into a rotation event ({path}): {flat}"
            );
            assert_eq!(
                body["job_id"], "7ee101aa-3267-4745-8096-06d07df7e144",
                "every phase event chains back to its rotation packet"
            );
        }
        let minted = &events[0].1;
        assert_eq!(minted["token_name"], "boss-dev-forge-token-7ee101aa");
        assert!(minted["token_id"].is_i64());
        assert_eq!(minted["scopes"], json!(["write:repository"]));
        assert_eq!(minted["replaced_orphan"], false);
        let installed_ev = &events[1].1;
        assert_eq!(installed_ev["secret_namespace"], "boss-dev");
        assert_eq!(installed_ev["secret_name"], "boss-dev-forge-token");
        assert_eq!(installed_ev["secret_key"], "token");
        assert_eq!(
            installed_ev["value_length"].as_u64().unwrap(),
            value.len() as u64,
            "the event carries the value's LENGTH, never the value"
        );
        let verified_ev = &events[2].1;
        assert_eq!(verified_ev["verify_repo"], "david/boss");
        assert_eq!(verified_ev["method"], "api");
        let revoked_ev = &events[3].1;
        assert_eq!(revoked_ev["old_token"], "the-old-write-token");
        assert_eq!(revoked_ev["deleted_now"], true);
        assert!(
            revoked_ev["confirmed_dead"]
                .as_str()
                .unwrap()
                .contains("no longer contains")
        );
    }

    #[tokio::test]
    async fn redelivery_after_a_finished_rotation_mints_nothing() {
        // The issuer already holds this packet's token and the Secret
        // holds its value; every phase step is already completed.
        let sha = "sha1-of-boss-dev-forge-token-7ee101aa-101";
        let issuer = FakeIssuer::with_tokens(vec![tok(
            101,
            "boss-dev-forge-token-7ee101aa",
            last_eight(sha),
        )]);
        issuer
            .values
            .lock()
            .unwrap()
            .insert("boss-dev-forge-token-7ee101aa".into(), sha.into());
        let secrets = FakeSecrets::seeded("boss-dev", "boss-dev-forge-token", "token", sha);
        const ALL_DONE: &[(&str, &str)] = &[
            ("scope", "completed"),
            ("issue", "completed"),
            ("install", "completed"),
            ("verify", "completed"),
            ("revoke", "completed"),
        ];
        let (jobs_url, captured, rotations) = stub_jobs_api(ALL_DONE).await;

        let h = CredentialRotateForgejo::new(jobs_url, issuer.clone(), secrets);
        h.invoke(&rotation_args(), &scope_done_ctx(None))
            .await
            .expect("idempotent re-run succeeds");

        assert!(issuer.minted.lock().unwrap().is_empty(), "no second mint");
        assert!(captured.lock().unwrap().is_empty(), "no step rewrites");
        assert!(
            rotations.lock().unwrap().is_empty(),
            "a finished rotation redelivered emits NOTHING — every phase's step \
             is recorded, so every phase's event already is too"
        );
    }

    #[tokio::test]
    async fn a_lost_value_replay_retires_the_orphan_and_mints_again() {
        // Prior attempt minted (id 55) then died before the Secret
        // write — the Secret is empty, the value unrecoverable.
        let issuer =
            FakeIssuer::with_tokens(vec![tok(55, "boss-dev-forge-token-7ee101aa", "51gone55")]);
        *issuer.next_id.lock().unwrap() = 100;
        let secrets = Arc::new(FakeSecrets::default());
        let (jobs_url, _captured, rotations) = stub_jobs_api(PENDING_PHASES).await;

        let h = CredentialRotateForgejo::new(jobs_url, issuer.clone(), secrets.clone());
        h.invoke(&rotation_args(), &scope_done_ctx(None))
            .await
            .expect("replay succeeds");

        let toks = issuer.tokens.lock().unwrap().clone();
        let mine: Vec<_> = toks
            .iter()
            .filter(|t| t.name == "boss-dev-forge-token-7ee101aa")
            .collect();
        assert_eq!(mine.len(), 1, "exactly one rotation token survives");
        assert_ne!(mine[0].id, 55, "the orphan was retired");
        assert!(
            secrets
                .get("boss-dev", "boss-dev-forge-token", "token")
                .is_some()
        );

        // The replacement mint is evented and says it retired an
        // orphan; no old_token was named, so nothing claims a revoke.
        let events = rotations.lock().unwrap().clone();
        let order: Vec<&str> = events.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "boss-dev-forge-token/minted",
                "boss-dev-forge-token/installed",
                "boss-dev-forge-token/verified",
            ]
        );
        assert_eq!(events[0].1["replaced_orphan"], true);
    }

    #[tokio::test]
    async fn a_failed_verification_stops_before_anything_destructive() {
        struct UnverifiableIssuer(Arc<FakeIssuer>);
        #[async_trait]
        impl ForgeTokenIssuer for UnverifiableIssuer {
            async fn list_tokens(&self, u: &str) -> Result<Vec<TokenInfo>, String> {
                self.0.list_tokens(u).await
            }
            async fn create_token(
                &self,
                u: &str,
                n: &str,
                s: &[String],
            ) -> Result<MintedToken, String> {
                self.0.create_token(u, n, s).await
            }
            async fn delete_token(&self, u: &str, t: i64) -> Result<bool, String> {
                self.0.delete_token(u, t).await
            }
            async fn repo_readable_with(&self, _t: &str, _r: &str) -> Result<bool, String> {
                Ok(false)
            }
        }
        let inner = FakeIssuer::with_tokens(vec![tok(7, "the-old-write-token", "deadbeef")]);
        let secrets = Arc::new(FakeSecrets::default());
        let (jobs_url, captured, rotations) = stub_jobs_api(PENDING_PHASES).await;

        let h = CredentialRotateForgejo::new(
            jobs_url,
            Arc::new(UnverifiableIssuer(inner.clone())),
            secrets,
        );
        let err = h
            .invoke(
                &rotation_args(),
                &scope_done_ctx(Some("the-old-write-token")),
            )
            .await
            .expect_err("verification failure is an error");
        assert!(matches!(err, HandlerError::Downstream(_)));

        // The old token survives and no packet step was touched.
        assert!(
            inner
                .tokens
                .lock()
                .unwrap()
                .iter()
                .any(|t| t.name == "the-old-write-token")
        );
        assert!(captured.lock().unwrap().is_empty());
        // The mint and install DID happen and are on the record; the
        // verification never became true, so no `verified` — and
        // nothing destructive ran, so no `revoked`.
        let order: Vec<String> = rotations
            .lock()
            .unwrap()
            .iter()
            .map(|(path, _)| path.clone())
            .collect();
        assert_eq!(
            order,
            vec![
                "boss-dev-forge-token/minted",
                "boss-dev-forge-token/installed",
            ]
        );
    }

    #[tokio::test]
    async fn naming_the_new_token_as_old_is_refused_permanently() {
        let issuer = FakeIssuer::with_tokens(vec![]);
        let secrets = Arc::new(FakeSecrets::default());
        let (jobs_url, _c, _r) = stub_jobs_api(PENDING_PHASES).await;
        let h = CredentialRotateForgejo::new(jobs_url, issuer, secrets);
        let err = h
            .invoke(
                &rotation_args(),
                &scope_done_ctx(Some("boss-dev-forge-token-7ee101aa")),
            )
            .await
            .expect_err("self-revocation is refused");
        assert!(err.is_permanent(), "got {err:?}");
    }

    #[tokio::test]
    async fn a_rotation_that_cannot_name_its_credential_fails_before_any_mint() {
        let issuer = FakeIssuer::with_tokens(vec![]);
        let secrets = Arc::new(FakeSecrets::default());
        let (jobs_url, _c, rotations) = stub_jobs_api(PENDING_PHASES).await;
        let h = CredentialRotateForgejo::new(jobs_url, issuer.clone(), secrets);
        let mut ctx = scope_done_ctx(None);
        // Neither the scope step's `credential` field nor a Subject.
        ctx.event_payload["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("credential");
        ctx.event_payload["subject_id"] = json!("");
        let err = h
            .invoke(&rotation_args(), &ctx)
            .await
            .expect_err("an unnameable rotation is an authoring fault");
        assert!(err.is_permanent(), "got {err:?}");
        assert!(
            issuer.minted.lock().unwrap().is_empty(),
            "the refusal comes BEFORE any side effect"
        );
        assert!(rotations.lock().unwrap().is_empty());
    }

    // ----- the old token named by its last eight (design 1c90d183, D3) -----

    #[test]
    fn a_last_eight_that_names_one_token_resolves_to_its_name() {
        let toks = [
            tok(3, "push-20260818", "aaaa1111"),
            tok(4, "k8s-pull", "bbbb2222"),
        ];
        let got = resolve_last_eight(&toks, "bbbb2222", "sec-7ee101aa").expect("one match");
        assert_eq!(got.name, "k8s-pull");
    }

    #[test]
    fn a_last_eight_that_names_no_token_is_refused() {
        let toks = [tok(3, "push-20260818", "aaaa1111")];
        let err = resolve_last_eight(&toks, "cccc3333", "sec-7ee101aa").expect_err("no match");
        assert!(
            err.contains("no token") && err.contains("cccc3333"),
            "{err}"
        );
    }

    #[test]
    fn a_last_eight_that_names_several_tokens_is_refused_naming_them() {
        let toks = [
            tok(3, "push-20260818", "aaaa1111"),
            tok(5, "dev-pod-push-20260821", "aaaa1111"),
        ];
        let err = resolve_last_eight(&toks, "aaaa1111", "sec-7ee101aa").expect_err("two match");
        assert!(
            err.contains("2 tokens")
                && err.contains("push-20260818")
                && err.contains("dev-pod-push-20260821"),
            "the refusal names every candidate so the scoper can choose by name: {err}"
        );
    }

    #[test]
    fn a_last_eight_that_names_the_replacement_is_refused() {
        let toks = [tok(9, "sec-7ee101aa", "89abcdef")];
        let err = resolve_last_eight(&toks, "89abcdef", "sec-7ee101aa").expect_err("self");
        assert!(err.contains("own replacement"), "{err}");
    }

    #[test]
    fn a_spared_name_matches_exactly_or_by_its_declared_prefix() {
        let spare = parse_spare("boss-gcp, boss-dev-forge-token-*");
        assert!(is_spared("boss-gcp", &spare));
        assert!(is_spared("boss-dev-forge-token-7ee101aa", &spare));
        assert!(!is_spared("boss-gcp-2", &spare), "an exact entry is exact");
        assert!(!is_spared("push-20260818", &spare));
        assert!(parse_spare("").is_empty());
    }

    #[test]
    fn the_revoke_gate_waits_for_an_off_host_delivery_and_refuses_a_wrong_one() {
        assert_eq!(
            revoke_gate(Delivery::Mount, None, "89abcdef"),
            RevokeGate::Open
        );
        assert_eq!(
            revoke_gate(Delivery::OffHost, None, "89abcdef"),
            RevokeGate::AwaitDelivery
        );
        assert_eq!(
            revoke_gate(Delivery::OffHost, Some("89abcdef"), "89abcdef"),
            RevokeGate::Open
        );
        assert!(matches!(
            revoke_gate(Delivery::OffHost, Some("00000000"), "89abcdef"),
            RevokeGate::Refused(_)
        ));
    }

    fn off_host_args() -> Vec<(String, Value)> {
        [
            ("forge_user", "david"),
            ("secret_namespace", "boss"),
            ("secret_name", "forge-host-checkout-token"),
            ("secret_key", "token"),
            ("scopes", "write:repository"),
            ("verify_repo", "david/boss"),
            ("delivery", "off-host"),
            ("spare", "boss-gcp,boss-dev-forge-token-*"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.into())))
        .collect()
    }

    const PACKET: &str = "c4fe0001-aaaa-bbbb-cccc-000000000001";
    const NEW_NAME: &str = "forge-host-checkout-token-c4fe0001";

    fn off_host_scope_metadata(l8: &str) -> JsonValue {
        json!({
            "credential": "forge-host-checkout-token",
            "reason": "leaked into the forge-converge journal",
            "locations": "the forge host checkout",
            "consumers": "forge-converge, publish-github-pr, the tenant-source check",
            "old_token_last_eight": l8,
        })
    }

    fn off_host_ctx(kind: &str, metadata: JsonValue) -> InvocationContext {
        InvocationContext {
            rule_name: "broker-rotates-the-forge-host-checkout-token".into(),
            triggering_event_id: "evt-fh-1".into(),
            triggering_topic: format!("step.done.{kind}"),
            event_payload: json!({
                "job_id": PACKET,
                "step_id": "step-scope",
                "kind": kind,
                "subject_kind": "custom",
                "subject_id": "forge-host-checkout-token",
                "metadata": metadata,
            }),
        }
    }

    /// The leaked token and an unrelated one, as the issuer lists them.
    fn leaked_ledger() -> Arc<FakeIssuer> {
        let issuer = FakeIssuer::with_tokens(vec![
            tok(7, "push-20260818", "1eaked01"),
            tok(8, "boss-gcp", "c0nduct0"),
        ]);
        issuer
            .values
            .lock()
            .unwrap()
            .insert("push-20260818".into(), "old-value".into());
        issuer
    }

    fn still_listed(issuer: &FakeIssuer, name: &str) -> bool {
        issuer.tokens.lock().unwrap().iter().any(|t| t.name == name)
    }

    #[tokio::test]
    async fn an_off_host_rotation_stops_before_revoke_until_the_host_records_delivery() {
        let issuer = leaked_ledger();
        let secrets = Arc::new(FakeSecrets::default());
        let (jobs_url, captured, rotations) = stub_jobs_api(&[
            ("scope", "completed"),
            ("issue", "ready"),
            ("install", "pending"),
            ("verify", "pending"),
            ("delivered", "pending"),
            ("revoke", "pending"),
        ])
        .await;
        let h = CredentialRotateForgejo::new(jobs_url, issuer.clone(), secrets.clone());
        h.invoke(
            &off_host_args(),
            &off_host_ctx("credential-rotation", off_host_scope_metadata("1eaked01")),
        )
        .await
        .expect("the first three phases run");

        assert!(
            secrets
                .get("boss", "forge-host-checkout-token", "token")
                .is_some(),
            "minted and installed into the declared Secret"
        );
        assert!(
            still_listed(&issuer, "push-20260818"),
            "NOTHING is revoked before the host has the new value: the forge fetch, the \
             publish and the tenant check read a file the host fills on its next pass"
        );
        let puts = super::super::listing_stub::fold_step_writes(&captured.lock().unwrap());
        let order: Vec<&str> = puts.iter().map(|(sid, _)| sid.as_str()).collect();
        assert_eq!(order, vec!["step-issue", "step-install", "step-verify"]);
        assert_eq!(
            puts[1].1["metadata"]["delivery"], "off-host",
            "the install step says how the value travels on — the field the protocol's \
             `delivered` step gates on"
        );
        let events: Vec<String> = rotations
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        assert_eq!(
            events,
            vec![
                "forge-host-checkout-token/minted",
                "forge-host-checkout-token/installed",
                "forge-host-checkout-token/verified",
            ],
            "no `revoked` before delivery"
        );
    }

    /// The Secret already holds this packet's token (the scope firing
    /// ran), and the host has recorded delivery.
    async fn delivered_case(
        delivered_l8: &str,
        scope_l8: &str,
    ) -> (
        Arc<FakeIssuer>,
        Captured,
        Captured,
        Result<(), HandlerError>,
    ) {
        let issuer = leaked_ledger();
        let sha = "sha1-of-the-new-forge-host-token-ab12cd34";
        issuer
            .tokens
            .lock()
            .unwrap()
            .push(tok(101, NEW_NAME, last_eight(sha)));
        issuer
            .values
            .lock()
            .unwrap()
            .insert(NEW_NAME.into(), sha.into());
        let secrets = FakeSecrets::seeded("boss", "forge-host-checkout-token", "token", sha);
        let (jobs_url, captured, rotations) = stub_jobs_api_with(vec![
            ("scope", "completed", off_host_scope_metadata(scope_l8)),
            ("issue", "completed", json!({})),
            ("install", "completed", json!({ "delivery": "off-host" })),
            ("verify", "completed", json!({})),
            (
                "delivered",
                "completed",
                json!({ "delivered_last_eight": delivered_l8 }),
            ),
            ("revoke", "ready", json!({})),
        ])
        .await;
        let h = CredentialRotateForgejo::new(jobs_url, issuer.clone(), secrets);
        let got = h
            .invoke(
                &off_host_args(),
                &off_host_ctx(
                    "credential-delivery",
                    json!({ "delivered_last_eight": delivered_l8 }),
                ),
            )
            .await;
        (issuer, captured, rotations, got)
    }

    #[tokio::test]
    async fn a_recorded_delivery_revokes_the_token_the_scope_named_by_its_last_eight() {
        // The fixture sha's last eight, which the host reads off its file.
        assert_eq!(
            last_eight("sha1-of-the-new-forge-host-token-ab12cd34"),
            "ab12cd34"
        );
        let (issuer, captured, rotations, got) = delivered_case("ab12cd34", "1eaked01").await;
        got.expect("delivery recorded with the Secret's last eight: revoke");
        assert!(
            !still_listed(&issuer, "push-20260818"),
            "the token the scope named by its last eight is revoked"
        );
        assert!(still_listed(&issuer, "boss-gcp"), "nothing else is");
        assert!(still_listed(&issuer, NEW_NAME), "the replacement survives");
        assert!(
            issuer.minted.lock().unwrap().is_empty(),
            "a delivery firing never mints"
        );
        let puts = super::super::listing_stub::fold_step_writes(&captured.lock().unwrap());
        let order: Vec<&str> = puts.iter().map(|(sid, _)| sid.as_str()).collect();
        assert_eq!(
            order,
            vec!["step-revoke"],
            "only the step still open is written"
        );
        assert!(
            puts[0].1["metadata"]["revoked"]
                .as_str()
                .unwrap()
                .contains("push-20260818"),
            "{:?}",
            puts[0].1
        );
        let events = rotations.lock().unwrap().clone();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, "forge-host-checkout-token/revoked");
        assert_eq!(events[0].1["old_token"], "push-20260818");
        assert_eq!(events[0].1["old_token_last_eight"], "1eaked01");
        assert_eq!(events[0].1["delivered_last_eight"], "ab12cd34");
    }

    #[tokio::test]
    async fn a_delivery_of_some_other_value_is_refused_and_revokes_nothing() {
        let (issuer, _c, rotations, got) = delivered_case("00000000", "1eaked01").await;
        let err = got.expect_err("the host delivered something the Secret does not hold");
        assert!(err.is_permanent(), "{err:?}");
        assert!(still_listed(&issuer, "push-20260818"));
        assert!(rotations.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_scope_that_names_the_replacement_by_its_last_eight_revokes_nothing() {
        // The replacement's own last eight named as the old token: once
        // minted, the ledger's one match is the replacement itself.
        let (issuer, _c, _r, got) = delivered_case("ab12cd34", "ab12cd34").await;
        let err = got.expect_err("the scope named the replacement");
        assert!(err.is_permanent(), "{err:?}");
        assert!(still_listed(&issuer, NEW_NAME));
        assert!(still_listed(&issuer, "push-20260818"));
    }

    #[tokio::test]
    async fn a_delivery_before_the_secret_holds_this_packets_token_mints_nothing() {
        let issuer = leaked_ledger();
        let secrets = Arc::new(FakeSecrets::default());
        let (jobs_url, _c, rotations) = stub_jobs_api_with(vec![
            ("scope", "completed", off_host_scope_metadata("1eaked01")),
            (
                "delivered",
                "completed",
                json!({ "delivered_last_eight": "ab12cd34" }),
            ),
        ])
        .await;
        let h = CredentialRotateForgejo::new(jobs_url, issuer.clone(), secrets);
        let err = h
            .invoke(
                &off_host_args(),
                &off_host_ctx(
                    "credential-delivery",
                    json!({ "delivered_last_eight": "ab12cd34" }),
                ),
            )
            .await
            .expect_err("nothing was installed for this packet");
        assert!(err.is_permanent(), "{err:?}");
        assert!(issuer.minted.lock().unwrap().is_empty());
        assert!(rotations.lock().unwrap().is_empty());
        assert!(still_listed(&issuer, "push-20260818"));
    }

    #[tokio::test]
    async fn a_last_eight_that_resolves_to_nothing_or_to_a_spared_token_refuses_before_the_mint() {
        for (l8, why) in [
            ("cccc3333", "no token ends in it"),
            (
                "c0nduct0",
                "it is the conductor's token, declared to another consumer",
            ),
        ] {
            let issuer = leaked_ledger();
            let secrets = Arc::new(FakeSecrets::default());
            let (jobs_url, _c, rotations) = stub_jobs_api(PENDING_PHASES).await;
            let h = CredentialRotateForgejo::new(jobs_url, issuer.clone(), secrets.clone());
            let err = h
                .invoke(
                    &off_host_args(),
                    &off_host_ctx("credential-rotation", off_host_scope_metadata(l8)),
                )
                .await
                .expect_err(why);
            assert!(err.is_permanent(), "{why}: {err:?}");
            assert!(
                issuer.minted.lock().unwrap().is_empty(),
                "{why}: refused BEFORE the mint, so a typo costs nothing"
            );
            assert!(rotations.lock().unwrap().is_empty(), "{why}");
            assert!(
                secrets
                    .get("boss", "forge-host-checkout-token", "token")
                    .is_none(),
                "{why}"
            );
        }
    }

    #[tokio::test]
    async fn a_delivery_firing_on_a_mount_delivered_credential_is_refused() {
        let issuer = FakeIssuer::with_tokens(vec![]);
        let secrets = Arc::new(FakeSecrets::default());
        let (jobs_url, _c, _r) = stub_jobs_api(PENDING_PHASES).await;
        let h = CredentialRotateForgejo::new(jobs_url, issuer, secrets);
        let err = h
            .invoke(
                &rotation_args(),
                &off_host_ctx(
                    "credential-delivery",
                    json!({ "delivered_last_eight": "ab12cd34" }),
                ),
            )
            .await
            .expect_err("a mount credential has no delivery step to wait on");
        assert!(err.is_permanent(), "{err:?}");
    }

    #[tokio::test]
    async fn a_mount_rotation_revokes_by_last_eight_in_one_firing() {
        let issuer = FakeIssuer::with_tokens(vec![tok(7, "the-old-write-token", "deadbeef")]);
        let secrets = Arc::new(FakeSecrets::default());
        let (jobs_url, captured, _r) = stub_jobs_api(PENDING_PHASES).await;
        let h = CredentialRotateForgejo::new(jobs_url, issuer.clone(), secrets);
        let mut ctx = scope_done_ctx(None);
        ctx.event_payload["metadata"]["old_token_last_eight"] = json!("deadbeef");
        h.invoke(&rotation_args(), &ctx)
            .await
            .expect("mount delivery keeps today's order");
        assert!(!still_listed(&issuer, "the-old-write-token"));
        let puts = super::super::listing_stub::fold_step_writes(&captured.lock().unwrap());
        assert_eq!(puts.len(), 4, "issue, install, verify, revoke");
        assert_eq!(puts[1].1["metadata"]["delivery"], "mount");
    }

    // ----- a revoke named by last eight is idempotent (review F1) -----
    //
    // The review of car 85b7b55f (2026-09-26, BLOCKING F1): every firing
    // re-resolved the last eight against the ledger, and zero matches
    // was Permanent. So a firing that DELETED the old token and then lost
    // its record to a jobs-API blip could never finish — its retry found
    // no token ending in those eight and refused, leaving the token dead
    // on the forge with no `credential.revoked` event and the revoke step
    // stuck ready. The fix resolves the last eight to a NAME once, before
    // anything is minted, and records it on the packet; every later
    // firing judges by that name, and a name the ledger no longer lists
    // is a token already gone — exactly as `old_token` has always read.

    fn off_host_phases() -> Vec<(&'static str, &'static str, JsonValue)> {
        vec![
            ("scope", "completed", off_host_scope_metadata("1eaked01")),
            ("issue", "ready", json!({})),
            ("install", "pending", json!({})),
            ("verify", "pending", json!({})),
            ("delivered", "pending", json!({})),
            ("revoke", "pending", json!({})),
        ]
    }

    /// The scope firing, then the host's delivery of what it installed.
    async fn scoped_and_delivered(
        issuer: &Arc<FakeIssuer>,
        secrets: &Arc<FakeSecrets>,
        api: &StubApi,
    ) -> Arc<CredentialRotateForgejo> {
        let h = CredentialRotateForgejo::new(api.url.clone(), issuer.clone(), secrets.clone());
        h.invoke(
            &off_host_args(),
            &off_host_ctx("credential-rotation", off_host_scope_metadata("1eaked01")),
        )
        .await
        .expect("the scope firing runs issue, install, verify");
        let installed = secrets
            .get("boss", "forge-host-checkout-token", "token")
            .expect("installed");
        api.complete(
            "delivered",
            json!({ "delivered_last_eight": last_eight(&installed) }),
        );
        h
    }

    fn delivery_ctx(api: &StubApi) -> InvocationContext {
        off_host_ctx(
            "credential-delivery",
            json!({ "delivered_last_eight": api.step_meta("delivered")["delivered_last_eight"] }),
        )
    }

    #[tokio::test]
    async fn the_scope_firing_records_the_name_its_last_eight_resolved_to() {
        let issuer = leaked_ledger();
        let secrets = Arc::new(FakeSecrets::default());
        let api = stub_api(off_host_phases()).await;
        scoped_and_delivered(&issuer, &secrets, &api).await;
        let resolved = api.job_meta.lock().unwrap()[OLD_TOKEN_RESOLVED_KEY].clone();
        assert_eq!(resolved["name"], "push-20260818", "{resolved}");
        assert_eq!(resolved["id"], 7, "{resolved}");
        assert_eq!(resolved["last_eight"], "1eaked01", "{resolved}");
        assert_eq!(
            api.job_patches.lock().unwrap().len(),
            1,
            "recorded once, through the job merge door"
        );
    }

    #[tokio::test]
    async fn a_revoke_retried_after_the_forge_delete_lands_its_event_and_its_step() {
        // Each place a jobs-API blip can fall AFTER the forge DELETE: the
        // rotation door's `revoked` record, and the revoke step's
        // completion.
        for blip in ["forge-host-checkout-token/revoked", "step-revoke"] {
            let issuer = leaked_ledger();
            let secrets = Arc::new(FakeSecrets::default());
            let api = stub_api(off_host_phases()).await;
            let h = scoped_and_delivered(&issuer, &secrets, &api).await;

            api.fail_once(blip);
            let err = h
                .invoke(&off_host_args(), &delivery_ctx(&api))
                .await
                .expect_err("the blip fails this firing");
            assert!(!err.is_permanent(), "{blip}: a blip is retryable: {err:?}");
            assert!(
                !still_listed(&issuer, "push-20260818"),
                "{blip}: the forge DELETE ran before the blip"
            );
            assert_ne!(api.status("revoke"), "completed", "{blip}");

            // The retry — JetStream redelivers the same event.
            h.invoke(&off_host_args(), &delivery_ctx(&api))
                .await
                .unwrap_or_else(|e| {
                    panic!(
                        "{blip}: the retry finds the token already gone and records it, \
                         rather than refusing a last eight that no longer matches: {e:?}"
                    )
                });
            assert_eq!(api.status("revoke"), "completed", "{blip}: the step lands");
            let revoke = api.step_meta("revoke");
            assert!(
                revoke["revoked"]
                    .as_str()
                    .is_some_and(|s| s.contains("push-20260818")),
                "{blip}: {revoke}"
            );
            let revoked: Vec<JsonValue> = api
                .rotations
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p == "forge-host-checkout-token/revoked")
                .map(|(_, b)| b.clone())
                .collect();
            assert!(!revoked.is_empty(), "{blip}: the event lands");
            assert!(
                revoked.iter().all(|b| b["old_token"] == "push-20260818"
                    && b["old_token_last_eight"] == "1eaked01"),
                "{blip}: {revoked:?}"
            );
            assert!(
                still_listed(&issuer, NEW_NAME),
                "{blip}: the replacement survives"
            );
            assert!(
                still_listed(&issuer, "boss-gcp"),
                "{blip}: nothing else goes"
            );
        }
    }

    #[tokio::test]
    async fn a_redelivered_finished_rotation_named_by_last_eight_is_a_no_op() {
        let issuer = leaked_ledger();
        let secrets = Arc::new(FakeSecrets::default());
        let api = stub_api(off_host_phases()).await;
        let h = scoped_and_delivered(&issuer, &secrets, &api).await;
        h.invoke(&off_host_args(), &delivery_ctx(&api))
            .await
            .expect("the delivery firing revokes");
        assert_eq!(api.status("revoke"), "completed");

        let (steps, events, patches, mints) = (
            api.captured.lock().unwrap().len(),
            api.rotations.lock().unwrap().len(),
            api.job_patches.lock().unwrap().len(),
            issuer.minted.lock().unwrap().len(),
        );
        // Both triggers redelivered after the rotation finished.
        for ctx in [
            off_host_ctx("credential-rotation", off_host_scope_metadata("1eaked01")),
            delivery_ctx(&api),
        ] {
            h.invoke(&off_host_args(), &ctx).await.unwrap_or_else(|e| {
                panic!(
                    "{}: a finished rotation redelivered is a no-op, not a refusal: {e:?}",
                    ctx.triggering_topic
                )
            });
        }
        assert_eq!(api.captured.lock().unwrap().len(), steps, "no step write");
        assert_eq!(api.rotations.lock().unwrap().len(), events, "no event");
        assert_eq!(
            api.job_patches.lock().unwrap().len(),
            patches,
            "no annotation"
        );
        assert_eq!(issuer.minted.lock().unwrap().len(), mints, "no mint");
    }

    #[tokio::test]
    async fn a_mount_revoke_by_last_eight_retried_after_the_delete_finishes() {
        let issuer = FakeIssuer::with_tokens(vec![tok(7, "the-old-write-token", "deadbeef")]);
        let secrets = Arc::new(FakeSecrets::default());
        let api = stub_api(
            PENDING_PHASES
                .iter()
                .map(|(s, st)| (*s, *st, json!({})))
                .collect(),
        )
        .await;
        let h = CredentialRotateForgejo::new(api.url.clone(), issuer.clone(), secrets);
        let mut ctx = scope_done_ctx(None);
        ctx.event_payload["metadata"]["old_token_last_eight"] = json!("deadbeef");
        api.fail_once("boss-dev-forge-token/revoked");
        let err = h
            .invoke(&rotation_args(), &ctx)
            .await
            .expect_err("the blip after the delete");
        assert!(!err.is_permanent(), "{err:?}");
        assert!(!still_listed(&issuer, "the-old-write-token"));
        h.invoke(&rotation_args(), &ctx)
            .await
            .expect("the retry records the absence");
        assert_eq!(api.status("revoke"), "completed");
        assert_eq!(
            issuer.minted.lock().unwrap().len(),
            1,
            "the retry converges; it never mints again"
        );
    }

    /// The recorded name is the one the scope's last eight resolved to;
    /// if the ledger later lists that NAME ending in different eight, the
    /// name no longer means the token the scoper named, and nothing is
    /// deleted by it.
    #[tokio::test]
    async fn a_recorded_name_that_now_ends_in_other_eight_revokes_nothing() {
        let issuer = leaked_ledger();
        let secrets = Arc::new(FakeSecrets::default());
        let api = stub_api(off_host_phases()).await;
        let h = scoped_and_delivered(&issuer, &secrets, &api).await;
        for t in issuer.tokens.lock().unwrap().iter_mut() {
            if t.name == "push-20260818" {
                t.token_last_eight = "0therone".into();
            }
        }
        let err = h
            .invoke(&off_host_args(), &delivery_ctx(&api))
            .await
            .expect_err("a reused name is not the named token");
        assert!(err.is_permanent(), "{err:?}");
        assert!(still_listed(&issuer, "push-20260818"));
    }

    // ----- review F1b of car 85b7b55f (re-review of 5ef6db0b, 2026-09-26) -----
    //
    // `old_token_resolved` is ordinary job metadata: the filer, or anyone
    // with job write, can set it through PATCH /api/jobs/{id}/metadata.
    // Trusted as written, a pre-seeded {name: "no-such-token", last_eight:
    // "1eaked01"} made the scope firing skip the resolve, and the delivery
    // firing read the absent name as "already revoked" — recording
    // credential.revoked and completing the revoke step while the leaked
    // token ending 1eaked01 stayed live. The record is now a claim judged
    // against the ledger every time it is read: an absent name is believed
    // only while NO live token but the replacement ends in its eight.

    /// The review's exact pre-seed.
    fn forged_resolution() -> JsonValue {
        json!({ "name": "no-such-token", "id": 999, "last_eight": "1eaked01" })
    }

    /// The revoked events recorded so far.
    fn revoked_events(api: &StubApi) -> usize {
        api.rotations
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.ends_with("/revoked"))
            .count()
    }

    #[tokio::test]
    async fn a_pre_seeded_resolution_naming_an_absent_token_is_refused_before_the_mint() {
        let issuer = leaked_ledger();
        let secrets = Arc::new(FakeSecrets::default());
        let api = stub_api(off_host_phases()).await;
        api.job_meta
            .lock()
            .unwrap()
            .insert(OLD_TOKEN_RESOLVED_KEY.into(), forged_resolution());
        let h = CredentialRotateForgejo::new(api.url.clone(), issuer.clone(), secrets.clone());
        let err = h
            .invoke(
                &off_host_args(),
                &off_host_ctx("credential-rotation", off_host_scope_metadata("1eaked01")),
            )
            .await
            .expect_err("a record naming an absent token while push-20260818 is live");
        assert!(err.is_permanent(), "{err:?}");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("push-20260818") && msg.contains("no-such-token"),
            "the refusal names the live token and the record it contradicts: {msg}"
        );
        assert!(issuer.minted.lock().unwrap().is_empty(), "nothing minted");
        assert!(api.rotations.lock().unwrap().is_empty(), "no event at all");
        assert!(still_listed(&issuer, "push-20260818"));
    }

    #[tokio::test]
    async fn a_resolution_forged_after_the_scope_firing_records_no_revoke() {
        let issuer = leaked_ledger();
        let secrets = Arc::new(FakeSecrets::default());
        let api = stub_api(off_host_phases()).await;
        let h = scoped_and_delivered(&issuer, &secrets, &api).await;
        // Tampered between the scope firing and the host's delivery.
        api.job_meta
            .lock()
            .unwrap()
            .insert(OLD_TOKEN_RESOLVED_KEY.into(), forged_resolution());
        let err = h
            .invoke(&off_host_args(), &delivery_ctx(&api))
            .await
            .expect_err("the delivery firing must not believe the forged name");
        assert!(err.is_permanent(), "{err:?}");
        assert!(still_listed(&issuer, "push-20260818"), "the leak is live");
        assert_eq!(revoked_events(&api), 0, "no credential.revoked recorded");
        assert_ne!(
            api.status("revoke"),
            "completed",
            "the revoke step stays open"
        );
    }

    /// An issuer whose FIRST listing hides one token — the ledger moving
    /// between the resolve point and the DELETE, so the second check is
    /// exercised on its own rather than behind the first.
    struct LateListing {
        inner: Arc<FakeIssuer>,
        hide_once: Mutex<Option<String>>,
    }

    #[async_trait]
    impl ForgeTokenIssuer for LateListing {
        async fn list_tokens(&self, u: &str) -> Result<Vec<TokenInfo>, String> {
            let mut all = self.inner.list_tokens(u).await?;
            if let Some(hidden) = self.hide_once.lock().unwrap().take() {
                all.retain(|t| t.name != hidden);
            }
            Ok(all)
        }
        async fn create_token(
            &self,
            u: &str,
            n: &str,
            s: &[String],
        ) -> Result<MintedToken, String> {
            self.inner.create_token(u, n, s).await
        }
        async fn delete_token(&self, u: &str, t: i64) -> Result<bool, String> {
            self.inner.delete_token(u, t).await
        }
        async fn repo_readable_with(&self, t: &str, r: &str) -> Result<bool, String> {
            self.inner.repo_readable_with(t, r).await
        }
    }

    #[tokio::test]
    async fn a_forged_resolution_is_judged_again_before_the_delete() {
        let issuer = leaked_ledger();
        let secrets = Arc::new(FakeSecrets::default());
        let api = stub_api(off_host_phases()).await;
        scoped_and_delivered(&issuer, &secrets, &api).await;
        api.job_meta
            .lock()
            .unwrap()
            .insert(OLD_TOKEN_RESOLVED_KEY.into(), forged_resolution());
        let late = Arc::new(LateListing {
            inner: issuer.clone(),
            hide_once: Mutex::new(Some("push-20260818".into())),
        });
        let h = CredentialRotateForgejo::new(api.url.clone(), late, secrets);
        let err = h
            .invoke(&off_host_args(), &delivery_ctx(&api))
            .await
            .expect_err("the pre-DELETE listing shows a live token ending 1eaked01");
        assert!(err.is_permanent(), "{err:?}");
        assert!(still_listed(&issuer, "push-20260818"));
        assert_eq!(revoked_events(&api), 0);
        assert_ne!(api.status("revoke"), "completed");
    }

    #[tokio::test]
    async fn a_resolution_deleted_after_the_forge_delete_fails_closed() {
        let issuer = leaked_ledger();
        let secrets = Arc::new(FakeSecrets::default());
        let api = stub_api(off_host_phases()).await;
        let h = scoped_and_delivered(&issuer, &secrets, &api).await;
        api.fail_once("forge-host-checkout-token/revoked");
        h.invoke(&off_host_args(), &delivery_ctx(&api))
            .await
            .expect_err("the blip after the delete");
        api.job_meta.lock().unwrap().remove(OLD_TOKEN_RESOLVED_KEY);
        let err = h
            .invoke(&off_host_args(), &delivery_ctx(&api))
            .await
            .expect_err("with no record, a last eight nothing ends in is refused");
        assert!(err.is_permanent(), "{err:?}");
        assert_eq!(
            revoked_events(&api),
            0,
            "no revoke claimed without a record"
        );
    }

    #[test]
    fn a_recorded_resolution_is_judged_against_the_ledger() {
        let r = ResolvedOld {
            name: "no-such-token".into(),
            id: 999,
            last_eight: "1eaked01".into(),
        };
        let live = [
            tok(7, "push-20260818", "1eaked01"),
            tok(101, NEW_NAME, "ab12cd34"),
        ];
        let err = judge_recorded(&live, &r, NEW_NAME).expect_err("a live token carries 1eaked01");
        assert!(err.contains("push-20260818"), "{err}");
        // Absent, and nothing but the replacement ends in the eight:
        // the honest retry after the DELETE.
        let gone = [
            tok(101, NEW_NAME, "1eaked01"),
            tok(8, "boss-gcp", "c0nduct0"),
        ];
        judge_recorded(&gone, &r, NEW_NAME).expect("already revoked");
        // Present by name: it must still end in the recorded eight.
        let honest = ResolvedOld {
            name: "push-20260818".into(),
            id: 7,
            last_eight: "1eaked01".into(),
        };
        judge_recorded(&live, &honest, NEW_NAME).expect("the named token, as recorded");
        let renamed = [tok(7, "push-20260818", "0therone")];
        judge_recorded(&renamed, &honest, NEW_NAME).expect_err("the name means another token");
    }

    // ----- round 3 review of 3ee1bb0c (2026-09-26): F1c, F1d, F1e -----
    //
    // F1c and F1d live in the adapter's contract with the forge, where the
    // in-memory issuer above cannot see them, so these drive the handler
    // through the REAL `ForgejoAdmin` against a forge stand-in that logs
    // every request as the forge would receive it (`forge_stub`).

    use crate::handlers::credential_issuer::ForgejoAdmin;
    use crate::handlers::forge_stub::{self, StubToken};

    /// The leaked token (oldest) and the conductor's, as the forge holds them.
    fn leaked_on_the_forge() -> Vec<StubToken> {
        vec![
            StubToken::new(7, "push-20260818", "old-value-1eaked01"),
            StubToken::new(8, "boss-gcp", "conductor-c0nduct0"),
        ]
    }

    /// One mount-delivered firing (the whole rotation, revoke included),
    /// the scope naming the old token by `old_last8` and/or `old_token`,
    /// with `record` pre-seeded as the packet's `old_token_resolved`.
    async fn fire_against_forge(
        forge: &forge_stub::ForgeStub,
        old_last8: Option<&str>,
        old_token: Option<&str>,
        record: Option<JsonValue>,
    ) -> (StubApi, Result<(), HandlerError>) {
        let api = stub_api(
            PENDING_PHASES
                .iter()
                .map(|(s, st)| (*s, *st, json!({})))
                .collect(),
        )
        .await;
        if let Some(r) = record {
            api.job_meta
                .lock()
                .unwrap()
                .insert(OLD_TOKEN_RESOLVED_KEY.into(), r);
        }
        let issuer = ForgejoAdmin::new(forge.url.clone(), "broker-root-stub");
        let h =
            CredentialRotateForgejo::new(api.url.clone(), issuer, Arc::new(FakeSecrets::default()));
        let mut ctx = scope_done_ctx(old_token);
        if let Some(l8) = old_last8 {
            ctx.event_payload["metadata"]["old_token_last_eight"] = json!(l8);
        }
        let got = h.invoke(&rotation_args(), &ctx).await;
        (api, got)
    }

    /// F1c, the review's shape: sixty tokens newer than the leak push it
    /// off the first page, and a pre-seeded absent name then read as
    /// "already revoked" against a ledger that never showed the leak.
    #[tokio::test]
    async fn a_leak_past_the_first_page_is_still_seen_by_the_judgement() {
        let mut tokens = leaked_on_the_forge();
        tokens.extend(forge_stub::newer_tokens(60));
        let forge = forge_stub::serve(tokens, 50).await;
        let (api, got) =
            fire_against_forge(&forge, Some("1eaked01"), None, Some(forged_resolution())).await;
        let err = got.expect_err("push-20260818 is live on page two and ends in 1eaked01");
        assert!(err.is_permanent(), "{err:?}");
        assert!(forge.listed("push-20260818"), "the leak is live");
        assert_eq!(revoked_events(&api), 0, "no credential.revoked recorded");
        assert_eq!(forge.mints(), 0, "refused before the mint");
        assert!(forge.deletes().is_empty(), "{:?}", forge.deletes());
    }

    /// The honest rotation of a leak that sits past the first page: the
    /// last eight resolves against the WHOLE ledger, and the DELETE names
    /// the token by the number the ledger row carries.
    #[tokio::test]
    async fn a_leak_past_the_first_page_is_resolved_and_deleted_by_its_id() {
        let mut tokens = leaked_on_the_forge();
        tokens.extend(forge_stub::newer_tokens(60));
        let forge = forge_stub::serve(tokens, 50).await;
        let (api, got) = fire_against_forge(&forge, Some("1eaked01"), None, None).await;
        got.expect("the leak resolves on page two and is revoked");
        assert!(!forge.listed("push-20260818"), "the leak is revoked");
        assert!(forge.listed("boss-gcp"), "nothing else is");
        assert_eq!(
            forge.deletes(),
            vec!["/api/v1/admin/users/david/tokens/7".to_string()],
            "one DELETE, by the ledger row's numeric id"
        );
        assert_eq!(revoked_events(&api), 1);
        assert_eq!(api.status("revoke"), "completed");
    }

    /// F1d, the review's shape: a recorded name that is a PATH. With no
    /// live token ending in the scope's eight (the leak revoked by hand),
    /// the record reads as already revoked — and it used to be DELETEd as
    /// spelled, which reqwest resolves to `/api/v1/repos/david/boss`, sent
    /// with the broker's admin root token.
    #[tokio::test]
    async fn a_recorded_name_that_is_a_path_never_reaches_a_delete() {
        let forge = forge_stub::serve(
            vec![StubToken::new(8, "boss-gcp", "conductor-c0nduct0")],
            50,
        )
        .await;
        let (_api, got) = fire_against_forge(
            &forge,
            Some("1eaked01"),
            None,
            Some(json!({
                "name": "../../../../repos/david/boss",
                "id": 999,
                "last_eight": "1eaked01",
            })),
        )
        .await;
        assert!(
            forge.deletes().is_empty(),
            "no DELETE of any kind: {:?}",
            forge.deletes()
        );
        assert!(forge.listed("boss-gcp"));
        // Nothing live ends in the named eight, so the revoke is recorded
        // as an observed absence, never as a deletion.
        got.expect("no live token ends in 1eaked01: already revoked");
    }

    /// The same path through the scope step's own `old_token`, which is
    /// how it reached the forge on main before this car (the scoper only).
    #[tokio::test]
    async fn a_scope_old_token_that_is_a_path_never_reaches_a_delete() {
        for old in ["../../../../repos/david/boss", "7?x=1", "7#frag", "0x7"] {
            let forge = forge_stub::serve(leaked_on_the_forge(), 50).await;
            let (_api, got) = fire_against_forge(&forge, None, Some(old), None).await;
            assert!(
                forge.deletes().is_empty(),
                "{old}: no DELETE of any kind: {:?} ({got:?})",
                forge.deletes()
            );
            assert!(forge.listed("push-20260818"), "{old}");
            assert!(forge.listed("boss-gcp"), "{old}");
        }
    }

    /// F1e: two live tokens share the scope's eight, and the record names
    /// the one the scoper did not mean. A last eight names ONE token or
    /// none; with two, nothing is revoked by it.
    #[tokio::test]
    async fn a_record_naming_one_of_two_tokens_sharing_the_eight_revokes_nothing() {
        let issuer = FakeIssuer::with_tokens(vec![
            tok(7, "push-20260818", "1eaked01"),
            tok(9, "dev-pod-push-20260821", "1eaked01"),
        ]);
        let api = stub_api(
            PENDING_PHASES
                .iter()
                .map(|(s, st)| (*s, *st, json!({})))
                .collect(),
        )
        .await;
        api.job_meta.lock().unwrap().insert(
            OLD_TOKEN_RESOLVED_KEY.into(),
            json!({ "name": "dev-pod-push-20260821", "id": 9, "last_eight": "1eaked01" }),
        );
        let h = CredentialRotateForgejo::new(
            api.url.clone(),
            issuer.clone(),
            Arc::new(FakeSecrets::default()),
        );
        let mut ctx = scope_done_ctx(None);
        ctx.event_payload["metadata"]["old_token_last_eight"] = json!("1eaked01");
        let err = h
            .invoke(&rotation_args(), &ctx)
            .await
            .expect_err("two tokens end in 1eaked01");
        assert!(err.is_permanent(), "{err:?}");
        assert!(still_listed(&issuer, "push-20260818"));
        assert!(still_listed(&issuer, "dev-pod-push-20260821"));
        assert_eq!(revoked_events(&api), 0);
        assert!(issuer.minted.lock().unwrap().is_empty(), "before the mint");
    }

    #[test]
    fn a_recorded_resolution_needs_exactly_one_carrier_of_its_eight() {
        let named = ResolvedOld {
            name: "dev-pod-push-20260821".into(),
            id: 9,
            last_eight: "1eaked01".into(),
        };
        let two = [
            tok(7, "push-20260818", "1eaked01"),
            tok(9, "dev-pod-push-20260821", "1eaked01"),
        ];
        let err = judge_recorded(&two, &named, NEW_NAME).expect_err("two carry the eight");
        assert!(
            err.contains("push-20260818") && err.contains("dev-pod-push-20260821"),
            "{err}"
        );
        // The replacement never counts as a carrier.
        let with_replacement = [
            tok(9, "dev-pod-push-20260821", "1eaked01"),
            tok(101, NEW_NAME, "1eaked01"),
        ];
        judge_recorded(&with_replacement, &named, NEW_NAME).expect("one carrier, the named one");
        // Listed by name, but under another id: the name now means another
        // token, whatever it ends in.
        let reissued = [tok(12, "dev-pod-push-20260821", "1eaked01")];
        judge_recorded(&reissued, &named, NEW_NAME).expect_err("same name, another token");
        // A record naming the replacement is refused.
        let self_named = ResolvedOld {
            name: NEW_NAME.into(),
            id: 101,
            last_eight: "1eaked01".into(),
        };
        judge_recorded(&with_replacement, &self_named, NEW_NAME)
            .expect_err("the replacement is never the old token");
    }

    #[tokio::test]
    async fn missing_declaration_arg_is_reported() {
        let issuer = FakeIssuer::with_tokens(vec![]);
        let secrets = Arc::new(FakeSecrets::default());
        let h = CredentialRotateForgejo::new("http://127.0.0.1:1", issuer, secrets);
        let mut args = rotation_args();
        args.retain(|(k, _)| k != "verify_repo");
        let err = h
            .invoke(&args, &scope_done_ctx(None))
            .await
            .expect_err("missing arg");
        assert!(matches!(err, HandlerError::MissingArg(_)));
    }
}
