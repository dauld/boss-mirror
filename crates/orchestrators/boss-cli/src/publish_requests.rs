//! `boss publish-requests` — the conductor drains `publish-request`
//! packets (protocol filed as 0b1b32f9).
//!
//! WHY THIS EXISTS. A workspace with no forge credential cannot push a
//! branch, so `publish-request v1` lets it file a packet instead:
//! metadata `{branch, head_sha, base_sha, bundle_b64, requested_by}`,
//! steps `filed(trigger) → publish(task)`, terminals
//! `published/refused` forking on `steps.publish.metadata.result`. The
//! conductor — the one actor holding the forge credential — verifies
//! the bundle and pushes, or refuses with a reason on the packet.
//!
//! THE NAME AND THE CONTENT MUST NOT DISAGREE (e7b3a044). The packet
//! *names* a head (`head_sha`); the bundle *carries* content. Every
//! refusal that comes from those two disagreeing names both shas, so
//! the workspace that filed it can see exactly which half lied.
//!
//! NEVER FORCE-PUSH. A branch already on the forge at `head_sha` is a
//! success (the mission was the ref, not the transfer); at any other
//! sha it is a refusal. The forge's history is never rewritten by a
//! drain, whatever the packet asks.
//!
//! FAILURE POSTURE. A refusal is an ANSWER and completes the packet's
//! `publish` step with `result=refused`. An *error* (forge unreachable,
//! push rejected mid-race, jobs API away) is not an answer: the packet
//! stays open and the next drain cycle retries it. One poisoned packet
//! must not hold the queue, so per-packet errors are logged and the
//! loop moves on — the same posture the conductor's branch sweep takes.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde_json::{Value, json};

use crate::gate::{api, rows, stamp};
use crate::train::{find_step, id8, metadata_map, step_done};

/// What a publish-request packet asks for, read off its metadata.
/// `bundle_b64` travels separately (it is transport, not intent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Request {
    pub branch: String,
    pub head_sha: String,
    pub base_sha: String,
    pub requested_by: String,
}

/// The drain's answer for one packet. Both arms carry the detail that
/// lands on the `publish` step — the workflow's terminals fork on the
/// `result` field, so this enum IS the protocol's fork, typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Pushed { detail: String },
    Refused { detail: String },
}

fn metadata_str(job: &Value, key: &str) -> Result<String> {
    job.get("metadata")
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("packet metadata is missing `{key}` (or it is not a string)"))
}

/// Read the request off the packet's metadata, naming any missing key.
pub(crate) fn parse_request(job: &Value) -> Result<Request> {
    Ok(Request {
        branch: metadata_str(job, "branch")?,
        head_sha: metadata_str(job, "head_sha")?,
        base_sha: metadata_str(job, "base_sha")?,
        // Who asked matters for the record, not for the verification;
        // its absence degrades the log line, never the drain.
        requested_by: metadata_str(job, "requested_by").unwrap_or_else(|_| "unknown".to_string()),
    })
}

/// The bundle payload, which is required and must be a string. Kept out
/// of [`Request`] because it is transport, not intent — the git half
/// works from the decoded file, and tests state requests without
/// hauling megabytes of encoded pack data around.
pub(crate) fn bundle_field(job: &Value) -> Result<String> {
    metadata_str(job, "bundle_b64")
}

/// Decode the transport encoding. Liberal about whitespace because
/// `base64` without `-w0` wraps its output and a packet filed that way
/// is still a well-formed request.
pub(crate) fn decode_bundle(b64: &str) -> Result<Vec<u8>> {
    let compact: String = b64.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .context("bundle_b64 does not decode as base64")
}

/// A full 40-hex sha — the only spelling of a commit that cannot
/// silently drift from its content. `HEAD`, a short sha, or a ref name
/// is a NAME, and this protocol exists because names and content
/// disagree (e7b3a044).
fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Refusals that need no git at all: a branch name that git or a shell
/// would read as punctuation, `main` itself, or a sha field that is not
/// a full sha. Checked before anything is spawned — same shape as
/// `gate::normalize_mode`: validate before acting, because acting
/// destroys the evidence.
pub(crate) fn refusal_before_git(req: &Request) -> Option<String> {
    if let Err(e) = crate::publish::check_branch(&req.branch) {
        return Some(format!("{e}"));
    }
    if req.branch == "main" {
        return Some(
            "refusing to publish `main` by request — main only moves by a merged train".to_string(),
        );
    }
    for (field, value) in [("head_sha", &req.head_sha), ("base_sha", &req.base_sha)] {
        if !is_full_sha(value) {
            return Some(format!(
                "`{field}` is {value:?}, not a full 40-hex sha — a symbolic or truncated \
                 name can drift from the content it points at, so only the full sha is \
                 accepted"
            ));
        }
    }
    None
}

/// The tip the bundle actually carries for this request.
///
/// A workspace may have built the bundle as `refs/heads/<branch>` or as
/// a bare `HEAD` (`git bundle create f origin/main..HEAD` records only
/// `HEAD`). Both are accepted; anything ambiguous is refused rather
/// than guessed, because the tip is what gets compared to `head_sha`.
pub(crate) fn bundle_tip(list_heads: &str, branch: &str) -> Result<String> {
    let heads: Vec<(&str, &str)> = list_heads
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            match (it.next(), it.next()) {
                (Some(sha), Some(name)) => Some((sha, name)),
                _ => None,
            }
        })
        .collect();
    let wanted = format!("refs/heads/{branch}");
    if let Some((sha, _)) = heads.iter().find(|(_, name)| *name == wanted) {
        return Ok((*sha).to_string());
    }
    if let Some((sha, _)) = heads.iter().find(|(_, name)| *name == "HEAD") {
        return Ok((*sha).to_string());
    }
    // One head under any name is unambiguous — the sha comparison
    // against `head_sha` is what pins the content, not the name.
    if let [(sha, _)] = heads.as_slice() {
        return Ok((*sha).to_string());
    }
    bail!(
        "the bundle does not say which head is `{branch}`: it carries [{}] and this verb \
         will not guess between them",
        heads
            .iter()
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// `None` = the tip and the declared head agree. `Some(detail)` = they
/// do not, and the detail names BOTH shas (e7b3a044).
pub(crate) fn tip_refusal(tip: &str, head_sha: &str) -> Option<String> {
    (tip != head_sha).then(|| {
        format!(
            "the bundle's tip is {tip} but the packet declares head_sha {head_sha} — the \
             name and the content disagree, and neither can be trusted over the other. \
             Re-file with a bundle built at the declared head."
        )
    })
}

/// What the forge already holding (or not holding) the branch means.
/// `None` = absent, proceed to push. `Some(outcome)` = the answer is
/// already decided without pushing anything.
pub(crate) fn forge_decision(
    on_forge: Option<&str>,
    head_sha: &str,
    branch: &str,
    remote: &str,
) -> Option<Outcome> {
    match on_forge {
        None => None,
        Some(sha) if sha == head_sha => Some(Outcome::Pushed {
            detail: format!(
                "already present: {remote} holds {branch} @ {head_sha}; nothing to push"
            ),
        }),
        Some(sha) => Some(Outcome::Refused {
            detail: format!(
                "{remote} already holds {branch} @ {sha}, not the requested {head_sha} — \
                 refusing to force-push over it; re-file against the branch's current head"
            ),
        }),
    }
}

fn git_in(clone: &Path, args: &[&str]) -> Result<std::process::Output> {
    // EVERY git call names its repository explicitly (5b65c2a8): a git
    // fixture or a drain must never act on whatever repo the process
    // happens to be standing in.
    std::process::Command::new("git")
        .arg("-C")
        .arg(clone)
        .args(args)
        .output()
        .with_context(|| format!("spawning git -C {} {args:?}", clone.display()))
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).trim().to_string()
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// What the forge holds for this branch, live — never a tracking ref,
/// which can be stale (`boss publish` learned this: verify by effect).
fn ls_remote_branch(clone: &Path, remote: &str, branch: &str) -> Result<Option<String>> {
    let out = git_in(
        clone,
        &["ls-remote", remote, &format!("refs/heads/{branch}")],
    )?;
    if !out.status.success() {
        bail!("git ls-remote {remote} failed: {}", stderr_of(&out));
    }
    Ok(stdout_of(&out)
        .split_whitespace()
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string))
}

/// The git half of one packet, given a decoded bundle on disk. Pure of
/// the jobs API on purpose: everything here is testable against
/// fixture repositories, and everything HTTP stays in `drain_one`.
///
/// `Ok(Refused)` is an ANSWER — the request can never succeed as filed.
/// `Err` is not: the forge was unreachable, or a push lost a race, and
/// the next drain cycle should try again. A race that left the branch
/// at another sha resolves itself: the next cycle's `forge_decision`
/// reads it and refuses.
///
/// The remote is a PARAMETER, resolved per-clone by the caller —
/// `origin`, `fork` and `gcp` all name different things in different
/// clones, which is why nothing here spells a remote name.
pub(crate) fn fulfil(clone: &Path, remote: &str, req: &Request, bundle: &Path) -> Result<Outcome> {
    if let Some(detail) = refusal_before_git(req) {
        return Ok(Outcome::Refused { detail });
    }
    let refuse = |detail: String| Ok(Outcome::Refused { detail });

    // Forge main FIRST, then verify: `git bundle verify` also checks
    // that the repository holds the bundle's prerequisites, and a
    // conductor clone that simply had not fetched yet must not refuse
    // a bundle that is perfectly good against the real main.
    let fetched = git_in(clone, &["fetch", "-q", remote, "main"])?;
    if !fetched.status.success() {
        bail!("git fetch {remote} main failed: {}", stderr_of(&fetched));
    }
    let main_out = git_in(clone, &["rev-parse", "FETCH_HEAD"])?;
    if !main_out.status.success() {
        bail!("could not resolve {remote} main after fetching it");
    }
    let main_sha = stdout_of(&main_out);

    let bundle_str = bundle
        .to_str()
        .ok_or_else(|| anyhow!("bundle path {} is not utf-8", bundle.display()))?;
    let verified = git_in(clone, &["bundle", "verify", "-q", bundle_str])?;
    if !verified.status.success() {
        return refuse(format!(
            "git bundle verify refused the bundle: {}",
            stderr_of(&verified)
        ));
    }

    let heads = git_in(clone, &["bundle", "list-heads", bundle_str])?;
    if !heads.status.success() {
        return refuse(format!(
            "git bundle list-heads failed: {}",
            stderr_of(&heads)
        ));
    }
    let tip = match bundle_tip(&stdout_of(&heads), &req.branch) {
        Ok(t) => t,
        Err(e) => return refuse(format!("{e:#}")),
    };
    if let Some(detail) = tip_refusal(&tip, &req.head_sha) {
        return refuse(detail);
    }

    // base_sha must sit in FORGE main's history, or the branch cannot
    // be gated against the tree it claims to extend.
    let anc = git_in(
        clone,
        &["merge-base", "--is-ancestor", &req.base_sha, &main_sha],
    )?;
    if !anc.status.success() {
        return refuse(format!(
            "base_sha {} is not an ancestor of {remote} main @ {main_sha} — rebase the \
             branch onto main and re-file",
            req.base_sha
        ));
    }

    // What the forge holds decides idempotence-vs-conflict, live.
    if let Some(outcome) = forge_decision(
        ls_remote_branch(clone, remote, &req.branch)?.as_deref(),
        &req.head_sha,
        &req.branch,
        remote,
    ) {
        return Ok(outcome);
    }

    // The objects land in the clone, then the ref, then the push —
    // never force, and never a refspec built in a shell (`boss
    // publish` exists because zsh once ate one).
    let unbundled = git_in(clone, &["bundle", "unbundle", bundle_str])?;
    if !unbundled.status.success() {
        return refuse(format!(
            "git bundle unbundle failed: {}",
            stderr_of(&unbundled)
        ));
    }
    let local_ref = format!("refs/heads/{}", req.branch);
    let set = git_in(clone, &["update-ref", &local_ref, &req.head_sha])?;
    if !set.status.success() {
        bail!(
            "could not set {local_ref} to {}: {}",
            req.head_sha,
            stderr_of(&set)
        );
    }
    let pushed = git_in(
        clone,
        &["push", remote, &format!("{local_ref}:{local_ref}")],
    )?;
    if !pushed.status.success() {
        // Could be a race (someone pushed since ls-remote) or a down
        // forge; either way the next cycle re-reads reality.
        bail!(
            "git push {remote} {local_ref} failed: {}",
            stderr_of(&pushed)
        );
    }

    // Verify by effect — the push said it worked; ask the forge
    // (`boss publish` learned this the hard way).
    let on_forge = ls_remote_branch(clone, remote, &req.branch)?.unwrap_or_default();
    crate::publish::verify(&req.head_sha, &on_forge, &req.branch)?;

    Ok(Outcome::Pushed {
        detail: format!(
            "pushed {} @ {} to {remote} (base {} verified against main @ {main_sha}, \
             requested by {})",
            req.branch, req.head_sha, req.base_sha, req.requested_by
        ),
    })
}

/// Removes the decoded bundle whatever happens to the packet.
struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Complete the packet's `publish` step: existing metadata carried
/// forward (a PUT replaces metadata wholesale), `result`/`detail` from
/// the outcome, `completed_at` in the one stamp format every verb
/// writes. The terminals fire from the workflow; nothing here touches
/// them.
async fn complete_publish(
    http: &reqwest::Client,
    jid: &str,
    step: &Value,
    outcome: &Outcome,
    now: DateTime<Utc>,
) -> Result<()> {
    let sid = step
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("publish step without an id on packet {}", id8(jid)))?;
    let (result, detail) = match outcome {
        Outcome::Pushed { detail } => ("pushed", detail),
        Outcome::Refused { detail } => ("refused", detail),
    };
    let mut md = metadata_map(step);
    md.insert("result".to_string(), json!(result));
    md.insert("detail".to_string(), json!(detail));
    md.insert("completed_at".to_string(), json!(stamp(now)));
    api(
        http,
        Method::PUT,
        &format!("/api/jobs/{jid}/steps/{sid}"),
        Some(json!({"status": "completed", "metadata": md})),
    )
    .await?;
    Ok(())
}

async fn drain_one(
    http: &reqwest::Client,
    listed: &Value,
    clone: &str,
    remote: &str,
    dry: bool,
    now: DateTime<Utc>,
) -> Result<()> {
    let jid = listed
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("packet without an id in the queue listing"))?
        .to_string();
    // Fetch the full packet: the list endpoint's rows are not trusted
    // to carry steps, and the step ids are what completion needs.
    let fetched = api(http, Method::GET, &format!("/api/jobs/{jid}"), None)
        .await?
        .ok_or_else(|| anyhow!("packet {} came back empty", id8(&jid)))?;
    let job = fetched.get("data").unwrap_or(&fetched).clone();

    let publish_step = find_step(&job, "publish", "Publish the branch");
    if step_done(publish_step) {
        // Already drained — the terminal fork is the workflow's job.
        return Ok(());
    }
    let Some(step) = publish_step else {
        bail!(
            "packet {} has no publish step — not the publish-request shape this verb drains",
            id8(&jid)
        );
    };

    if dry {
        let md = job.get("metadata");
        let m = |k: &str| {
            md.and_then(|m| m.get(k))
                .and_then(Value::as_str)
                .unwrap_or("?")
        };
        println!(
            "publish-requests: DRY would process {} (branch {} @ {})",
            id8(&jid),
            m("branch"),
            m("head_sha")
        );
        return Ok(());
    }

    // A malformed packet can never succeed, so malformation is an
    // ANSWER (refused, with the reason), not an error to retry.
    let outcome = match parse_request(&job) {
        Err(e) => Outcome::Refused {
            detail: format!("{e:#}"),
        },
        Ok(req) => match bundle_field(&job).and_then(|b| decode_bundle(&b)) {
            Err(e) => Outcome::Refused {
                detail: format!("{e:#}"),
            },
            Ok(bytes) => {
                let path =
                    std::env::temp_dir().join(format!("boss-publish-request-{}.bundle", id8(&jid)));
                std::fs::write(&path, &bytes)
                    .with_context(|| format!("writing {}", path.display()))?;
                let _guard = TempFile(path.clone());
                fulfil(Path::new(clone), remote, &req, &path)?
            }
        },
    };

    complete_publish(http, &jid, step, &outcome, now).await?;
    let (verdict, detail) = match &outcome {
        Outcome::Pushed { detail } => ("pushed", detail),
        Outcome::Refused { detail } => ("refused", detail),
    };
    println!("publish-requests: {} {verdict} — {detail}", id8(&jid));
    Ok(())
}

/// Drain every open publish-request packet. The jobs API base comes
/// from `gate::api`, which REFUSES when `BOSS_JOBS_URL` is unset — a
/// drain against the wrong deployment would answer instead of erroring,
/// which is worse.
///
/// `now` arrives from the caller's boundary (the CLI entry or the
/// conductor's run), never from a wallclock read here — the one stamp
/// this verb writes derives from it.
pub(crate) async fn run(clone: &str, remote: &str, dry: bool, now: DateTime<Utc>) -> Result<()> {
    let http = reqwest::Client::new();
    let open = rows(
        api(
            &http,
            Method::GET,
            "/api/jobs?kind=publish-request&status=open&limit=100",
            None,
        )
        .await?,
    );
    if open.is_empty() {
        println!("publish-requests: queue empty");
        return Ok(());
    }
    for packet in &open {
        let id = packet
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        if let Err(e) = drain_one(&http, packet, clone, remote, dry, now).await {
            // An error is not an answer: the packet stays open for the
            // next cycle, and the queue behind it still drains.
            eprintln!(
                "publish-requests: packet {} not drained (left open for the next cycle): {e:#}",
                id8(&id)
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // Pure decisions
    // -----------------------------------------------------------------

    fn packet(md: Value) -> Value {
        json!({"id": "aaaabbbb-1111", "kind": "publish-request", "metadata": md})
    }

    fn req(branch: &str, head: &str, base: &str) -> Request {
        Request {
            branch: branch.to_string(),
            head_sha: head.to_string(),
            base_sha: base.to_string(),
            requested_by: "ws-test".to_string(),
        }
    }

    const A40: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B40: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn a_request_reads_off_the_packet_metadata() {
        let j = packet(json!({
            "branch": "feat/x", "head_sha": A40, "base_sha": B40,
            "bundle_b64": "aGk=", "requested_by": "ws-7",
        }));
        let r = parse_request(&j).expect("parses");
        assert_eq!(r.branch, "feat/x");
        assert_eq!(r.head_sha, A40);
        assert_eq!(r.base_sha, B40);
        assert_eq!(r.requested_by, "ws-7");
        assert_eq!(bundle_field(&j).expect("bundle"), "aGk=");
    }

    /// A refusal the workspace can act on must say WHICH key was
    /// missing, not just that the packet was malformed.
    #[test]
    fn a_missing_metadata_key_is_named_in_the_error() {
        let j = packet(json!({"branch": "feat/x", "base_sha": B40, "bundle_b64": "aGk="}));
        let e = parse_request(&j).expect_err("must refuse").to_string();
        assert!(e.contains("head_sha"), "{e}");

        let no_bundle = packet(json!({"branch": "b", "head_sha": A40, "base_sha": B40}));
        let e = bundle_field(&no_bundle)
            .expect_err("must refuse")
            .to_string();
        assert!(e.contains("bundle_b64"), "{e}");
    }

    #[test]
    fn the_branch_ref_wins_when_the_bundle_names_it() {
        let heads = format!("{B40} HEAD\n{A40} refs/heads/feat/x\n");
        assert_eq!(bundle_tip(&heads, "feat/x").expect("tip"), A40);
    }

    /// `git bundle create f origin/main..HEAD` records only `HEAD` —
    /// the shape a workspace's own bundling instructions produce.
    #[test]
    fn a_head_only_bundle_still_yields_a_tip() {
        let heads = format!("{A40} HEAD\n");
        assert_eq!(bundle_tip(&heads, "feat/x").expect("tip"), A40);
    }

    #[test]
    fn a_single_foreign_ref_is_accepted_because_the_sha_gate_still_holds() {
        // The tip is compared to head_sha next, so the NAME being off
        // cannot smuggle content — the sha pins it.
        let heads = format!("{A40} refs/heads/feat/other\n");
        assert_eq!(bundle_tip(&heads, "feat/x").expect("tip"), A40);
    }

    #[test]
    fn an_ambiguous_bundle_refuses_to_guess() {
        let heads = format!("{A40} refs/heads/feat/a\n{B40} refs/heads/feat/b\n");
        let e = bundle_tip(&heads, "feat/x")
            .expect_err("must refuse")
            .to_string();
        assert!(e.contains("feat/a") && e.contains("feat/b"), "{e}");

        assert!(bundle_tip("", "feat/x").is_err(), "no heads at all");
    }

    /// THE e7b3a044 RULE. The packet names a head; the bundle carries
    /// one. When they disagree the refusal must name BOTH, or the
    /// workspace cannot tell which half of its filing lied.
    #[test]
    fn a_tip_mismatch_names_both_shas() {
        let detail = tip_refusal(A40, B40).expect("must refuse");
        assert!(detail.contains(A40), "must name the bundle tip: {detail}");
        assert!(
            detail.contains(B40),
            "must name the declared head: {detail}"
        );
        assert_eq!(tip_refusal(A40, A40), None, "agreement is not a refusal");
    }

    #[test]
    fn an_absent_forge_branch_proceeds_to_the_push() {
        assert_eq!(forge_decision(None, A40, "feat/x", "origin"), None);
    }

    /// Idempotence: the mission is the ref, not the transfer. A branch
    /// already at head_sha is a success on a re-drain (the previous
    /// cycle may have pushed and then lost the step-completion write).
    #[test]
    fn the_same_sha_on_the_forge_short_circuits_to_pushed() {
        match forge_decision(Some(A40), A40, "feat/x", "origin") {
            Some(Outcome::Pushed { detail }) => {
                assert!(detail.contains("already"), "{detail}");
            }
            other => panic!("must be an idempotent success: {other:?}"),
        }
    }

    #[test]
    fn a_different_sha_on_the_forge_refuses_to_force_push() {
        match forge_decision(Some(B40), A40, "feat/x", "origin") {
            Some(Outcome::Refused { detail }) => {
                assert!(detail.contains(A40) && detail.contains(B40), "{detail}");
                assert!(detail.contains("force"), "{detail}");
            }
            other => panic!("must refuse, never force-push: {other:?}"),
        }
    }

    /// Validate before spawning anything — these values travel straight
    /// into git argv.
    #[test]
    fn unsafe_branches_and_malformed_shas_are_refused_before_git() {
        for bad in ["a b", "a;rm -rf /", "--force", ""] {
            assert!(
                refusal_before_git(&req(bad, A40, B40)).is_some(),
                "branch {bad:?} must be refused"
            );
        }
        // main is never published by request — trains own main.
        assert!(refusal_before_git(&req("main", A40, B40)).is_some());
        // A short or symbolic sha is a name that can drift from content.
        for bad in ["abc123", "HEAD", "refs/heads/feat/x", ""] {
            assert!(
                refusal_before_git(&req("feat/x", bad, B40)).is_some(),
                "head_sha {bad:?} must be refused"
            );
            assert!(
                refusal_before_git(&req("feat/x", A40, bad)).is_some(),
                "base_sha {bad:?} must be refused"
            );
        }
        assert_eq!(refusal_before_git(&req("feat/x", A40, B40)), None);
    }

    #[test]
    fn whitespace_in_the_transport_encoding_is_tolerated() {
        // `base64` without -w0 wraps lines; the payload is the same.
        let wrapped = "aGVs\nbG8=\n";
        assert_eq!(decode_bundle(wrapped).expect("decodes"), b"hello");
        assert!(decode_bundle("not base64!!!").is_err());
    }

    // -----------------------------------------------------------------
    // Git-side, against fixture repositories. NEVER the invoking repo
    // (5b65c2a8): every repository lives under an explicit temp dir and
    // every git call is `-C <that dir>`.
    // -----------------------------------------------------------------

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git_ok(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn rev(dir: &Path, refname: &str) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", refname])
            .output()
            .expect("rev-parse");
        assert!(out.status.success(), "rev-parse {refname}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    struct Fx {
        root: PathBuf,
        forge: PathBuf,
        conductor: PathBuf,
        ws: PathBuf,
    }

    /// A bare forge, a workspace that files requests, and a conductor
    /// clone whose forge remote is deliberately named `forge` — NOT
    /// `origin` or `fork` — so any hardcoded remote name in the code
    /// under test fails here instead of in production.
    fn fixture(name: &str) -> (Scratch, Fx) {
        let root = std::env::temp_dir().join(format!("boss-pubreq-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir root");
        let guard = Scratch(root.clone());

        let forge = root.join("forge.git");
        std::fs::create_dir_all(&forge).expect("mkdir forge");
        let out = std::process::Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .arg(&forge)
            .output()
            .expect("init bare");
        assert!(out.status.success());

        let ws = root.join("ws");
        std::fs::create_dir_all(&ws).expect("mkdir ws");
        git_ok(&ws, &["init", "-b", "main"]);
        git_ok(&ws, &["config", "user.email", "t@example.com"]);
        git_ok(&ws, &["config", "user.name", "t"]);
        std::fs::write(ws.join("README"), name).expect("write");
        git_ok(&ws, &["add", "-A"]);
        git_ok(&ws, &["commit", "-qm", "base"]);
        git_ok(
            &ws,
            &["remote", "add", "forge", forge.to_str().expect("utf8")],
        );
        git_ok(&ws, &["push", "-q", "forge", "main"]);

        git_ok(
            &root,
            &["clone", "-q", "-o", "forge", "forge.git", "conductor"],
        );
        let conductor = root.join("conductor");

        (
            guard,
            Fx {
                root,
                forge,
                conductor,
                ws,
            },
        )
    }

    /// N commits on a new branch in the workspace; main stays put.
    fn make_branch(fx: &Fx, branch: &str, commits: usize) {
        git_ok(&fx.ws, &["checkout", "-qb", branch]);
        for i in 0..commits {
            std::fs::write(fx.ws.join("work"), format!("{branch}-{i}")).expect("write");
            git_ok(&fx.ws, &["add", "-A"]);
            git_ok(&fx.ws, &["commit", "-qm", &format!("work {i}")]);
        }
        git_ok(&fx.ws, &["checkout", "-q", "main"]);
    }

    fn make_bundle(fx: &Fx, name: &str, range: &str) -> PathBuf {
        let path = fx.root.join(name);
        git_ok(
            &fx.ws,
            &["bundle", "create", path.to_str().expect("utf8"), range],
        );
        path
    }

    /// What the forge itself holds — read from the bare repo, not from
    /// any clone's tracking ref.
    fn forge_holds(fx: &Fx, branch: &str) -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&fx.forge)
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .output()
            .expect("rev-parse");
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    #[test]
    fn a_valid_bundle_lands_the_branch_on_the_forge() {
        let (_g, fx) = fixture("valid");
        make_branch(&fx, "feat/x", 1);
        let head = rev(&fx.ws, "feat/x");
        let base = rev(&fx.ws, "main");
        let bundle = make_bundle(&fx, "x.bundle", "main..feat/x");

        let out = fulfil(
            &fx.conductor,
            "forge",
            &req("feat/x", &head, &base),
            &bundle,
        )
        .expect("fulfil");
        match out {
            Outcome::Pushed { detail } => {
                assert!(detail.contains(&head), "the detail names the sha: {detail}");
            }
            other => panic!("a valid bundle must push: {other:?}"),
        }
        assert_eq!(
            forge_holds(&fx, "feat/x").as_deref(),
            Some(head.as_str()),
            "the forge must hold the branch at exactly the declared head"
        );
    }

    /// The packet names one head; the bundle carries another. Refuse,
    /// naming both (e7b3a044) — and the forge must be untouched.
    #[test]
    fn a_bundle_tip_that_contradicts_the_declared_head_is_refused() {
        let (_g, fx) = fixture("tipmismatch");
        make_branch(&fx, "feat/x", 1);
        let tip = rev(&fx.ws, "feat/x");
        let base = rev(&fx.ws, "main");
        let bundle = make_bundle(&fx, "x.bundle", "main..feat/x");

        // Declare main's sha as the head — a real sha, just not the
        // bundle's content.
        let out = fulfil(
            &fx.conductor,
            "forge",
            &req("feat/x", &base, &base),
            &bundle,
        )
        .expect("fulfil");
        match out {
            Outcome::Refused { detail } => {
                assert!(detail.contains(&tip), "must name the bundle tip: {detail}");
                assert!(
                    detail.contains(&base),
                    "must name the declared head: {detail}"
                );
            }
            other => panic!("a lying head_sha must refuse: {other:?}"),
        }
        assert_eq!(forge_holds(&fx, "feat/x"), None, "nothing may be pushed");
    }

    /// base_sha must be an ancestor of FORGE main — a request based on
    /// history the forge never had cannot be gated against it.
    #[test]
    fn a_base_outside_forge_main_history_is_refused() {
        let (_g, fx) = fixture("badbase");
        make_branch(&fx, "feat/x", 2);
        let head = rev(&fx.ws, "feat/x");
        // The FIRST branch commit: real, carried by the bundle's
        // history claim, and on no branch the forge knows.
        let base = rev(&fx.ws, "feat/x~1");
        let bundle = make_bundle(&fx, "x.bundle", "main..feat/x");

        let out = fulfil(
            &fx.conductor,
            "forge",
            &req("feat/x", &head, &base),
            &bundle,
        )
        .expect("fulfil");
        match out {
            Outcome::Refused { detail } => {
                assert!(detail.contains(&base), "must name the base: {detail}");
            }
            other => panic!("a base off main must refuse: {other:?}"),
        }
        assert_eq!(forge_holds(&fx, "feat/x"), None);
    }

    /// The re-drain case: the previous cycle pushed, then lost the
    /// step-completion write. The branch is already exactly where the
    /// packet asked — that is a success, not a conflict.
    #[test]
    fn a_branch_already_at_the_head_is_pushed_idempotently() {
        let (_g, fx) = fixture("idempotent");
        make_branch(&fx, "feat/x", 1);
        let head = rev(&fx.ws, "feat/x");
        let base = rev(&fx.ws, "main");
        let bundle = make_bundle(&fx, "x.bundle", "main..feat/x");
        git_ok(&fx.ws, &["push", "-q", "forge", "feat/x"]);

        let out = fulfil(
            &fx.conductor,
            "forge",
            &req("feat/x", &head, &base),
            &bundle,
        )
        .expect("fulfil");
        match out {
            Outcome::Pushed { detail } => assert!(detail.contains("already"), "{detail}"),
            other => panic!("already-at-head must read as pushed: {other:?}"),
        }
        assert_eq!(forge_holds(&fx, "feat/x").as_deref(), Some(head.as_str()));
    }

    /// NEVER FORCE-PUSH. Someone else moved the branch; the drain must
    /// not move it back, whatever the packet says.
    #[test]
    fn a_branch_at_a_different_sha_is_never_force_pushed() {
        let (_g, fx) = fixture("conflict");
        make_branch(&fx, "feat/x", 1);
        let head = rev(&fx.ws, "feat/x");
        let base = rev(&fx.ws, "main");
        let bundle = make_bundle(&fx, "x.bundle", "main..feat/x");
        // The branch moves on after the bundle was filed.
        git_ok(&fx.ws, &["checkout", "-q", "feat/x"]);
        std::fs::write(fx.ws.join("work"), "newer").expect("write");
        git_ok(&fx.ws, &["add", "-A"]);
        git_ok(&fx.ws, &["commit", "-qm", "newer"]);
        git_ok(&fx.ws, &["push", "-q", "forge", "feat/x"]);
        git_ok(&fx.ws, &["checkout", "-q", "main"]);
        let newer = forge_holds(&fx, "feat/x").expect("forge has the newer sha");

        let out = fulfil(
            &fx.conductor,
            "forge",
            &req("feat/x", &head, &base),
            &bundle,
        )
        .expect("fulfil");
        match out {
            Outcome::Refused { detail } => {
                assert!(
                    detail.contains(&head) && detail.contains(&newer),
                    "{detail}"
                );
            }
            other => panic!("a moved branch must refuse: {other:?}"),
        }
        assert_eq!(
            forge_holds(&fx, "feat/x").as_deref(),
            Some(newer.as_str()),
            "the forge must keep ITS sha — the drain never rewrites history"
        );
    }

    #[test]
    fn garbage_that_is_not_a_bundle_is_refused() {
        let (_g, fx) = fixture("garbage");
        make_branch(&fx, "feat/x", 1);
        let head = rev(&fx.ws, "feat/x");
        let base = rev(&fx.ws, "main");
        let bundle = fx.root.join("garbage.bundle");
        std::fs::write(&bundle, b"not a bundle at all").expect("write");

        let out = fulfil(
            &fx.conductor,
            "forge",
            &req("feat/x", &head, &base),
            &bundle,
        )
        .expect("fulfil");
        assert!(
            matches!(out, Outcome::Refused { .. }),
            "unverifiable bytes must refuse: {out:?}"
        );
        assert_eq!(forge_holds(&fx, "feat/x"), None);
    }
}
