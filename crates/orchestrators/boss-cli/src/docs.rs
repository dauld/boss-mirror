//! `boss docs` subcommand — reindex + flush worker.
//!
//! `boss docs reindex` hits the reindex endpoint.
//! `boss docs flush-pending` polls the queued flush jobs, applies
//! each one to its markdown file via `docs_flush::apply_decisions`,
//! commits the result, and marks the job succeeded. Failures mark
//! the job as failed with the error message.
//!
//! It commits and stops there. Getting the commit anywhere is a car's
//! job — see `resolve_push_remote` for why, and for the opt-in.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::docs_flush::{self, DecisionKind, FlushDecision};

/// The message a caller gets when `BOSS_DOCS_API` is unset. Kept apart
/// from the lookup so a test can assert what it teaches without touching
/// process environment — the same split as `gate::no_instance_message`.
pub(crate) fn no_docs_api_message() -> String {
    "BOSS_DOCS_API is not set, and this verb has no default on purpose.\n\
     It used to fall back to http://127.0.0.1:7050. On boss-gcp that is \
     not the docs API of record — it is a SECOND, older docs stack \
     holding different data (measured: the local stack showed 0 queued \
     flush jobs while the cluster held 3 holding 11 decisions), and a \
     wrong instance does not fail, it answers, which is worse — the same \
     defect class as `boss gate` before aa783636.\n\
     Set it explicitly to the docs API of record for your vantage; from \
     inside the cluster:\n    \
     BOSS_DOCS_API=http://boss-docs-internal.boss.svc.cluster.local:7050 boss docs flush-pending"
        .to_string()
}

/// The docs API this verb talks to. **NO DEFAULT, deliberately** (packet
/// 7e10d3be). A default that is right on one host and silently wrong on
/// another IS the defect: `boss_ports::url("docs")` is `127.0.0.1:7050`,
/// which on boss-gcp is a legacy stack, so `flush-pending` reported
/// success while the cluster's queued decisions went unprocessed. A verb
/// that cannot reach the right instance now reaches none.
fn api_base() -> Result<String> {
    std::env::var("BOSS_DOCS_API")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow!("{}", no_docs_api_message()))
}

fn repo_root() -> PathBuf {
    std::env::var("BOSS_REPO_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/boss"))
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct ApiFlushJob {
    id: String,
    doc_path: String,
    status: String,
    payload: ApiPayload,
}

#[derive(Deserialize, Debug)]
struct ApiPayload {
    doc_path: String,
    #[allow(dead_code)]
    base_commit_sha: String,
    decisions: Vec<ApiDecision>,
}

#[derive(Deserialize, Debug)]
struct ApiDecision {
    anchor: String,
    kind: String,
    resolution: String,
    rationale: Option<String>,
}

#[derive(Serialize)]
struct StatusUpdate<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_sha: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

pub async fn reindex() -> Result<()> {
    let url = format!("{}/api/design/reindex", api_base()?);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .send()
        .await
        .context("POST /api/design/reindex")?;
    if !resp.status().is_success() {
        anyhow::bail!("reindex failed: HTTP {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    println!(
        "reindex complete: {} docs indexed, {} deleted ({} ms)",
        body["docs_indexed"].as_u64().unwrap_or(0),
        body["docs_deleted"].as_u64().unwrap_or(0),
        body["duration_ms"].as_u64().unwrap_or(0),
    );
    Ok(())
}

pub async fn flush_pending() -> Result<()> {
    let client = reqwest::Client::new();
    let base = api_base()?;
    let root = repo_root();

    // 1. Pull queued jobs.
    let url = format!("{base}/api/design/flush-jobs?status=queued");
    let resp = client.get(&url).send().await.context("GET queued jobs")?;
    if !resp.status().is_success() {
        anyhow::bail!("fetching queued jobs: HTTP {}", resp.status());
    }
    let jobs: Vec<ApiFlushJob> = resp.json().await?;

    if jobs.is_empty() {
        println!("no pending flush jobs");
        return Ok(());
    }

    println!(
        "{} pending flush job{}",
        jobs.len(),
        if jobs.len() == 1 { "" } else { "s" }
    );

    let mut any_failed = false;
    for job in jobs {
        match process_job(&client, &base, &root, &job).await {
            Ok(sha) => {
                println!("  ✓ {} → {}", job.id, sha);
            }
            Err(e) => {
                any_failed = true;
                println!("  ✗ {} failed: {e}", job.id);
                // Mark the job as failed on the server so the UI can
                // surface the error + offer a retry.
                let _ = mark_failed(&client, &base, &job.id, &e.to_string()).await;
            }
        }
    }

    if any_failed {
        anyhow::bail!("one or more jobs failed — see output above");
    }
    Ok(())
}

async fn process_job(
    client: &reqwest::Client,
    base: &str,
    root: &Path,
    job: &ApiFlushJob,
) -> Result<String> {
    if job.status != "queued" {
        return Err(anyhow!("job {} is not queued ({})", job.id, job.status));
    }

    // 2. Mark running so concurrent workers skip it.
    mark_running(client, base, &job.id).await?;

    // 3. Translate the API payload into the pure types and apply.
    let decisions: Vec<FlushDecision> = job
        .payload
        .decisions
        .iter()
        .map(|d| {
            let kind = match d.kind.as_str() {
                "override" => DecisionKind::Override,
                _ => DecisionKind::Accept,
            };
            FlushDecision {
                anchor: d.anchor.clone(),
                kind,
                resolution: d.resolution.clone(),
                rationale: d.rationale.clone(),
            }
        })
        .collect();

    let file_path = docs_flush::locate_file(root, &job.payload.doc_path);
    let before = docs_flush::load_markdown(&file_path)?;
    let after = docs_flush::apply_decisions(&before, &decisions, docs_flush::today())
        .with_context(|| format!("applying decisions to {}", job.payload.doc_path))?;

    if before == after {
        return Err(anyhow!("decisions produced no changes to the file"));
    }

    docs_flush::save_markdown(&file_path, &after)?;

    // 4. git add + commit + push.
    let doc_slug = Path::new(&job.payload.doc_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("doc")
        .to_string();
    let commit_msg = format!(
        "docs: resolve {} question{} in {}",
        decisions.len(),
        if decisions.len() == 1 { "" } else { "s" },
        doc_slug,
    );

    run_git(root, &["add", &job.payload.doc_path])?;
    run_git(root, &["commit", "-m", &commit_msg])?;
    match resolve_push_remote(|k| std::env::var(k).ok()) {
        None => {
            eprintln!("  (committed only — set BOSS_DOCS_FLUSH_REMOTE to push)");
        }
        Some(remote) => {
            if let Err(e) = run_git(root, &["push", &remote, "HEAD"]) {
                let sha = run_git_capture(root, &["rev-parse", "HEAD"])
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| "unknown".into());
                return Err(anyhow!(
                    "commit {sha} succeeded but push to `{remote}` failed: {e}"
                ));
            }
        }
    }

    let sha = run_git_capture(root, &["rev-parse", "HEAD"])?;
    let sha = sha.trim();

    // 5. Mark succeeded on the server.
    mark_succeeded(client, base, &job.id, sha).await?;

    Ok(sha.to_string())
}

async fn mark_running(client: &reqwest::Client, base: &str, job_id: &str) -> Result<()> {
    let url = format!("{base}/api/design/flush-jobs?id={job_id}");
    let body = StatusUpdate {
        status: "running",
        commit_sha: None,
        error: None,
    };
    let resp = client.put(&url).json(&body).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("PUT running: HTTP {}", resp.status());
    }
    Ok(())
}

async fn mark_succeeded(
    client: &reqwest::Client,
    base: &str,
    job_id: &str,
    sha: &str,
) -> Result<()> {
    let url = format!("{base}/api/design/flush-jobs?id={job_id}");
    let body = StatusUpdate {
        status: "succeeded",
        commit_sha: Some(sha),
        error: None,
    };
    let resp = client.put(&url).json(&body).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("PUT succeeded: HTTP {}", resp.status());
    }
    Ok(())
}

async fn mark_failed(client: &reqwest::Client, base: &str, job_id: &str, err: &str) -> Result<()> {
    let url = format!("{base}/api/design/flush-jobs?id={job_id}");
    let body = StatusUpdate {
        status: "failed",
        commit_sha: None,
        error: Some(err),
    };
    client.put(&url).json(&body).send().await?;
    Ok(())
}

/// Where — if anywhere — a flushed commit gets pushed.
///
/// The flusher's job ends at the commit. That is David's answer to
/// design-docs-as-data Q4, which says twice that the reviewable unit
/// is "the docs car that carries the flush through a train": the
/// commit is the flush's output, and a car is what moves it.
///
/// It used to default to `origin`, and `origin` on a BOSS deployment
/// is the public GitHub mirror — the one remote whose protocol says
/// nothing reaches it without a human sign-off. So the default
/// guessed, guessed the most dangerous remote of the four, and 403'd:
/// five decisions landed in idm-kanidm on 2026-08-16 and the run
/// still exited non-zero, reporting failure for work that was safely
/// committed (f89348b5).
///
/// Pushing is therefore opt-in and named explicitly. The conductor's
/// `BOSS_TRAIN_DEPLOY_REMOTE` is deliberately NOT consulted as a
/// fallback, though the triage suggested it: that variable answers
/// "where does the conductor deploy from", the flusher usually sits
/// on `main`, and inheriting it would push design commits straight
/// past the train that Q4 says should carry them.
fn resolve_push_remote<F>(env: F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    env("BOSS_DOCS_FLUSH_REMOTE")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<()> {
    let output = crate::git_auth::command()
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("spawning git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

fn run_git_capture(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = crate::git_auth::command()
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("spawning git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8(output.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::{no_docs_api_message, resolve_push_remote};

    // The verb must refuse a silent default, and the refusal must TEACH:
    // name the variable, say there is no default, and warn that the old
    // 127.0.0.1:7050 fallback is a second stack that answers wrongly
    // (packet 7e10d3be). Asserted on the message, not the env lookup, so
    // the test is parallel-safe.
    #[test]
    fn the_missing_api_message_names_the_variable_and_the_trap() {
        let m = no_docs_api_message();
        assert!(m.contains("BOSS_DOCS_API"), "{m}");
        assert!(m.contains("no default"), "{m}");
        assert!(m.contains("127.0.0.1:7050"), "{m}");
        assert!(m.contains("boss-docs-internal"), "{m}");
    }

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| (*v).to_string())
        }
    }

    // The regression. An unconfigured flusher used to push `origin`,
    // and on this deployment `origin` is https://github.com/algedonic-dev/boss
    // — the public mirror, which needs a sign-off no automated run has.
    #[test]
    fn an_unconfigured_flush_pushes_nowhere() {
        assert_eq!(resolve_push_remote(env_of(&[])), None);
    }

    // The conductor's remote is the conductor's. Reading it here would
    // put design commits on the deploy remote's default branch without
    // the train that design-docs-as-data Q4 says carries them.
    #[test]
    fn the_conductors_deploy_remote_is_not_inherited() {
        assert_eq!(
            resolve_push_remote(env_of(&[("BOSS_TRAIN_DEPLOY_REMOTE", "forge")])),
            None
        );
    }

    #[test]
    fn pushing_happens_when_a_remote_is_named() {
        assert_eq!(
            resolve_push_remote(env_of(&[("BOSS_DOCS_FLUSH_REMOTE", "forge")])),
            Some("forge".to_string())
        );
    }

    // A variable set to nothing is a half-finished config, not a
    // request to push to a remote called "".
    #[test]
    fn a_blank_remote_is_not_a_remote() {
        assert_eq!(
            resolve_push_remote(env_of(&[("BOSS_DOCS_FLUSH_REMOTE", "   ")])),
            None
        );
    }
}
