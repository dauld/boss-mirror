//! The forge seam — GitHub and Forgejo adapters, and the CI rollup readers.

use super::*;

// ---------------------------------------------------------------------------
// The forge seam (internal-forge.md Q7a): every talk-to-the-code-host
// call goes through Forge, so internalizing Git/CI is an adapter swap
// — a ForgejoForge sibling selected by BOSS_TRAIN_FORGE — instead of
// a conductor rewrite at cutover. The GitHub adapter shells to `gh`
// exactly as before; behavior is unchanged by this refactor.
// ---------------------------------------------------------------------------

/// The code host as the conductor sees it: five verbs.
#[async_trait]
pub(super) trait Forge: Send + Sync {
    /// -> {state, mergeCommit, mergedAt, statusCheckRollup} for a PR url.
    async fn pr_info(&self, url: &str) -> Result<Value>;
    /// Open a PR head->main on repo; return its url.
    async fn pr_create(
        &self,
        repo: &str,
        head_branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String>;
    /// Squash-merge the PR. `message` is the squash commit's BODY —
    /// the consist, rendered by `squash_message` (backlog f252cb1c);
    /// the subject stays the forge's own default. Empty = no body,
    /// which is what every train carried until 2026-09-19.
    async fn merge(&self, url: &str, message: &str) -> Result<()>;
    /// Close a PR WITHOUT merging — a cancelled train's PR must not
    /// sit open inviting a merge.
    async fn close_pr(&self, url: &str) -> Result<()>;
    /// Delete `branch` from the repo car branches are pushed to.
    /// Ok(true) = deleted; Ok(false) = already gone (404) — an
    /// expected state, the repo auto-deletes merged `train/*` PR
    /// heads and hand sweeps happen. Anything else is an error.
    async fn delete_branch(&self, branch: &str) -> Result<bool>;
    /// The branch's head sha right now, or Ok(None) when the branch is
    /// not there (404). The sweep's head guard reads this: a landed
    /// car's branch is only deletable while it still points at what
    /// boarded.
    async fn branch_head(&self, branch: &str) -> Result<Option<String>>;
    /// Cancel the still-running CI runs belonging to this train, and
    /// say how many were cancelled.
    ///
    /// Cancelling a train releases its cars and closes its PR but used
    /// to leave the run burning: measured 2026-08-17, a job for the
    /// cancelled train 58 was still running 27 minutes later, holding
    /// 78.65GB across three volumes (`docker system df` reporting 0B
    /// reclaimable is the tell — they are attached to a LIVE
    /// container), and the runner is single-concurrency, so the next
    /// train's jobs all sat in `waiting` behind work for a train that
    /// no longer existed. The forge host fell from 136G free to 44G,
    /// under locomotive's own 70GB floor, so the following train would
    /// have red-ed on a preflight telling the truth about a condition
    /// nobody caused. Packet `89b27e60`.
    ///
    /// There is no rerun API on this forge, but cancel works — probed
    /// against an already-finished run so the probe could not disturb
    /// live work.
    async fn cancel_ci_runs(&self, pr_index: &str, head_sha: &str) -> Result<usize>;
}

/// `owner/name` from a clone url — https or ssh, with or without
/// `.git`: `https://github.com/dauld/boss-fork.git` and
/// `git@github.com:dauld/boss-fork` both give `dauld/boss-fork`.
pub(crate) fn repo_path(url: &str) -> String {
    let u = url.trim_end_matches('/').trim_end_matches(".git");
    let mut segs = u.rsplit(['/', ':']);
    let name = segs.next().unwrap_or_default();
    let owner = segs.next().unwrap_or_default();
    format!("{owner}/{name}")
}

struct GitHubForge {
    head_owner: String,
    /// The fork holding car branches (`owner/name`) — under GitHub
    /// the cars push to the fork, so that is where a landed car's
    /// branch gets deleted from.
    fork_repo: String,
}

#[async_trait]
impl Forge for GitHubForge {
    async fn pr_info(&self, url: &str) -> Result<Value> {
        let r = sh(&[
            "gh",
            "pr",
            "view",
            url,
            "--json",
            "state,mergeCommit,mergedAt,statusCheckRollup",
        ])?;
        serde_json::from_str(&stdout_str(&r)).context("parsing gh pr view output")
    }

    async fn pr_create(
        &self,
        repo: &str,
        head_branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        let head = format!("{}:{head_branch}", self.head_owner);
        let r = sh(&[
            "gh", "pr", "create", "--repo", repo, "--head", &head, "--base", "main", "--title",
            title, "--body", body,
        ])?;
        let out = stdout_str(&r);
        Ok(out.trim().lines().last().unwrap_or_default().to_string())
    }

    async fn merge(&self, url: &str, message: &str) -> Result<()> {
        // `--body` alone leaves gh's default subject in place, which
        // is what "keep the subject as it is today" means here.
        let mut args = vec!["gh", "pr", "merge", url, "--squash"];
        if !message.is_empty() {
            args.extend(["--body", message]);
        }
        sh(&args)?;
        Ok(())
    }

    async fn close_pr(&self, url: &str) -> Result<()> {
        sh(&["gh", "pr", "close", url])?;
        Ok(())
    }

    async fn delete_branch(&self, branch: &str) -> Result<bool> {
        let path = format!("repos/{}/git/refs/heads/{branch}", self.fork_repo);
        let r = sh_unchecked(&["gh", "api", "--method", "DELETE", &path])?;
        if r.status.success() {
            return Ok(true);
        }
        let stderr = String::from_utf8_lossy(&r.stderr);
        if stderr.contains("HTTP 404") || stderr.contains("Not Found") {
            return Ok(false);
        }
        bail!("gh api DELETE {path}: {}", stderr.trim());
    }

    /// `git/ref/heads/<branch>` — the singular form, which answers
    /// with the ONE ref; the plural `git/refs/...` answers with every
    /// ref sharing the prefix, and `feat/x` would happily return
    /// `feat/x-followup`.
    async fn branch_head(&self, branch: &str) -> Result<Option<String>> {
        let path = format!("repos/{}/git/ref/heads/{branch}", self.fork_repo);
        let r = sh_unchecked(&["gh", "api", &path])?;
        if !r.status.success() {
            let stderr = String::from_utf8_lossy(&r.stderr);
            if stderr.contains("HTTP 404") || stderr.contains("Not Found") {
                return Ok(None);
            }
            bail!("gh api {path}: {}", stderr.trim());
        }
        let v: Value =
            serde_json::from_str(&stdout_str(&r)).context("parsing gh api git/ref output")?;
        Ok(v.get("object")
            .and_then(|o| o.get("sha"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string))
    }

    /// GitHub keys runs by head sha, which `gh run list --commit`
    /// takes directly — so this adapter does not need
    /// `cancellable_run_ids`, whose whole job is working around the
    /// Forgejo shape. A failure here is logged, never propagated: the
    /// pipeline does not run on this adapter any more, and a cancel
    /// that cannot reach GitHub must still release the cars.
    async fn cancel_ci_runs(&self, _pr_index: &str, head_sha: &str) -> Result<usize> {
        if head_sha.is_empty() {
            return Ok(0);
        }
        let r = sh_unchecked(&[
            "gh",
            "run",
            "list",
            "--commit",
            head_sha,
            "--json",
            "databaseId,status",
        ])?;
        if !r.status.success() {
            log(format!(
                "cancel: could not list GitHub runs for {head_sha}: {}",
                String::from_utf8_lossy(&r.stderr).trim()
            ));
            return Ok(0);
        }
        let runs: Vec<Value> = serde_json::from_str(&stdout_str(&r)).unwrap_or_default();
        let mut cancelled = 0;
        for run in runs {
            let status = run
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !["in_progress", "queued", "waiting", "requested", "pending"].contains(&status) {
                continue;
            }
            let Some(id) = run.get("databaseId").and_then(Value::as_i64) else {
                continue;
            };
            let out = sh_unchecked(&["gh", "run", "cancel", &id.to_string()])?;
            if out.status.success() {
                cancelled += 1;
            }
        }
        Ok(cancelled)
    }
}

/// The same five verbs against the internal forge's API. PRs are
/// same-repo (no fork dance): the train branch pushes to the one
/// repo, the PR head is the bare branch name, and car branches get
/// deleted from that same repo at arrival.
struct ForgejoForge {
    base: String,
    repo: String,
    token: String,
    http: reqwest::Client,
}

impl ForgejoForge {
    fn new() -> Result<Self> {
        let base = env_or("BOSS_TRAIN_FORGE_URL", "http://10.20.0.15:3000")
            .trim_end_matches('/')
            .to_string();
        let repo = env_or("BOSS_TRAIN_FORGE_REPO", "david/boss");
        let token_file = env_or("BOSS_TRAIN_FORGE_TOKEN_FILE", "/etc/boss-train/forge.token");
        let token = fs::read_to_string(&token_file)
            .with_context(|| format!("reading {token_file}"))?
            .trim()
            .to_string();
        Ok(ForgejoForge {
            base,
            repo,
            token,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
        })
    }

    async fn api(
        &self,
        method: Method,
        path: &str,
        payload: Option<Value>,
    ) -> Result<Option<Value>> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}/api/v1{path}", self.base))
            .header("Authorization", format!("token {}", self.token))
            .header("Content-Type", "application/json");
        if let Some(p) = &payload {
            req = req.json(p);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("forge {method} {path}"))?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            bail!("forge {method} {path}: HTTP {status}: {}", body.trim());
        }
        if body.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(serde_json::from_str(&body).with_context(|| {
                format!("parsing forge {method} {path} response")
            })?))
        }
    }

    fn index(url: &str) -> String {
        url.trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// For each FAILING rollup entry, resolve the failing job's id from
    /// its `target_url` and attach a bounded tail of that job's log, so a
    /// red verdict carries WHY, not just WHICH check. This is the
    /// effectful seam; every decision about a JSON payload lives in the
    /// pure helpers above (`parse_run_job_ref`, `job_id_for_index`,
    /// `log_tail`), which are what the tests pin.
    ///
    /// STRICTLY BEST-EFFORT — this is observability. Every failure logs
    /// and continues, leaving the entry with no `log_tail`; nothing here
    /// can abort `pr_info`, the verdict, or reconcile. A run's jobs are
    /// fetched once and shared across the checks that name it (a red
    /// train usually has one run).
    async fn attach_failing_logs(&self, rollup: &mut [Value]) {
        // Phase 1: fetch each DISTINCT run's jobs once. Kept separate
        // from the attach phase so the cache insert is a plain statement,
        // not a `contains_key`-then-insert in the loop (and so no
        // `HashMap::Entry` is held across the `.await`).
        let runs: BTreeSet<i64> = rollup
            .iter()
            .filter(|e| e.get("conclusion").and_then(Value::as_str) == Some("FAILURE"))
            .filter_map(|e| e.get("target_url").and_then(Value::as_str))
            .filter_map(|u| parse_run_job_ref(u).map(|(run, _)| run))
            .collect();
        let mut run_jobs: HashMap<i64, Vec<Value>> = HashMap::new();
        for run in runs {
            match self
                .api(
                    Method::GET,
                    &format!("/repos/{}/actions/runs/{run}/jobs", self.repo),
                    None,
                )
                .await
            {
                Ok(v) => {
                    let jobs = v
                        .as_ref()
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    run_jobs.insert(run, jobs);
                }
                Err(e) => log(format!(
                    "red-log: run {run} jobs list failed (non-fatal): {e}"
                )),
            }
        }
        // Phase 2: for each failing check, resolve its job id positionally
        // and attach a bounded tail of that job's log.
        for entry in rollup.iter_mut() {
            if entry.get("conclusion").and_then(Value::as_str) != Some("FAILURE") {
                continue;
            }
            let ctx = entry
                .get("context")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let target = entry
                .get("target_url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some((run, index)) = parse_run_job_ref(target) else {
                log(format!(
                    "red-log: no run/job in target_url {target:?} for {ctx:?} — no log attached"
                ));
                continue;
            };
            let jobs = run_jobs.get(&run).map(Vec::as_slice).unwrap_or(&[]);
            let Some((job_id, note)) = job_id_for_index(jobs, index, &ctx) else {
                log(format!(
                    "red-log: job index {index} out of range for run {run} ({ctx:?}) \
                     — no log attached"
                ));
                continue;
            };
            match self.fetch_job_log_tail(job_id).await {
                Ok(tail) if !tail.is_empty() => {
                    entry["log_tail"] = json!(tail);
                    entry["log_job_id"] = json!(job_id);
                    if let Some(n) = note {
                        entry["log_note"] = json!(n);
                    }
                }
                Ok(_) => {}
                Err(e) => log(format!(
                    "red-log: job {job_id} log fetch failed for {ctx:?} (non-fatal): {e}"
                )),
            }
        }
    }

    /// Pull the tail of a job's log. A suffix `Range` request keeps a
    /// 300KB+ (test jobs: far more) log off the wire — the forge answers
    /// `206 Partial Content` — and the raw text (the logs endpoint serves
    /// `text/plain`, not JSON, so it bypasses `api`) is reduced to a
    /// bounded tail. The job id MUST be the run-jobs `id`, never the
    /// tasks-list id; see `attach_failing_logs`.
    async fn fetch_job_log_tail(&self, job_id: i64) -> Result<String> {
        let url = format!(
            "{}/api/v1/repos/{}/actions/jobs/{job_id}/logs",
            self.base, self.repo
        );
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("Range", format!("bytes=-{LOG_FETCH_BYTES}"))
            .send()
            .await
            .with_context(|| format!("forge GET job {job_id} logs"))?;
        let status = resp.status();
        // 200 (whole, small log) and 206 (Range honoured) both succeed.
        if !status.is_success() {
            bail!("forge GET job {job_id} logs: HTTP {status}");
        }
        let partial = content_range_starts_past_zero(
            resp.headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok()),
        );
        // Forgejo pads job logs with NUL bytes; they survive into the
        // packet as \u0000 and make an excerpt unreadable.
        let body = resp.text().await?.replace('\0', "");
        Ok(failing_excerpt(
            &body,
            partial,
            LOG_TAIL_LINES,
            LOG_TAIL_BYTES,
        ))
    }
}

#[async_trait]
impl Forge for ForgejoForge {
    /// Shape Forgejo's PR + combined status into the exact dict the
    /// GitHub adapter returns, so reconcile stays forge-blind.
    async fn pr_info(&self, url: &str) -> Result<Value> {
        let idx = Self::index(url);
        let pr = self
            .api(
                Method::GET,
                &format!("/repos/{}/pulls/{idx}", self.repo),
                None,
            )
            .await?
            .ok_or_else(|| anyhow!("empty PR body for {url}"))?;
        let state = if truthy(pr.get("merged")) {
            "MERGED"
        } else if pr.get("state").and_then(Value::as_str) == Some("open") {
            "OPEN"
        } else {
            "CLOSED"
        };
        let head_sha = pr
            .get("head")
            .and_then(|h| h.get("sha"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut rollup = Vec::new();
        if !head_sha.is_empty() {
            let combined = self
                .api(
                    Method::GET,
                    &format!("/repos/{}/commits/{head_sha}/status", self.repo),
                    None,
                )
                .await?;
            let statuses = combined
                .as_ref()
                .and_then(|c| c.get("statuses"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for st in &statuses {
                rollup.push(rollup_entry(st));
            }
        }
        // A red verdict names its log, not just its check: best-effort,
        // and only for the FAILING entries, so a green train pays nothing.
        self.attach_failing_logs(&mut rollup).await;
        Ok(json!({
            "state": state,
            "mergeCommit": {
                "oid": pr.get("merge_commit_sha").and_then(Value::as_str).unwrap_or_default()
            },
            // When the forge says the PR merged — the merge-lost arm's
            // evidence names it beside its own two readings (f9256445).
            "mergedAt": pr.get("merged_at").cloned().unwrap_or(Value::Null),
            "statusCheckRollup": rollup,
        }))
    }

    async fn pr_create(
        &self,
        _repo: &str,
        head_branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        let pr = self
            .api(
                Method::POST,
                &format!("/repos/{}/pulls", self.repo),
                Some(json!({
                    "head": head_branch, "base": "main",
                    "title": title, "body": body,
                })),
            )
            .await?
            .ok_or_else(|| anyhow!("empty create-PR response from the forge"))?;
        pr.get("html_url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("create-PR response without html_url"))
    }

    async fn merge(&self, url: &str, message: &str) -> Result<()> {
        let idx = Self::index(url);
        // No MergeTitleField: the forge composes its default subject,
        // `<PR title> (#<idx>)`, and appends MergeMessageField beneath
        // it — a blank one is dropped, which is the 610-commit history
        // measured on 2026-09-19 (backlog f252cb1c).
        self.api(
            Method::POST,
            &format!("/repos/{}/pulls/{idx}/merge", self.repo),
            Some(json!({"Do": "squash", "MergeMessageField": message})),
        )
        .await?;
        Ok(())
    }

    async fn close_pr(&self, url: &str) -> Result<()> {
        let idx = Self::index(url);
        self.api(
            Method::PATCH,
            &format!("/repos/{}/pulls/{idx}", self.repo),
            Some(json!({"state": "closed"})),
        )
        .await?;
        Ok(())
    }

    /// DELETE /repos/{owner}/{repo}/branches/{branch}. Not through
    /// `api()` — a 404 here is an answer (already gone), not an
    /// error, and `api()` bails on every non-2xx.
    async fn delete_branch(&self, branch: &str) -> Result<bool> {
        let resp = self
            .http
            .request(
                Method::DELETE,
                format!("{}/api/v1/repos/{}/branches/{branch}", self.base, self.repo),
            )
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await
            .with_context(|| format!("forge DELETE branches/{branch}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        let body = resp.text().await?;
        // Forgejo answers a DELETE of an absent branch with 500 and
        // `object does not exist [id: refs/heads/<b>]`, not 404 —
        // observed 2026-08-13 against branches removed out of band,
        // where it failed every reconcile AFTER the merge and deploy
        // had already succeeded, so the run reported rc=1 and re-filed
        // its arrival report each tick. Already-gone is the sweep's
        // success condition whatever status dresses it up.
        if !status.is_success() && body.contains("object does not exist") {
            return Ok(false);
        }
        if !status.is_success() {
            bail!(
                "forge DELETE /repos/{}/branches/{branch}: HTTP {status}: {}",
                self.repo,
                body.trim()
            );
        }
        Ok(true)
    }

    /// GET /repos/{owner}/{repo}/branches/{branch} — `commit.id` is
    /// the head. Not through `api()` for the same reason as the delete
    /// above: a 404 here is an answer (no such branch), not an error.
    async fn branch_head(&self, branch: &str) -> Result<Option<String>> {
        let resp = self
            .http
            .request(
                Method::GET,
                format!("{}/api/v1/repos/{}/branches/{branch}", self.base, self.repo),
            )
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await
            .with_context(|| format!("forge GET branches/{branch}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body = resp.text().await?;
        if !status.is_success() {
            bail!(
                "forge GET /repos/{}/branches/{branch}: HTTP {status}: {}",
                self.repo,
                body.trim()
            );
        }
        let v: Value = serde_json::from_str(&body)
            .with_context(|| format!("parsing forge branches/{branch} response"))?;
        Ok(v.get("commit")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string))
    }

    /// List the repo's recent runs, pick this train's still-active
    /// ones with `cancellable_run_ids`, and POST cancel to each.
    ///
    /// NEVER PROPAGATES. Cancelling CI is a courtesy to the next
    /// train; failing to do it must not abort the cancel that
    /// releases the cars, because a car stuck aboard a dead train is
    /// far worse than a run left burning. Every failure is logged and
    /// swallowed, and the count returned is what actually succeeded.
    async fn cancel_ci_runs(&self, pr_index: &str, head_sha: &str) -> Result<usize> {
        let listed = match self
            .api(
                Method::GET,
                &format!("/repos/{}/actions/runs?limit=50", self.repo),
                None,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                log(format!("cancel: could not list CI runs: {e}"));
                return Ok(0);
            }
        };
        let runs = listed
            .as_ref()
            .and_then(|v| v.get("workflow_runs"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let ids = cancellable_run_ids(&runs, pr_index, head_sha);
        let mut cancelled = 0;
        for id in ids {
            match self
                .api(
                    Method::POST,
                    &format!("/repos/{}/actions/runs/{id}/cancel", self.repo),
                    None,
                )
                .await
            {
                Ok(_) => {
                    cancelled += 1;
                    log(format!("cancel: cancelled CI run {id}"));
                }
                Err(e) => log(format!("cancel: CI run {id} would not cancel: {e}")),
            }
        }
        Ok(cancelled)
    }
}

pub(super) fn make_forge(cfg: &Config) -> Result<Box<dyn Forge>> {
    match cfg.forge_kind.as_str() {
        "github" => Ok(Box::new(GitHubForge {
            head_owner: cfg.head_owner.clone(),
            fork_repo: repo_path(&cfg.fork_url),
        })),
        "forgejo" => Ok(Box::new(ForgejoForge::new()?)),
        other => bail!("unknown BOSS_TRAIN_FORGE {other:?} — expected github or forgejo"),
    }
}

/// Collapse the forge's per-check rollup to green/pending/failing.
/// The per-check detail behind a CI verdict, as `context:state`
/// pairs — the evidence `ci_verdict` reduces to a single word and
/// then discards.
///
/// David, 2026-08-17: "especially with agent actors, we want
/// verifiable evidence like a commit hash, actual CI pass report, or
/// other data that should already be getting generated as a result of
/// actually doing the work. This should be more about accounting and
/// documenting than needing a new step or capability."
///
/// This is exactly that: the rollup is already fetched to compute the
/// verdict, so recording it costs one string and no new call. Reading
/// a red train used to mean hand-querying the forge for the run and
/// then its jobs — three API shapes, none of them obvious — to learn
/// which check failed. Now the packet says.
pub(super) fn ci_check_summary(rollup: Option<&Value>) -> String {
    let Some(items) = rollup.and_then(Value::as_array) else {
        return String::new();
    };
    items
        .iter()
        .map(|c| {
            let ctx = c
                .get("context")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("?");
            let state = c
                .get("conclusion")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| c.get("status").and_then(Value::as_str))
                .filter(|s| !s.is_empty())
                .unwrap_or("?");
            format!("{ctx}:{state}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The rollup read down to one word: `green`, `failing`, `aborted`
/// (a run was killed before it judged anything) or `pending`.
pub(super) fn ci_verdict(rollup: Option<&Value>) -> &'static str {
    let Some(items) = rollup.and_then(Value::as_array).filter(|a| !a.is_empty()) else {
        return "pending";
    };
    let states: BTreeSet<String> = items
        .iter()
        .map(|c| {
            c.get("conclusion")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    c.get("status")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or_default()
                .to_uppercase()
        })
        .collect();
    const FAILING: [&str; 3] = ["FAILURE", "TIMED_OUT", "ACTION_REQUIRED"];
    if states.iter().any(|s| FAILING.contains(&s.as_str())) {
        return "failing";
    }
    // A KILLED RUN JUDGED NOTHING. `CANCELLED` used to sit in FAILING,
    // which made an infrastructure incident indistinguishable from a
    // broken consist: on 2026-08-22 two runs died mid-flight, the
    // conductor read red, and four innocent cars took the strikes (see
    // `verdict_strikes_cars`). Ordered after the failing check on
    // purpose — a genuine failure beside a cancelled sibling is still a
    // real verdict, and the aborted sibling does not soften it.
    const SETTLED: [&str; 4] = ["SUCCESS", "NEUTRAL", "SKIPPED", "COMPLETED"];
    // A still-running sibling means the rollup has not settled — the
    // train may yet get an answer from it, cancelled neighbour or not.
    if states
        .iter()
        .any(|s| !SETTLED.contains(&s.as_str()) && s != "CANCELLED")
    {
        return "pending";
    }
    if states.iter().any(|s| s == "CANCELLED") {
        return "aborted";
    }
    "green"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_path_reads_https_and_ssh_clone_urls() {
        assert_eq!(
            repo_path("https://github.com/dauld/boss-fork.git"),
            "dauld/boss-fork"
        );
        assert_eq!(
            repo_path("git@github.com:dauld/boss-fork"),
            "dauld/boss-fork"
        );
    }

    // ---- ci_check_summary ----------------------------------------
    //
    // Shapes taken from a real Forgejo rollup: the adapter builds each
    // entry with `context` and `status`, and `conclusion` is null on
    // this forge (see cancellable_run_ids for the same lesson about
    // trusting field names).

    #[test]
    fn the_ci_summary_names_each_check_and_its_state() {
        let rollup = json!([
            {"context": "CI / fast", "status": "success", "conclusion": Value::Null},
            {"context": "CI / test", "status": "failure", "conclusion": Value::Null},
        ]);
        assert_eq!(
            ci_check_summary(Some(&rollup)),
            "CI / fast:success, CI / test:failure",
            "a red train must say WHICH check, not just that one failed"
        );
    }

    /// The verdict and the detail must agree about the same rollup —
    /// they are two readings of one fetch, and a summary that
    /// disagreed with the verdict would be worse than none.
    #[test]
    fn the_summary_and_the_verdict_read_the_same_rollup() {
        let rollup = json!([
            {"context": "a", "status": "success"},
            {"context": "b", "status": "failure"},
        ]);
        assert_eq!(ci_verdict(Some(&rollup)), "failing");
        assert!(ci_check_summary(Some(&rollup)).contains("b:failure"));
    }

    /// A run that was cancelled judged nothing. Reading it as `failing`
    /// is what struck four innocent cars on 2026-08-22 — the verdict is
    /// the one fact that decides whether a cancel counts against a car,
    /// so it has to distinguish "we looked and it is broken" from "the
    /// run was killed before it could look".
    #[test]
    fn a_cancelled_run_is_not_a_failing_verdict() {
        // Forgejo reports state in `status` with a null `conclusion`.
        let killed = json!([
            {"context": "CI / fast", "status": "success", "conclusion": Value::Null},
            {"context": "CI / test", "status": "cancelled", "conclusion": Value::Null},
        ]);
        assert_eq!(ci_verdict(Some(&killed)), "aborted");
        // A real failure alongside a cancelled sibling is still red —
        // something DID judge the consist and found it wanting.
        let judged = json!([
            {"context": "CI / fast", "status": "failure", "conclusion": Value::Null},
            {"context": "CI / test", "status": "cancelled", "conclusion": Value::Null},
        ]);
        assert_eq!(ci_verdict(Some(&judged)), "failing");
        // A cancelled run alongside one still going has not settled.
        let mid_flight = json!([
            {"context": "CI / fast", "status": "running", "conclusion": Value::Null},
            {"context": "CI / test", "status": "cancelled", "conclusion": Value::Null},
        ]);
        assert_eq!(ci_verdict(Some(&mid_flight)), "pending");
    }

    /// No rollup is not an empty rollup: pending CI has nothing to
    /// report and must not stamp a misleading empty summary as if it
    /// had looked and found nothing.
    #[test]
    fn an_absent_rollup_summarises_to_nothing() {
        assert_eq!(ci_check_summary(None), "");
        assert_eq!(ci_check_summary(Some(&json!([]))), "");
    }
}
