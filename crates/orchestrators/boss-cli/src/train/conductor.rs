//! The conductor.

use super::*;

// ---------------------------------------------------------------------------
// The conductor
// ---------------------------------------------------------------------------

pub(super) struct Conductor {
    pub(super) cfg: Config,
    http: reqwest::Client,
    forge: Box<dyn Forge>,
    /// Who the conductor's packets are filed to — the platform owner,
    /// read from the people registry through the port and cached for
    /// this process (backlog 3c23662d). Every alarm and the train's
    /// gate-run go through `owner_for_filing`; none names a person.
    owner: boss_people_client::ReqwestPlatformOwner,
    /// THE RULES THIS INVOCATION DECIDES BY — resolved once, from the
    /// registry, and threaded to every decision point below. Nothing in
    /// this file reaches for a policy constant any more; if a threshold
    /// appears in a decision here, it arrived on this field.
    policy: DeliveryPolicy,
}

impl Conductor {
    pub(super) fn new(cfg: Config, forge: Box<dyn Forge>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .default_headers({
                // Machine token (7fcd78fa phase 1): rides as a default
                // header so every jobs-API verb the conductor runs
                // carries it once the operator configures one.
                let mut h = reqwest::header::HeaderMap::new();
                boss_core::machine_token::attach(&mut h);
                h
            })
            .build()?;
        // Built on the compiled fallback so the conductor can make the
        // very API call that resolves the real one; `with_policy`
        // replaces it before any decision is taken.
        let owner = crate::owner::resolver(&cfg.jobs);
        Ok(Conductor {
            cfg,
            http,
            forge,
            owner,
            policy: DeliveryPolicy::compiled(),
        })
    }

    /// The owner a packet this loop files carries: the port's answer,
    /// or nobody with the refusal in the journal — the filing goes
    /// ahead either way, because an alarm that fell silent for want of
    /// an owner would be the failure mode this loop exists to end.
    async fn owner_for_filing(&self) -> String {
        boss_core::platform_owner::owner_for_filing(&self.owner, |e| {
            log(format!(
                "{e}; filing with no owner named — the jobs API resolves the kind's owner_role, or refuses"
            ))
        })
        .await
    }

    pub(super) fn with_policy(mut self, policy: DeliveryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Read the delivery policy in force. Never fails — an unreachable
    /// or unusable registry lands on the compiled fallback with one loud
    /// journal line (`delivery_policy::resolve_from`), because a policy
    /// registry must not become a new way to wedge every train.
    pub(super) async fn resolve_policy(&self) -> DeliveryPolicy {
        let fetched = self
            .api(
                Method::GET,
                &format!("/api/delivery/policy/{}", delivery_policy::POLICY_NAME),
                None,
            )
            .await
            .and_then(row_of_policy);
        let policy = delivery_policy::resolve_from(fetched, &|m| log(m));
        if policy.is_from_registry() {
            log(format!(
                "delivery policy v{} in force (hold {}, stall {}h)",
                policy.version, policy.max_red_trains, policy.stall_hours
            ));
        }
        policy
    }

    /// The policy a train in flight is judged by: the version it
    /// DEPARTED under, not the one in force now. A mid-flight registry
    /// edit changes the next train, never this one — the same promise a
    /// packet gets from its pinned workflow version.
    async fn policy_for(&self, train: &Value) -> DeliveryPolicy {
        let Some(version) = delivery_policy::version_to_fetch(train, &self.policy) else {
            return self.policy.clone();
        };
        let fetched = self
            .api(
                Method::GET,
                &format!(
                    "/api/delivery/policy/{}/versions/{version}",
                    delivery_policy::POLICY_NAME
                ),
                None,
            )
            .await
            .and_then(row_of_policy);
        match fetched.and_then(|row| {
            row.ok_or_else(|| anyhow!("policy v{version} is not in the registry"))
                .and_then(delivery_policy::parse)
        }) {
            Ok(pinned) => pinned,
            Err(e) => {
                // Loud, and then carry on under the active policy: a
                // train whose pin cannot be read still has to be
                // reconciled, and refusing would strand its consist.
                log(format!(
                    "delivery policy: train pinned v{version} but it could not be read ({e}) — \
                     judging it under v{} instead",
                    self.policy.version
                ));
                self.policy.clone()
            }
        }
    }

    /// Every jobs-API call the conductor makes, under the blip guard:
    /// a rolling system of record must not fail a whole verb.
    async fn api(
        &self,
        method: Method,
        path: &str,
        payload: Option<Value>,
    ) -> Result<Option<Value>> {
        retrying(
            &JOBS_API_RETRY,
            &method,
            self.policy.blip_cause_budget,
            &|m| log(m),
            || {
                let method = method.clone();
                let payload = payload.clone();
                async move { self.api_once(method, path, payload).await }
            },
        )
        .await
    }

    /// One attempt. Every way it can fail is classified on the way
    /// out, so the caller above decides retry-or-surface on evidence
    /// rather than on a string match over an error message.
    async fn api_once(
        &self,
        method: Method,
        path: &str,
        payload: Option<Value>,
    ) -> std::result::Result<Option<Value>, ApiFailure> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.cfg.jobs))
            .header("content-type", "application/json")
            .header("x-boss-user", boss_user());
        if let Some(p) = &payload {
            req = req.json(p);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ApiFailure::transport(e, format!("{method} {path}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| ApiFailure::transport(e, format!("reading {method} {path} response")))?;
        if !status.is_success() {
            return Err(ApiFailure {
                kind: Failure::Http(status.as_u16()),
                cause: anyhow!("{method} {path}: HTTP {status}: {}", body.trim()),
            });
        }
        if body.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&body)
            .map(Some)
            .map_err(|e| ApiFailure {
                kind: Failure::Malformed,
                cause: anyhow::Error::new(e).context(format!("parsing {method} {path} response")),
            })
    }

    async fn get_job(&self, id: &str) -> Result<Value> {
        self.api(Method::GET, &format!("/api/jobs/{id}"), None)
            .await?
            .ok_or_else(|| anyhow!("job {id} came back empty"))
    }

    /// File the urgent packet a red train becomes — the estate-alarm
    /// idiom (kind backlog-item, priority urgent, on the pipeline
    /// subject), keyed by `train_alert` so the overdue/watchlist
    /// machinery can see it. Dedup is the train's `red_alert_filed`
    /// flag (see `announce_red_train`), not this key.
    async fn file_train_alert(&self, tid: &str, alert: &RedTrainAlert) -> Result<()> {
        let owner = self.owner_for_filing().await;
        self.api(
            Method::POST,
            "/api/jobs",
            Some(red_train_alert_body(tid, alert, &owner)),
        )
        .await?;
        Ok(())
    }

    /// Announce a red train unless it is already announced — best-effort
    /// caller in `reconcile`. Both the flag read and the POST are
    /// fallible; the caller treats ANY error here as non-fatal, because
    /// filing an alert is observability and must never abort the pass
    /// that boards, merges, and auto-cancels. See `reconcile`.
    ///
    /// Dedup is a per-train metadata FLAG (`red_alert_filed`), mirroring
    /// `deploy_alarm_filed` / `converge_alarm_filed` — not a scan of open
    /// backlog-items. The scan it replaces read `status=open&limit=200`
    /// and treated a truncated page as "no alert exists"; once open
    /// backlog-items passed 200 the existing alert sat beyond row 200,
    /// the dedup answered "not raised", and the alert re-filed every
    /// reconcile pass (a self-compounding notification flood). The flag
    /// is stamped only after a successful file, so a failed POST leaves
    /// the train unflagged and the next pass retries.
    async fn announce_red_train(&self, train: &Value, alert: &RedTrainAlert) -> Result<()> {
        if red_alert_filed(train) {
            return Ok(());
        }
        let tid = job_id(train)?;
        self.file_train_alert(tid, alert).await?;
        self.api(
            Method::PATCH,
            &format!("/api/jobs/{tid}/metadata"),
            Some(json!({"red_alert_filed": true})),
        )
        .await?;
        log(format!(
            "train {} red — filed alert: {}",
            id8(tid),
            alert.title
        ));
        Ok(())
    }

    /// Complete `step` on `job` with evidence fields (None values are
    /// dropped, matching the python kwargs filter).
    async fn complete_step(
        &self,
        job: &Value,
        step: Option<&Value>,
        fields: &[(&str, Option<String>)],
    ) -> Result<()> {
        if step_done(step) {
            return Ok(());
        }
        let jid = job_id(job)?;
        let step = step.ok_or_else(|| anyhow!("step missing on job {}", id8(jid)))?;
        let mut md = metadata_map(step);
        for (k, v) in fields {
            if let Some(v) = v {
                md.insert((*k).to_string(), json!(v));
            }
        }
        if self.cfg.dry {
            log(format!(
                "DRY: would complete {} on {} with {}",
                step_label(step),
                id8(jid),
                py_dict(fields)
            ));
            return Ok(());
        }
        let sid = step
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("step without an id on job {jid}"))?;
        // WHEN is evidence too: steps carry only a completion DATE,
        // so the conductor stamps the instant itself — the arrival
        // report's timings derive from these.
        md.insert(
            "completed_at".to_string(),
            json!(crate::gate::stamp(Utc::now())),
        );
        self.api(
            Method::PUT,
            &format!("/api/jobs/{jid}/steps/{sid}"),
            Some(json!({"status": "completed", "metadata": md})),
        )
        .await?;
        log(completion_log_line(
            &step_label(step),
            id8(jid).as_str(),
            fields,
        ));
        Ok(())
    }

    /// update_job takes a whole Job; fetch, merge metadata, put back.
    /// The overlay itself is `overlay_metadata` — pure, and pinned by
    /// tests: PUT replaces metadata wholesale, so clobbering here
    /// would silently eat another writer's keys. A `Value::Null`
    /// value removes the key.
    /// The server now offers this merge atomically as
    /// `PATCH /api/jobs/{id}/metadata` (same null-removes convention);
    /// migrating the conductor off this client-side RMW is a follow-up.
    async fn merge_job_metadata(&self, jid: &str, kv: Vec<(&str, Value)>) -> Result<Value> {
        let mut job = self.get_job(jid).await?;
        let keys: Vec<&str> = kv.iter().map(|(k, _)| *k).collect();
        let md = overlay_metadata(&job, kv);
        job["metadata"] = Value::Object(md);
        if self.cfg.dry {
            log(format!(
                "DRY: would set {} on job {}",
                py_keys(&keys),
                id8(jid)
            ));
            return Ok(job);
        }
        self.api(Method::PUT, &format!("/api/jobs/{jid}"), Some(job.clone()))
            .await?;
        Ok(job)
    }

    /// Close each boarded car of a just-merged train, BEST-EFFORT, and
    /// return how many failed to close this pass.
    ///
    /// Each car's close is its own fallible scope: a failure LOGS a line
    /// naming the car and the loop moves on, so one bad car cannot orphan
    /// the rest. The caller completes the train's `merged` step only when
    /// this returns 0, so a partial pass is retried on the next reconcile.
    /// All three writes are idempotent — `get_job` reads, `complete_step`
    /// early-returns on a done step, `merge_job_metadata` merges — so a
    /// retry re-closes only the car that did not close before.
    async fn close_boarded_cars(
        &self,
        tid: &str,
        boarded: &[String],
        merge_ref: &str,
        pr_url: &str,
    ) -> usize {
        let mut failures = 0usize;
        for cid in boarded {
            // The car's review closes HERE, not at boarding — the change
            // was open for review until it landed, and leaving the step
            // ready while the car rides is what lets a cancelled train
            // release it (see the boarding loop). Review first, because
            // the ship-a-change spec gates `merged` on `steps.review.done
            // AND job.metadata.merged`, and the marker below is what the
            // dispatcher watches to close the Job.
            let close: Result<()> = async {
                let car = self.get_job(cid).await?;
                let review = find_step(&car, "review", "Open for review");
                if !step_done(review) {
                    self.complete_step(
                        &car,
                        review,
                        &[
                            ("pr_url", Some(pr_url.to_string())),
                            ("note", Some(format!("landed on main as {merge_ref}"))),
                        ],
                    )
                    .await?;
                }
                // v3 ship-a-change gates `merged` on this marker; the
                // dispatcher closes the Job once it is set.
                self.merge_job_metadata(
                    cid,
                    vec![("merged", json!("true")), ("merge_ref", json!(merge_ref))],
                )
                .await?;
                Ok(())
            }
            .await;
            if let Err(e) = close {
                // BEST-EFFORT: one car's failed close must not orphan the
                // rest. Count it, name it, move on — the caller holds the
                // `merged` step pending so the next reconcile retries it.
                failures += 1;
                log(format!(
                    "train {} merged, but closing car {} failed (non-fatal, retries next \
                     pass): {e}",
                    id8(tid),
                    id8(cid)
                ));
            }
        }
        failures
    }

    // -----------------------------------------------------------------------
    // Phase 1 — reconcile open trains against reality
    // -----------------------------------------------------------------------

    /// Complete a merged train's `deployed` step. The conductor deploys
    /// nothing itself: the cluster converges on forge main by itself
    /// via the forge-host cluster-deploy-runner (deployment-as-network;
    /// the migration in docs/design/the-cluster-is-the-system.md), so
    /// this touches no repository and no tree — it records the one
    /// honest evidence (NO_PLAYGROUND_DEPLOY_EVIDENCE) and returns, and
    /// the convergence-verification step downstream is what proves the
    /// cluster took the merge.
    async fn deploy(&self, train: &Value, deployed_step: &Value) -> Result<()> {
        log("deploy skipped — no playground tree; the cluster converges on forge main");
        self.complete_step(
            train,
            Some(deployed_step),
            &[("deployed", Some(NO_PLAYGROUND_DEPLOY_EVIDENCE.to_string()))],
        )
        .await?;
        Ok(())
    }

    /// Verify the CLUSTER is serving this train's merge, and complete
    /// the `converged` step with the evidence — or file the loud
    /// packet when convergence has lagged past the threshold.
    ///
    /// The proof is self-report: the jobs API's health endpoint
    /// answers with the commit its binary was BUILT from
    /// (`Capabilities.commit`, baked in by the image build). That is
    /// stronger than reading the image tag off the Deployment — a tag
    /// proves a push was requested; a running binary reporting the
    /// commit proves the pod restarted onto it.
    async fn verify_convergence(&self, train: &Value, now: DateTime<Utc>) -> Result<()> {
        let tid = job_id(train)?.to_string();
        let converged_step = find_step(train, "converged", "Cluster converged")
            .ok_or_else(|| anyhow!("converged step missing on job {}", id8(&tid)))?;
        let merge_ref = find_step(train, "merged", "Merged into main")
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("merge_ref"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("no merge_ref on job {} — nothing to verify", id8(&tid)))?
            .to_string();
        let health = self.api(Method::GET, "/api/jobs/health", None).await?;
        let cluster_commit = health
            .as_ref()
            .and_then(|h| h.get("capabilities"))
            .and_then(|c| c.get("commit"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let merged_at = parse_stamp(step_stamp(train, "merged", "Merged into main"));
        let mins_since_merge = merged_at
            .map(|m| (now.fixed_offset() - m).num_minutes())
            .unwrap_or(0);
        // Equality misses "rolled past" (see convergence_verdict); ask
        // git the ancestry question only when equality already failed,
        // against the conductor clone — which reconcile keeps fetched.
        // Any git failure (no clone yet, commit unknown to the forge)
        // reads None: converges nothing, never guesses.
        let ancestor = match cluster_commit.as_deref() {
            Some(c) if !commits_match(&merge_ref, c) => {
                let clone = self.cfg.clone.clone();
                sh_unchecked(&[
                    "git",
                    "-C",
                    &clone,
                    "merge-base",
                    "--is-ancestor",
                    &merge_ref,
                    c,
                ])
                .ok()
                .map(|o| o.status.success())
            }
            _ => None,
        };
        match convergence_verdict(
            &merge_ref,
            cluster_commit.as_deref(),
            ancestor,
            mins_since_merge,
            self.cfg.converge_alarm_mins,
        ) {
            ConvergenceVerdict::Converged => {
                let commit = cluster_commit.unwrap_or_default();
                self.complete_step(
                    train,
                    Some(converged_step),
                    &[
                        ("cluster_commit", Some(commit.clone())),
                        (
                            "verified",
                            Some(format!(
                                "the running cluster jobs API self-reports build commit \
                                 {} — matches merge {merge_ref}; verified {} min after merge",
                                id8(&commit),
                                mins_since_merge
                            )),
                        ),
                    ],
                )
                .await
            }
            ConvergenceVerdict::Waiting => {
                log(format!(
                    "train {}: cluster not yet on {} ({} min since merge, alarm at {})",
                    id8(&tid),
                    id8(&merge_ref),
                    mins_since_merge,
                    self.cfg.converge_alarm_mins
                ));
                Ok(())
            }
            ConvergenceVerdict::Overdue => {
                if truthy(
                    train
                        .get("metadata")
                        .and_then(|m| m.get("converge_alarm_filed")),
                ) {
                    return Ok(());
                }
                log(format!(
                    "train {}: cluster convergence OVERDUE ({} min since merge) — filing packet",
                    id8(&tid),
                    mins_since_merge
                ));
                if self.cfg.dry {
                    return Ok(());
                }
                let reported = cluster_commit.as_deref().unwrap_or("nothing");
                let owner = self.owner_for_filing().await;
                self.api(
                    Method::POST,
                    "/api/jobs",
                    Some(convergence_overdue_alarm_body(
                        &tid,
                        &merge_ref,
                        mins_since_merge,
                        reported,
                        self.cfg.converge_alarm_mins,
                        &owner,
                    )),
                )
                .await?;
                self.merge_job_metadata(&tid, vec![("converge_alarm_filed", json!(true))])
                    .await?;
                Ok(())
            }
        }
    }

    // `record_abandon_reason` lived here: it wrote the machine's reason
    // onto the `cancelled` terminal of a train the board had opened only
    // to abandon. A board no longer opens a packet it is not departing
    // (see "A BOARD THAT DEPARTS NO TRAIN OPENS NO PACKET"), so there is
    // no self-cancelled train left to explain — the reason it used to
    // carry is now the journal's `no train departed` line and the cars'
    // own `skip_reason`. An operator's `boss train cancel` still fills
    // the same terminal with its `--reason`, on its own path.

    /// Settle gate-runs whose runner died without reporting: complete
    /// `record-verdict` as `lost`, the terminal the workflow already
    /// provides for exactly this. NOT green and NOT failed — the checks
    /// never finished, so the run says nothing about the branch, and a
    /// verdict nobody observed would be a lie the audit log carries
    /// forever. The decision itself is `dead_gate_run_hours`, pure and
    /// tested; this is the adapter that acts on it.
    async fn reap_dead_gate_runs(&self, now: DateTime<Utc>) -> Result<()> {
        let runs = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=gate-run&status=open&limit=100",
                None,
            )
            .await?,
        )?;
        for r0 in runs {
            let rid = job_id(&r0)?.to_string();
            let run = self.get_job(&rid).await?;
            let Some(hours) = dead_gate_run_hours(&run, now) else {
                continue;
            };
            let branch = metadata_map(&run)
                .get("branch")
                .and_then(Value::as_str)
                .unwrap_or("(no branch)")
                .to_string();
            log(format!(
                "reconcile: gate-run {} ({branch}) active {hours}h with no verdict — \
                 past the {GATE_DEADLINE_HOURS}h Job deadline, settling as lost",
                id8(&rid)
            ));
            let verdict_step = find_step(&run, "record-verdict", "Record the gate verdict");
            self.complete_step(
                &run,
                verdict_step,
                &[
                    ("verdict", Some("lost".to_string())),
                    (
                        "receipt",
                        Some(format!(
                            "NO VERDICT WAS PRODUCED. Active {hours}h with none recorded, past \
                             the gate Job's {GATE_DEADLINE_HOURS}h activeDeadlineSeconds, so the \
                             pod is gone and the checks never finished. Settled as LOST by the \
                             conductor's reconcile: this run says nothing about {branch}, and an \
                             infrastructure death is not a consist failure. Re-gate for a real \
                             verdict."
                        )),
                    ),
                ],
            )
            .await?;
        }
        Ok(())
    }

    /// A change that landed buries its own verdicts. A closed gate-run
    /// whose verdict was `failed` or `lost` stays a red row on the yard's
    /// approach until a car names its branch, a later green answers it,
    /// or an operator annotates it `superseded` — and a change that went
    /// to main through the emergency lane has none of those, so its dead
    /// gate sat red on the yard for a day (fix/lean-ci-builds, lost
    /// 2026-09-04, buried by hand 2026-09-05). This is the machine's
    /// version of that annotation: if the run's sha is already an
    /// ancestor of main, the question it raised is answered by main
    /// itself. `verdict_to_bury` decides, pure and tested; this is the
    /// adapter that asks git and writes the annotation the yard reads.
    async fn bury_landed_verdicts(&self, now: DateTime<Utc>) -> Result<()> {
        let runs = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=gate-run&status=closed&limit=100",
                None,
            )
            .await?,
        )?;
        let clone = self.cfg.clone.clone();
        for run in runs {
            let Some((sha, verdict)) = verdict_to_bury(&run, now) else {
                continue;
            };
            let landed = sh_unchecked(&[
                "git",
                "-C",
                &clone,
                "merge-base",
                "--is-ancestor",
                &sha,
                "origin/main",
            ])?
            .status
            .success();
            if !landed {
                continue;
            }
            let main_sha = stdout_str(&sh_unchecked(&[
                "git",
                "-C",
                &clone,
                "rev-parse",
                "--short",
                "origin/main",
            ])?)
            .trim()
            .to_string();
            let rid = job_id(&run)?.to_string();
            let branch = metadata_map(&run)
                .get("branch")
                .and_then(Value::as_str)
                .unwrap_or("(no branch)")
                .to_string();
            log(format!(
                "reconcile: gate-run {} ({branch}) went {verdict}, but its sha {} is an ancestor of main ({main_sha}) — the change landed; burying the verdict",
                id8(&rid),
                &sha[..sha.len().min(7)]
            ));
            self.merge_job_metadata(
                &rid,
                vec![(
                    "superseded",
                    json!(format!(
                        "landed on main: {} is an ancestor of {main_sha} — the change went in without this gate (a re-gate or the emergency lane); buried by the conductor's reconcile",
                        &sha[..sha.len().min(7)]
                    )),
                )],
            )
            .await?;
        }
        Ok(())
    }

    /// The train gate as RECORDED, for a train whose ci step is already
    /// judged: the standing of the gate-run the train names, read and
    /// never filed or relaunched — the judgement is made; this keeps it
    /// in force (`train_gate::judged_verdict`). `None` when the train
    /// never had a gate-run; an unreadable run reads as pending, so a
    /// blip holds the train rather than merging it on CI alone.
    async fn recorded_train_gate(
        &self,
        t: &Value,
        tid: &str,
    ) -> (Option<crate::train_gate::Standing>, u32) {
        use crate::train_gate::{self as tg, Standing};
        let relaunches = t
            .pointer(&format!("/metadata/{}", tg::KEY_RELAUNCHES))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let Some(run_id) = t
            .pointer(&format!("/metadata/{}", tg::KEY_RUN))
            .and_then(Value::as_str)
        else {
            return (None, relaunches);
        };
        match self.get_job(run_id).await {
            Ok(run) => (Some(tg::standing(&run)), relaunches),
            Err(e) => {
                log(format!(
                    "train {}: could not re-read its judged gate-run {} this pass ({e}) — holding",
                    id8(tid),
                    id8(run_id)
                ));
                (Some(Standing::Pending), relaunches)
            }
        }
    }

    /// THE TRAIN GATE, read or filed (design 128b5496). Returns the
    /// gate's standing (None when no gate-run is on the train yet) and
    /// the relaunch count, for `train_gate::combined_verdict`. Every
    /// failure here is logged and read as "pending": a gate the
    /// conductor could not file or read this pass is filed or read the
    /// next, and the train waits — it never merges on CI alone. Two
    /// callers: `board`, once, right after the PR is recorded (so the
    /// gate starts in the pass that opens the PR, backlog 95c349a5),
    /// and the ci-step block of every `reconcile` pass, which is the
    /// retry when that first launch failed.
    async fn train_gate(
        &self,
        t: &mut Value,
        tid: &str,
        now: DateTime<Utc>,
    ) -> (Option<crate::train_gate::Standing>, u32) {
        use crate::train_gate::{self as tg, Standing};
        let relaunches = t
            .pointer(&format!("/metadata/{}", tg::KEY_RELAUNCHES))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let run_id = t
            .pointer(&format!("/metadata/{}", tg::KEY_RUN))
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(run_id) = run_id {
            let run = match self.get_job(&run_id).await {
                Ok(r) => r,
                Err(e) => {
                    log(format!(
                        "train {}: could not read its gate-run {} this pass ({e}) — reading it as running",
                        id8(tid),
                        id8(&run_id)
                    ));
                    return (Some(Standing::Pending), relaunches);
                }
            };
            let standing = tg::standing(&run);
            if tg::wants_relaunch(&standing, relaunches) {
                let why = match &standing {
                    Standing::Refused(w) => w.clone(),
                    _ => "the Job ended without a verdict".to_string(),
                };
                log(format!(
                    "train {}: {}",
                    id8(tid),
                    tg::describe(Some(&standing), relaunches)
                ));
                if !self.cfg.dry {
                    let mut refusals = t
                        .pointer(&format!("/metadata/{}", tg::KEY_REFUSALS))
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    refusals.push(json!({
                        "gate_run": run_id,
                        "why": why,
                        "at": now.to_rfc3339(),
                    }));
                    if let Err(e) = self
                        .merge_job_metadata(
                            tid,
                            vec![
                                (tg::KEY_RUN, Value::Null),
                                (tg::KEY_RELAUNCHES, json!(relaunches + 1)),
                                (tg::KEY_REFUSALS, json!(refusals)),
                            ],
                        )
                        .await
                    {
                        log(format!(
                            "train {}: could not record the gate refusal ({e}) — retrying next pass",
                            id8(tid)
                        ));
                        return (Some(standing), relaunches);
                    }
                    if let Ok(fresh) = self.get_job(tid).await {
                        *t = fresh;
                    }
                }
                return (Some(standing), relaunches + 1);
            }
            return (Some(standing), relaunches);
        }
        // No gate-run yet: file one. Not in a dry run, and not past the
        // cluster's gate bound — the next pass tries again.
        if self.cfg.dry {
            log(format!("DRY: would file a train gate for {}", id8(tid)));
            return (None, relaunches);
        }
        match self.launch_train_gate(t, tid).await {
            Ok(run_id) => {
                log(format!(
                    "train {}: train gate filed — gate-run {} on {}",
                    id8(tid),
                    id8(&run_id),
                    self.cfg.gate_namespace
                ));
                if let Err(e) = self
                    .merge_job_metadata(
                        tid,
                        vec![
                            (tg::KEY_RUN, json!(run_id)),
                            (tg::KEY_LAUNCHED_AT, json!(now.to_rfc3339())),
                        ],
                    )
                    .await
                {
                    log(format!(
                        "train {}: gate-run {} launched but could not be recorded on the train ({e}) — \
                         the next pass would file a second gate; retried",
                        id8(tid),
                        id8(&run_id)
                    ));
                }
                if let Ok(fresh) = self.get_job(tid).await {
                    *t = fresh;
                }
                (Some(Standing::Pending), relaunches)
            }
            Err(e) => {
                let failures = t
                    .pointer(&format!("/metadata/{}", tg::KEY_LAUNCH_FAILURES))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32
                    + 1;
                let why = format!("{e:#}");
                let unavailable = failures >= tg::MAX_LAUNCH_FAILURES;
                log(format!(
                    "train {}: train gate not filed this pass ({why}) — attempt {failures} of {}; {}",
                    id8(tid),
                    tg::MAX_LAUNCH_FAILURES,
                    if !unavailable {
                        "retrying next pass; the train waits"
                    } else if self.cfg.gate_required {
                        "the gate is REQUIRED, the train waits"
                    } else {
                        "reading the gate as UNAVAILABLE: CI alone judges this train, stamped on it"
                    }
                ));
                let mut kv = vec![(tg::KEY_LAUNCH_FAILURES, json!(failures))];
                if unavailable && !self.cfg.gate_required {
                    kv.push((
                        tg::KEY_FALLBACK,
                        json!(format!(
                            "the train gate could not be filed {} passes running ({why}); CI alone judged this train ({}=0)",
                            tg::MAX_LAUNCH_FAILURES,
                            tg::REQUIRED_ENV
                        )),
                    ));
                }
                if !self.cfg.dry {
                    if let Err(e2) = self.merge_job_metadata(tid, kv).await {
                        log(format!(
                            "train {}: could not record the launch failure ({e2})",
                            id8(tid)
                        ));
                    } else if let Ok(fresh) = self.get_job(tid).await {
                        *t = fresh;
                    }
                }
                if unavailable {
                    (Some(Standing::Unavailable(why)), relaunches)
                } else {
                    (None, relaunches)
                }
            }
        }
    }

    /// File the gate-run for the train branch and create its Job — the
    /// same packet body, manifest rendering and `kubectl create` that
    /// `boss gate` performs, without the operator-facing guards (the
    /// train branch is the conductor's own, freshly assembled on main).
    /// What the train's gate-run says failed (`train_gate::fails`) and
    /// why (`train_gate::fails_excerpt`), for the red-train alert — both
    /// off the one GET. Empty when the train has no gate-run or it
    /// cannot be read this pass — the alert then names what the forge
    /// names, as before; a missing name is never an error here.
    async fn train_gate_fails(&self, t: &Value) -> (Vec<String>, Vec<(String, String)>) {
        let Some(run_id) = t
            .pointer(&format!("/metadata/{}", crate::train_gate::KEY_RUN))
            .and_then(Value::as_str)
        else {
            return (Vec::new(), Vec::new());
        };
        match self.get_job(run_id).await {
            Ok(run) => (
                crate::train_gate::fails(&run),
                crate::train_gate::fails_excerpt(&run),
            ),
            Err(_) => (Vec::new(), Vec::new()),
        }
    }

    async fn launch_train_gate(&self, t: &Value, tid: &str) -> Result<String> {
        let train_ref = train_ref_of(t)
            .ok_or_else(|| anyhow!("the train carries no train_ref on its assemble step"))?;
        let (branch, short) = train_ref
            .split_once('@')
            .ok_or_else(|| anyhow!("train_ref {train_ref:?} is not <branch>@<sha>"))?;
        // The full sha from the conductor's own clone, where the branch
        // was assembled; the short one from train_ref if the clone
        // cannot answer.
        let sha = sh(&[
            "git",
            "-C",
            &self.cfg.clone,
            "rev-parse",
            "--verify",
            branch,
        ])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| stdout_str(&o).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| short.to_string());
        let manifest_text =
            std::fs::read_to_string(&self.cfg.gate_manifest).with_context(|| {
                format!(
                    "reading the gate runner manifest {} ({}=…)",
                    self.cfg.gate_manifest,
                    crate::train_gate::MANIFEST_ENV
                )
            })?;
        let ns = self.cfg.gate_namespace.as_str();
        let max = crate::gate::max_concurrent(&self.http).await?;
        let live = crate::gate::running_gates(ns)?;
        if live.len() >= max {
            bail!(
                "the cluster is at its gate bound ({} running of {max}: {})",
                live.len(),
                live.join(", ")
            );
        }
        let owner = self.owner_for_filing().await;
        let created = self
            .api(
                Method::POST,
                "/api/jobs",
                Some(crate::gate::gate_run_body(
                    branch,
                    &sha,
                    &self.cfg.gate_manifest,
                    None,
                    &owner,
                )),
            )
            .await?;
        let run_id = created
            .as_ref()
            .and_then(|c| c.get("data").unwrap_or(c).get("id"))
            .and_then(Value::as_str)
            .context("the jobs API returned no id for the train's gate-run")?
            .to_string();
        let title = t.get("title").and_then(Value::as_str).unwrap_or("PR train");
        self.api(
            Method::PATCH,
            &format!("/api/jobs/{run_id}/metadata"),
            Some(crate::train_gate::packet_marks(tid, title)),
        )
        .await
        .context("marking the gate-run as the train's")?;
        let job = crate::gate::render_job(&manifest_text, branch, &run_id, "--auto")?;
        let mut child = crate::gate::kubectl(ns)
            .args(["create", "-f", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("spawning kubectl create — is kubectl in the conductor's image?")?;
        {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .context("kubectl stdin")?
                .write_all(job.as_bytes())?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!(
                "kubectl create failed for the train gate ({}): {}",
                branch,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(run_id)
    }

    pub(super) async fn reconcile(&self, now: DateTime<Utc>) -> Result<()> {
        // Keep the clone fetched before the convergence check below asks git
        // "is the cluster's running commit a descendant of this train's
        // merge?" — a question answered AGAINST THIS CLONE. reconcile does
        // not board (only boarding called ensure_clone), so without this the
        // clone stays frozen at the last board and lacks the cluster's newer
        // commit; merge-base then exits non-zero (object unknown), is read as
        // "not an ancestor", and every train whose commit was superseded
        // between boards wedges at `converged` forever. On 2026-09-04 three
        // trains wedged exactly this way after the cutover boarded once and
        // then reconciled repeatedly against a stale clone. convergence_verdict's
        // comment claimed reconcile kept the clone fetched; it did not until
        // this line. A fetch failure is non-fatal — ancestry falls to None,
        // which converges nothing and retries next pass, the safe direction.
        // Log a failure rather than swallow it: a silent ensure_clone
        // error (the .ok() this replaces) is exactly how a broken clone
        // stayed invisible while trains wedged.
        if let Err(e) = self.ensure_clone() {
            log(format!(
                "reconcile: ensure_clone failed — ancestry-based convergence \
                 reads None this pass (converges nothing, retries): {e}"
            ));
        }
        // Bury the yard's dead before reading it. A gate pod that dies
        // without recording a verdict leaves its packet at
        // `record-verdict` forever: it holds one of the three gate slots,
        // renders as a live gate, and nothing ever clears it. On
        // 2026-09-04 two such ghosts sat there 17 hours with their
        // branches long landed, and a third silently ate a car — the
        // change was never gated and nobody noticed until a census.
        //
        // gate-runner.yaml already states the intent ("a runner that dies
        // anyway leaves an overdue packet — the alarm the protocol
        // already provides"); the alarm just had nobody listening. This
        // is the listener, and it belongs in reconcile because reconcile
        // IS the verb that makes the record match reality.
        //
        // Settling requires no cluster access, only a clock: past the
        // gate Job's own activeDeadlineSeconds, Kubernetes has already
        // killed the Job, so a packet still claiming to gate cannot be.
        if let Err(e) = self.reap_dead_gate_runs(now).await {
            log(format!("reconcile: gate-run reap failed (non-fatal): {e}"));
        }
        if let Err(e) = self.bury_landed_verdicts(now).await {
            log(format!(
                "reconcile: burying landed verdicts failed this pass (retries next): {e}"
            ));
        }
        let trains = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=pr-train&status=open&limit=50",
                None,
            )
            .await?,
        )?;
        let trains_len = trains.len();
        let mut isolated_failures = 0usize;
        for t0 in trains {
            // PER-TRAIN ISOLATION (2026-09-06). One train's failure — a
            // failed observability write, a forge blip, a merge conflict —
            // must never abort the pass and wedge every train behind it.
            // That is what froze all landings for ~8h: a red-train alert
            // POST returned 422 and, filed with `?`, aborted reconcile
            // every pass. Each iteration now runs in its own fallible
            // scope, so a sick train costs itself one pass, not the fleet.
            // (`continue` inside the loop body therefore becomes
            // `return Ok(())` — the same "skip the rest of this train".)
            let outcome: Result<()> = async {
                let tid = job_id(&t0)?.to_string();
                let mut t = self.get_job(&tid).await?;
            // The rules THIS train departed under, which may not be the
            // ones in force now.
            let policy = self.policy_for(&t).await;
            // The stall sentinel first — a train stuck BEFORE its PR
            // (assembly died, push hung) would slip past the
            // pr-step early-continues below and stall invisibly.
            self.note_stall(&t, now, &policy).await?;
            let pr_step = find_step(&t, "pr", "Open the batched PR");
            if !step_done(pr_step) {
                return Ok(()); // this window's board phase, or a stalled assembly
            }
            let pr_url = pr_step
                .and_then(|s| s.get("metadata"))
                .and_then(|m| m.get("pr_url"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if pr_url.is_empty() {
                return Ok(());
            }
            let mut info = self.forge.pr_info(&pr_url).await?;

            // THE TRAIN GATE (design 128b5496): the train's Rust checks
            // are a gate-run of the train branch on the cluster, filed
            // here while the PR is open and unjudged, and the verdict is
            // CI's and the gate's read together (`train_gate`).
            // Once judged, the gate-run is RE-READ (never relaunched)
            // and still combined: the judged arm used to recompute
            // `forge_verdict` alone, and train #361 merged with its gate
            // RED on the tick after its ci step recorded `failing`
            // (2026-09-14, 6f18390b).
            let ci_judged = step_done(find_step(&t, "ci", "CI verdict"));
            let (gate, relaunches) = if ci_judged {
                self.recorded_train_gate(&t, &tid).await
            } else {
                self.train_gate(&mut t, &tid, now).await
            };
            let ci_step = find_step(&t, "ci", "CI verdict");
            let forge_verdict = ci_verdict(info.get("statusCheckRollup"));
            let verdict = if step_done(ci_step) {
                crate::train_gate::judged_verdict(
                    forge_verdict,
                    gate.as_ref(),
                    relaunches,
                    self.cfg.gate_required,
                )
            } else {
                crate::train_gate::combined_verdict(
                    forge_verdict,
                    gate.as_ref(),
                    relaunches,
                    self.cfg.gate_required,
                )
            };
            if !step_done(ci_step) && verdict != "pending" {
                let checks = ci_check_summary(info.get("statusCheckRollup"));
                // WHY, not just WHICH: the tail of each failing job's log,
                // resolved through /actions/runs/{run}/jobs (empty on green
                // or when no log could be fetched).
                let check_logs = format_check_logs(
                    &failing_check_logs(info.get("statusCheckRollup")),
                    CI_STEP_LOG_BYTES,
                );
                let gate_line = crate::train_gate::describe(gate.as_ref(), relaunches);
                let gate_run = t
                    .pointer(&format!("/metadata/{}", crate::train_gate::KEY_RUN))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.complete_step(
                    &t,
                    ci_step,
                    &[
                        ("result", Some(verdict.to_string())),
                        // Both halves of the verdict, so a reader sees
                        // which one spoke.
                        ("forge_result", Some(forge_verdict.to_string())),
                        ("train_gate", Some(gate_line)),
                        ("train_gate_run", gate_run),
                        // WHICH check, not just that one failed.
                        ("checks", (!checks.is_empty()).then_some(checks)),
                        // The failing job's log tail — a verdict names WHY.
                        ("check_logs", (!check_logs.is_empty()).then_some(check_logs)),
                    ],
                )
                .await?;
            } else if step_done(ci_step) {
                // The step has already recorded its verdict and cannot
                // record another — terminal rows are frozen. Compare
                // against the last verdict we NOTICED (the job stamp,
                // falling back to the step's original) so this fires on
                // each change rather than on every ten-minute tick.
                let md = t.get("metadata");
                let noticed = md
                    .and_then(|m| m.get("ci_verdict_latest"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        ci_step
                            .and_then(|s| s.get("metadata"))
                            .and_then(|m| m.get("result"))
                            .and_then(Value::as_str)
                    });
                if let Some(note) = verdict_drift(noticed, verdict) {
                    log(format!("train {}: {note}", id8(&tid)));
                    if !self.cfg.dry {
                        self.merge_job_metadata(
                            &tid,
                            vec![
                                ("ci_verdict_latest", json!(verdict)),
                                ("ci_verdict_changed_at", json!(now.to_rfc3339())),
                            ],
                        )
                        .await?;
                        t = self.get_job(&tid).await?;
                    }
                }
            }

            // Asked and unanswered. Stamped once, like the stall
            // sentinel, so a hung runner produces one line rather than
            // one every ten minutes for as long as it hangs.
            if !truthy(t.get("metadata").and_then(|m| m.get("ci_overdue_since")))
                && let Some(note) = ci_overdue(&t, now, self.cfg.ci_hours)
            {
                log(format!("train {}: {note}", id8(&tid)));
                if !self.cfg.dry {
                    self.merge_job_metadata(
                        &tid,
                        vec![("ci_overdue_since", json!(now.to_rfc3339()))],
                    )
                    .await?;
                    t = self.get_job(&tid).await?;
                }
            }

            // The overnight rule, before the merge check: a train that
            // is red — or whose run was killed without judging anything
            // — AND has stopped moving releases its consist so the next
            // window can board without it. Decided on the LIVE verdict
            // just read, never on the `ci` step's first stamp. Whether
            // the release counts against the cars is a separate
            // question, and only a returned failing verdict answers it
            // yes (`verdict_strikes_cars`).
            // A red train announces ITSELF, immediately — not only when it
            // stalls out into auto-cancel below, and not only when a human
            // asks. One urgent packet naming the failing check, deduped, so
            // a red train is never a surprise (d69c4274).
            //
            // BEST-EFFORT, and that is load-bearing: filing this alert is
            // observability, and observability must NEVER abort the pass
            // that boards, merges, and auto-cancels. The first cut filed it
            // with `?`, so a malformed body (HTTP 422) aborted reconcile at
            // rc=1 every pass — one red train froze all landings for ~8h
            // (2026-09-06). Any error here now logs and the pass continues,
            // so a broken alert is at worst a missing alert, never a wedge.
            // The gate's failing checks are read only on a red pass —
            // one extra GET when there is something to name.
            let (gate_fails, gate_excerpt) = if verdict == "failing" {
                self.train_gate_fails(&t).await
            } else {
                (Vec::new(), Vec::new())
            };
            if info.get("state").and_then(Value::as_str) == Some("OPEN")
                && let Some(alert) = red_train_alert(
                    &t,
                    verdict,
                    info.get("statusCheckRollup"),
                    &gate_fails,
                    &gate_excerpt,
                )
            {
                if self.cfg.dry {
                    log(format!(
                        "DRY: would alert on red train {} ({})",
                        id8(&tid),
                        alert.title
                    ));
                } else if let Err(e) = self.announce_red_train(&t, &alert).await {
                    log(format!(
                        "train {} red — alert filing failed (non-fatal, reconcile continues): {e}",
                        id8(&tid)
                    ));
                }
            }

            // The yard's cancel button (7a24caf3): an operator's
            // `cancel_requested` stamp is honoured before the automatic
            // rule and never strikes the cars. Non-fatal by construction
            // — the method returns a bool, so nothing here can abort the
            // pass — and a request claims the train's pass whether the
            // cancel succeeded, is dry, or is being retried.
            if self
                .honour_cancel_request(&t, &tid, info.get("state").and_then(Value::as_str))
                .await
            {
                return Ok(());
            }

            if self.cfg.auto_cancel
                && info.get("state").and_then(Value::as_str) == Some("OPEN")
                && let Some(reason) = auto_cancel_reason(&t, verdict, now, policy.stall_hours)
            {
                log(format!("train {} auto-cancelling: {reason}", id8(&tid)));
                if self.cfg.dry {
                    log(format!("DRY: would cancel {} ({reason})", id8(&tid)));
                } else {
                    self.cancel_train(
                        &tid,
                        &reason,
                        verdict_strikes_cars(verdict, info.get("statusCheckRollup")),
                    )
                    .await?;
                }
                return Ok(());
            }

            let pr_state = info.get("state").and_then(Value::as_str);
            // Second lock on the same door: the ci step's RECORDED
            // verdict. The live reading above is what merges; this is
            // the frozen judgement, and a train judged failing or
            // aborted never merges whatever the live reading says —
            // the merge observed against the record, never assumed.
            let judged_red = find_step(&t, "ci", "CI verdict")
                .and_then(|s| s.get("metadata"))
                .and_then(|m| m.get("result"))
                .and_then(Value::as_str)
                .is_some_and(|r| matches!(r, "failing" | "aborted"));
            if judged_red && verdict == "green" && pr_state == Some("OPEN") {
                log(format!(
                    "train {}: live verdict green but the ci step is judged red — NOT merging (6f18390b)",
                    id8(&tid)
                ));
            }
            if self.cfg.auto_merge && verdict == "green" && !judged_red && pr_state == Some("OPEN") {
                log(format!(
                    "CI green — merging {pr_url} (train protocol 27ab7680)"
                ));
                if !self.cfg.dry {
                    self.forge.merge(&pr_url).await?;
                    info = self.forge.pr_info(&pr_url).await?;
                }
            } else if let Some(why) = merge_declined_reason(self.cfg.auto_merge, verdict, pr_state)
            {
                // A decline says so. Silence here cost 2026-09-04 hours —
                // see `merge_declined_reason`. Not stamped-once like the
                // overdue sentinel: green-and-unmerged is a train stopped
                // one step from landing, and it should read as stopped on
                // every pass until the switch is on or the operator merges.
                log(format!("CI green on {pr_url} but NOT merging — {why}"));
            }

            let merged_step = find_step(&t, "merged", "Merged into main");
            if info.get("state").and_then(Value::as_str) == Some("MERGED")
                && !step_done(merged_step)
            {
                let merge_ref: String = info
                    .get("mergeCommit")
                    .and_then(|m| m.get("oid"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .chars()
                    .take(12)
                    .collect();
                let boarded: Vec<String> = t
                    .get("metadata")
                    .and_then(|m| m.get("boarded_jobs"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                // Close every boarded car BEST-EFFORT, then complete the
                // train's `merged` step — and only when nothing failed.
                //
                // ORDER IS LOAD-BEARING. The first cut completed `merged`
                // FIRST and looped the cars with `?`: one car whose close
                // write errored aborted the (isolated) per-train scope, but
                // `merged` was already `completed`, so the retry guard above
                // (`state==MERGED && !step_done(merged)`) was false forever
                // after — the OTHER landed cars never got their close
                // markers and their car Jobs stayed open as residue,
                // inflating the open-car count and starving boarding. Now a
                // bad car costs only itself, and a partial pass leaves
                // `merged` pending so the next reconcile retries the
                // stragglers. Every close write is idempotent, so the retry
                // re-closes only the car that did not close before.
                let failures = self
                    .close_boarded_cars(&tid, &boarded, &merge_ref, &pr_url)
                    .await;
                if failures == 0 {
                    self.complete_step(&t, merged_step, &[("merge_ref", Some(merge_ref.clone()))])
                        .await?;
                } else {
                    log(format!(
                        "train {} merged, but {failures} car(s) failed to close this pass — \
                         holding the `merged` step pending so the next reconcile retries them",
                        id8(&tid)
                    ));
                }
                t = self.get_job(&tid).await?;
            }

            let merged_step = find_step(&t, "merged", "Merged into main");
            let deployed_step = find_step(&t, "deployed", "Deployed to the playground");
            if step_done(merged_step) && !step_done(deployed_step) {
                let deployed_step = deployed_step
                    .ok_or_else(|| anyhow!("deployed step missing on job {}", id8(&tid)))?;
                self.deploy(&t, deployed_step).await?;
                t = self.get_job(&tid).await?;
            }
            // Installation is not the finish line either — the cluster
            // must be SERVING the merge before the train can claim
            // arrival (fdff316c / 7e5ee013, decided 2026-08-19).
            // Trains admitted under the pre-converged spec have no
            // such step and skip this whole pass — version pinning
            // working as designed, nothing stranded.
            let converged_step = find_step(&t, "converged", "Cluster converged");
            if step_done(find_step(&t, "deployed", "Deployed to the playground"))
                && converged_step.is_some()
                && !step_done(converged_step)
                && let Err(e) = self.verify_convergence(&t, now).await
            {
                // Convergence checking must not fail the run whose
                // deploys succeeded — the next pass looks again, and
                // the overdue alarm bounds the silence.
                log(format!("convergence check failed (run stands): {e}"));
            }
                Ok(())
            }
            .await;
            if let Err(e) = outcome {
                isolated_failures += 1;
                let tid = t0
                    .get("id")
                    .and_then(Value::as_str)
                    .map(id8)
                    .unwrap_or_else(|| "?".to_string());
                log(format!(
                    "reconcile: train {tid} failed this pass — isolated, other trains continue: {e}"
                ));
            }
        }
        // Isolation must not become a blind spot. If EVERY train failed
        // this pass, that is almost never N independent per-train faults —
        // it is a systemic outage (forge / API / auth) that per-train
        // logging would scatter into noise indistinguishable from an
        // all-green pass. Say so, loudly and once, so a total downstream
        // failure surfaces rather than hiding behind the very isolation
        // that protects against the single-bad-train case.
        if trains_len > 0 && isolated_failures == trains_len {
            log(format!(
                "reconcile: ALL {trains_len} train(s) failed this pass — likely a SYSTEMIC \
                 outage (forge/API/auth), not per-train faults; investigate"
            ));
        }
        // Housekeeping must not fail a run whose real work succeeded.
        // The sweep runs last, after merges, deploys and evidence are
        // recorded; on 2026-08-13 a single un-deletable branch made
        // every reconcile report rc=1 and re-file its arrival report,
        // which reads as "the conductor is broken" when the trains had
        // in fact landed. Journal the failure, keep the verb green.
        if let Err(e) = self.sweep_landed_branches().await {
            log(format!(
                "branch sweep failed (housekeeping, run stands): {e}"
            ));
        }
        // The dock's merge preview rides the same tick (12a25f3e):
        // best-effort like the sweep — a failed preview journals and
        // the reconcile stands, because a projection that sometimes
        // lags is stale-not-wrong by design.
        if let Err(e) = self.preview_dock(now).await {
            log(format!("dock preview failed (projection, run stands): {e}"));
        }
        // A stranded green — gated, never parked — rots silently until a
        // human runs orient and reads it. This makes that detection
        // active: a green past the threshold with no car files a
        // backlog-item so the overdue/watchlist machinery can see it.
        // BEST-EFFORT, like the sweep and preview above: this froze
        // delivery for 8h once (a fatal write in reconcile stops ALL
        // landings — boss-conductor-loop-writes-must-not-be-fatal), so
        // any failure — a bad read, the POST filing the packet — journals
        // one visible line and the reconcile stands. Never .ok()-swallow:
        // a silent failure here is exactly how a strand stays unfiled.
        if let Err(e) = self.alarm_stranded_greens(now).await {
            log(format!(
                "stranded-green alarm failed (housekeeping, run stands): {e}"
            ));
        }
        Ok(())
    }

    /// File a best-effort backlog-item for each stranded green past its
    /// window that no open alarm already names, REFRESH the alarm of a
    /// strand that persists, and CLOSE the alarm of a strand that ended.
    /// Detection is `census::stranded_gate_runs` (which reads
    /// `boss_jobs::stranded`, the one definition the yard uses, §9a);
    /// the pure selection + dedup is `stranded_greens_to_alarm`; this
    /// method is only the I/O around them. Reads the same closed
    /// gate-runs orient reads (a gate-run CLOSES on its verdict, so a
    /// `status=open` query would miss every green). Returns `Err` on a
    /// read/write failure so the caller can journal it — the caller
    /// wraps this non-fatally.
    ///
    /// THE CLEAR IS NOT OPTIONAL. Raising and never revisiting the
    /// claim left four false STRANDED GREEN packets on the operator's
    /// queue on 2026-09-09, closed by hand (e60398dc): each branch
    /// parked, landed or was held within minutes of the alarm.
    async fn alarm_stranded_greens(&self, now: DateTime<Utc>) -> Result<()> {
        let gate_runs = rows(
            self.api(Method::GET, "/api/jobs?kind=gate-run&limit=60", None)
                .await?,
        )?;
        // EVERY car, not one page: at 832 ship-a-change packets on
        // 2026-09-09 the old `limit=800` was already truncated, and the
        // rows it dropped were the OLDEST — so a landed branch whose car
        // had aged off the page read as "no car" and alarmed
        // (a-limit-is-not-a-filter). Open AND closed: a landing closes
        // the car, and a closed car still answers "this branch has one".
        let cars = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!("/api/jobs?kind=ship-a-change&limit={PAGE_LIMIT}&offset={offset}"),
                None,
            )
            .await
        })
        .await?;
        let car_branches: BTreeSet<String> = cars
            .iter()
            .filter_map(|c| {
                c.get("metadata")
                    .and_then(|m| m.get("branch"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .filter(|b| !b.is_empty())
            .collect();
        // Dedup set: the branches an OPEN backlog-item alarm already
        // names in `metadata.stranded_branch`. A persisting strand is one
        // packet, not one every ten minutes. (A closed-then-still-
        // stranded green re-files — a recurrence after a human answered
        // is a new fact, the same call estate.alarm makes.)
        //
        // Read PAST page one. A bare `limit=200` treated a truncated
        // page as the whole set, so once open backlog-items passed 200
        // the existing alarm sat beyond the page, `already_alarmed`
        // missed it, and the strand re-filed every reconcile pass — a
        // self-compounding flood. `list_all_pages` pages on `total`
        // until every matching row is read (same paginator boarding and
        // the dock preview use), so the dedup set is complete.
        let open_alarms = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=backlog-item&status=open&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        let already_alarmed: BTreeSet<String> = open_alarms
            .iter()
            .filter_map(|j| {
                j.get("metadata")
                    .and_then(|m| m.get("stranded_branch"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let windows = StrandWindows {
            never_parked_mins: self.cfg.stranded_alarm_mins,
            auto_park_grace_mins: self.cfg.auto_park_grace_mins,
        };
        let to_alarm =
            stranded_greens_to_alarm(&gate_runs, &car_branches, &already_alarmed, now, windows);
        for a in &to_alarm {
            log(format!(
                "reconcile: stranded green {} — gated green {}min, no car, no open alarm; filing backlog-item",
                a.branch, a.age_mins
            ));
            if self.cfg.dry {
                continue;
            }
            let owner = self.owner_for_filing().await;
            self.api(
                Method::POST,
                "/api/jobs",
                Some(stranded_alarm_body(a, windows, now, &owner)),
            )
            .await?;
        }
        // A STANDING alarm is refreshed, never twinned: the strand is
        // still true, and the packet should carry today's measurement
        // rather than the age it was filed with (the silence sweep's
        // idiom). `already_alarmed` kept these out of `to_alarm`.
        let stranded_now: BTreeSet<String> =
            crate::census::stranded_gate_runs(&gate_runs, &car_branches)
                .into_iter()
                .collect();
        for j in &open_alarms {
            let Some(branch) = j
                .get("metadata")
                .and_then(|m| m.get("stranded_branch"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if !stranded_now.contains(branch) {
                continue;
            }
            let (Some(id), Some((gate_run_id, age_mins, park_intent))) = (
                j.get("id").and_then(Value::as_str),
                freshest_green(&gate_runs, branch, now),
            ) else {
                continue;
            };
            if self.cfg.dry {
                continue;
            }
            let measured = StrandedGreen {
                branch: branch.to_string(),
                gate_run_id,
                age_mins,
                cause: StrandCause::from_intent(park_intent),
            };
            self.api(
                Method::PATCH,
                &format!("/api/jobs/{id}/metadata"),
                Some(stranded_refresh_patch(&measured, now)),
            )
            .await?;
        }
        // AND THE ALARM CLOSES ITSELF when the claim stops holding.
        for (id, branch) in stranded_alarms_to_clear(&open_alarms, &stranded_now) {
            let why = stranded_clear_reason(&gate_runs, &car_branches, &branch);
            log(format!(
                "reconcile: stranded-green alarm {} for {branch} no longer holds ({why}); closing it",
                id8(&id)
            ));
            if self.cfg.dry {
                continue;
            }
            let job = self
                .api(Method::GET, &format!("/api/jobs/{id}"), None)
                .await?;
            let Some(job) = job else { continue };
            let Some(step) = find_step(&job, "triage", "Measure the claim, choose a route") else {
                // A packet with no triage step cannot close itself. Say
                // so once rather than silently leaving a false alarm.
                log(format!(
                    "reconcile: stranded-green alarm {} has no triage step; left open",
                    id8(&id)
                ));
                continue;
            };
            let Some(step_id) = step.get("id").and_then(Value::as_str) else {
                continue;
            };
            let existing = metadata_map(step);
            self.api(
                Method::PUT,
                &format!("/api/jobs/{id}/steps/{step_id}"),
                Some(stranded_clear_step_body(&existing, &branch, &why)),
            )
            .await?;
        }
        Ok(())
    }

    /// The dock's SHA-anchored merge preview (12a25f3e): every
    /// parked-ready car gets `metadata.merge_preview` — clean-or-
    /// conflicted vs current main, pairwise conflicts across the
    /// parked set, anchored to main@sha + a parked-set hash so a moved
    /// input reads STALE rather than wrong. Written only on CHANGE
    /// (`dock_preview::changed`): the 10-minute tick is a heartbeat,
    /// not an event source.
    async fn preview_dock(&self, now: DateTime<Utc>) -> Result<()> {
        use crate::dock_preview as dp;
        let clone = &self.cfg.clone;
        // Every parked car needs its merge preview, so read past page
        // one: a tail car left off would silently get no preview, and
        // the pairwise-conflict set would be computed over an incomplete
        // dock — a "clean" preview that hides a real conflict.
        let listed = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=ship-a-change&status=open&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        let mut cars: Vec<(String, Value, String)> = Vec::new(); // (id, job, branch)
        for j0 in listed {
            let jid = job_id(&j0)?.to_string();
            if !parked_ready(&j0) {
                continue;
            }
            let Some(branch) = j0
                .pointer("/metadata/branch")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            cars.push((jid, j0, branch));
        }
        if cars.is_empty() {
            return Ok(());
        }
        // Bring main + every parked branch into temp refs the trial
        // merges can address; refs/preview/* is cleaned each tick so a
        // deleted branch does not linger as a phantom.
        //
        // BEST-EFFORT PER BRANCH (2026-09-06). One car whose branch has
        // vanished — rerailed, deleted, or held with its branch removed —
        // must not abort the whole preview: a single combined fetch with
        // a missing refspec exits rc=128 and blanked the entire dock
        // projection every pass. `main` is required (the baseline); each
        // car branch is fetched on its own, and a car whose ref does not
        // resolve is dropped from the preview (it is not boardable anyway).
        let dir = Some(Path::new(clone.as_str()));
        sh_in(
            dir,
            true,
            &[
                "git",
                "fetch",
                "--quiet",
                "origin",
                "+refs/heads/main:refs/preview/main",
            ],
        )?;
        for (_, _, b) in &cars {
            let refspec = format!("+refs/heads/{b}:refs/preview/{b}");
            // check=false: a vanished branch is expected here and handled
            // by the resolve-and-drop below, not an error.
            let _ = sh_in(dir, false, &["git", "fetch", "--quiet", "origin", &refspec]);
        }
        let rev = |r: &str| -> Option<String> {
            let out = sh_in(dir, false, &["git", "rev-parse", "--verify", "--quiet", r]).ok()?;
            if !out.status.success() {
                return None;
            }
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!s.is_empty()).then_some(s)
        };
        let main_sha =
            rev("refs/preview/main").ok_or_else(|| anyhow!("preview: main ref did not resolve"))?;
        let mut pairs: Vec<(String, String)> = Vec::new();
        cars.retain(|(_, _, b)| match rev(&format!("refs/preview/{b}")) {
            Some(sha) => {
                pairs.push((b.clone(), sha));
                true
            }
            None => {
                log(format!(
                    "preview: branch {b} did not resolve (vanished?) — dropped from the dock preview"
                ));
                false
            }
        });
        if cars.is_empty() {
            return Ok(());
        }
        let set = dp::set_hash(clone, &pairs)?;
        let stamp = boss_jobs::car::stamp(now);

        // vs main, then pairwise. n is dock-sized (<=24 by WIP limit);
        // n^2 in-memory merges is cheap next to one real boarding.
        let mut vs_main: Vec<dp::Verdict> = Vec::new();
        for (_, _, b) in &cars {
            vs_main.push(dp::trial_merge(
                clone,
                "refs/preview/main",
                &format!("refs/preview/{b}"),
            )?);
        }
        for (i, (jid, job, b)) in cars.iter().enumerate() {
            let mut co: Vec<(String, Vec<String>)> = Vec::new();
            for (k, (_, _, other)) in cars.iter().enumerate() {
                if i == k {
                    continue;
                }
                if let dp::Verdict::Conflicts(files) = dp::trial_merge(
                    clone,
                    &format!("refs/preview/{b}"),
                    &format!("refs/preview/{other}"),
                )? {
                    co.push((other.clone(), files));
                }
            }
            let fresh = dp::preview_payload(&vs_main[i], &co, &main_sha, &set, &stamp);
            let stored = job.pointer("/metadata/merge_preview");
            if dp::changed(stored, &fresh) && !self.cfg.dry {
                self.merge_job_metadata(jid, vec![("merge_preview", fresh)])
                    .await?;
                log(format!(
                    "{}: merge preview updated (vs-main {}, {} co-boarder conflict(s))",
                    id8(jid),
                    if matches!(vs_main[i], dp::Verdict::Clean) {
                        "clean"
                    } else {
                        "CONFLICT"
                    },
                    co.len(),
                ));
            }
        }
        Ok(())
    }

    /// The stall sentinel: stamp `stalled_since` (once) when an open
    /// train's newest step completion ages past the threshold; clear
    /// the stamp when the train advances. Raising is protocol,
    /// cancelling is judgment — nothing here auto-cancels; the
    /// operator's verb for that is `boss train cancel`.
    async fn note_stall(
        &self,
        t: &Value,
        now: DateTime<Utc>,
        policy: &DeliveryPolicy,
    ) -> Result<()> {
        let tid = job_id(t)?;
        let stamped = truthy(t.get("metadata").and_then(|m| m.get("stalled_since")));
        match stall_age_hours(t, now, policy.stall_hours) {
            Some(age) if !stamped => {
                log(format!(
                    "train {} stalled ({age}h, threshold {}h)",
                    id8(tid),
                    policy.stall_hours
                ));
                // Since WHEN: the newest completion — the moment
                // progress provably stopped, not the moment the
                // sentinel happened to look.
                let since = newest_completion(t).unwrap_or_default().to_string();
                self.merge_job_metadata(tid, vec![("stalled_since", json!(since))])
                    .await?;
            }
            None if stamped => {
                log(format!("train {} advanced — stall stamp cleared", id8(tid)));
                self.merge_job_metadata(tid, vec![("stalled_since", Value::Null)])
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Reconcile's arrival sweep: delete landed cars' branches from
    /// the forge (protocol decision, David). The repo auto-deletes
    /// merged `train/*` PR heads, but each CAR branch survives its
    /// squash-merged content landing — and ancestry cannot prove the
    /// landing, so nothing git-side can ever say "safe to sweep".
    /// The job record can: once a train has closed (arrived) and a
    /// boarded car closed with the merged outcome, that branch's
    /// work is on main and the conductor deletes it. A 404 is a fine
    /// answer — something got there first, and the sweep says nothing
    /// about it (see `sweep_note`). A train with nothing left that
    /// could become deletable is stamped `branches_swept`
    /// (`sweep_complete`), so the steady state costs the list read and
    /// no per-car fetches.
    ///
    /// Cost: one jobs-list PAGE per hundred closed trains, plus per
    /// UNSWEPT train one fetch per boarded car, one `branch_head` per
    /// deletable branch, and one delete of the train's own branch (a
    /// silent 404 once it is gone). The `branches_swept` stamp bounds
    /// the per-train work, not the read — the read pages the whole
    /// closed set, because the stamp cannot bound what it has not
    /// seen.
    ///
    /// THE LEAK THIS PAGING FIXES (measured 2026-09-10, packet
    /// 02069932). This read was `limit=50` under a comment claiming
    /// coverage was never capped. It was capped at 50, and the window
    /// turns over fast: a consist check that refuses opens and closes
    /// a cancelled train about once a minute, so ~50 minutes of
    /// refusals push every arrived train off page one. A train is
    /// stamped only once all its cars are terminal, so a car still
    /// open at `proven` — the residue we spend sessions draining —
    /// leaves its train unstamped, and once the window has turned
    /// over that train is never read again and its landed branches
    /// stay on the forge for good. Self-aggravating: proof delay
    /// causes the leak, and proof delay is what we drain.
    ///
    /// Measured: 971 closed trains, eight branches of merged+closed
    /// cars still on the forge, 529 consist-refused trains closed in
    /// the preceding nine hours. Two of those eight belong to train
    /// 82a643b4, whose cars closed 46 seconds AFTER the only reconcile
    /// that pass — it was still inside the window then, so the cap is
    /// not what held those two that hour; every later reconcile exited
    /// on `another conductor run holds the lock` (a separate defect,
    /// backlog), and by the time the sweep runs again the window has
    /// turned over 10 times and the cap is what keeps them leaked.
    async fn sweep_landed_branches(&self) -> Result<()> {
        // Every closed train, not the newest page of them: the one
        // paginator this file shares with candidates,
        // open_car_branches, preview_dock and probe_dock_depth.
        let arrived = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=pr-train&status=closed&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        let pending = sweep_pending(&arrived);
        if pending.is_empty() {
            return Ok(());
        }
        // Branches still-open cars name, fetched once per pass: a
        // live car's claim beats any landed car's deletion.
        let open_branches = self.open_car_branches().await?;
        for t in pending {
            // PER-TRAIN ISOLATION (mirrors reconcile's, 2026-09-06).
            // The sweep is housekeeping and its CALLER already keeps the
            // reconcile green — but the sweep had no per-item isolation
            // of its own, so one persistent failure (a boarded car Job
            // deleted → 404 at get_job, a malformed arrival-report
            // PATCH, a forge blip on branch_head/delete_branch) aborted
            // the WHOLE sweep on a `?` every pass. Every LATER pending
            // train then went unswept and its landed `train/*` and car
            // branches accumulated on the forge — the recurring
            // forge-disk fill. Each train now runs in its own fallible
            // scope: a sick train costs itself one pass, not the fleet.
            let t_id = t.get("id").and_then(Value::as_str).unwrap_or("?");
            let outcome: Result<()> = async {
                let tid = job_id(t)?;
                let boarded: Vec<String> = t
                    .get("metadata")
                    .and_then(|m| m.get("boarded_jobs"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut cars = Vec::with_capacity(boarded.len());
                for cid in &boarded {
                    cars.push(self.get_job(cid).await?);
                }
                // The full record, once per unswept train: the arrival
                // report and the branch cleanup both read its steps,
                // which the list rows do not carry.
                let train = self.get_job(tid).await?;
                self.file_arrival_report(&train, &cars).await?;
                self.clean_arrived_train_branch(&train).await;
                // PER-BRANCH ISOLATION. One un-sweepable branch must not
                // strand the train's OTHER landed branches on the forge:
                // a `?` here would abort this train and leave its clean
                // siblings undeleted (disk debt) until the failing one
                // healed. A branch that failed also leaves the train
                // UNSTAMPED below, so it is revisited next pass rather
                // than marked swept with the branch leaked.
                let mut branch_failures = 0usize;
                // THE RECORD OF THIS PASS, one row per branch decided,
                // written on the train beside the stamp so the stamp
                // carries its evidence (1096b1a4: six leaked branches
                // under stamped trains, and the only trace of what the
                // forge had answered was a journal outside the cluster).
                let mut report: Vec<Value> = Vec::new();
                for b in deletable_branches(&cars, &open_branches) {
                    let branch_outcome: Result<()> = async {
                        // The job record proved the CONTENT landed; the head
                        // guard proves the branch still holds only that
                        // content. Both, or the branch stays (car 23923b40).
                        // For a rerail original the recorded head is the one
                        // the rerail read off it, so a commit pushed after the
                        // rerail keeps the original exactly as a commit pushed
                        // after boarding keeps a car's own branch.
                        let current = self.forge.branch_head(&b.branch).await?;
                        let guard = sweep_guard(b.head.as_deref(), current.as_deref());
                        // Verdicts that keep a branch narrate themselves, and
                        // a branch already off the forge narrates nothing.
                        if let Some(note) = sweep_note(&guard, &b) {
                            log(note);
                        }
                        match &guard {
                            SweepGuard::Gone => report.push(sweep_report_row(&b, "gone before this pass")),
                            SweepGuard::NoRecord => report.push(sweep_report_row(&b, "kept: no boarded head on record")),
                            SweepGuard::Moved { recorded, current } => report.push(sweep_report_row(
                                &b,
                                &format!("kept: moved since boarding ({} -> {})", &recorded[..recorded.len().min(8)], &current[..current.len().min(8)]),
                            )),
                            SweepGuard::Delete => {}
                        }
                        if guard == SweepGuard::Delete {
                            let what = sweep_subject(&b);
                            if self.cfg.dry {
                                log(format!(
                                    "DRY: would delete {what} (car {} landed)",
                                    id8(&b.car)
                                ));
                                return Ok(());
                            }
                            // THE DELETE IS OBSERVED, NEVER ASSUMED. The forge's
                            // answer is a claim; the branch read back afterwards
                            // is the fact. A branch still present after an
                            // answered delete is a failure of THIS branch: the
                            // train stays pending, the row says what the forge
                            // said, and the line is loud.
                            let claimed = self.forge.delete_branch(&b.branch).await?;
                            let after = self.forge.branch_head(&b.branch).await?;
                            match sweep_delete_verdict(claimed, after.as_deref()) {
                                SweepDelete::Deleted => {
                                    log(format!("deleted {what} (car {} landed)", id8(&b.car)));
                                    report.push(sweep_report_row(&b, "deleted"));
                                }
                                SweepDelete::AlreadyGone => {
                                    // It existed a moment ago — something else
                                    // swept it between the two calls. Rare, and
                                    // worth saying so it is not read as our doing.
                                    log(format!("{what} already gone (car {} landed)", id8(&b.car)));
                                    report.push(sweep_report_row(&b, "already gone"));
                                }
                                SweepDelete::StillPresent { forge_said } => {
                                    let head = after.unwrap_or_default();
                                    log(sweep_still_present_line(&b, forge_said, &head));
                                    report.push(sweep_report_row(
                                        &b,
                                        &format!("still present after DELETE answered \"{forge_said}\""),
                                    ));
                                    bail!(
                                        "{what} still on the forge after DELETE answered \"{forge_said}\""
                                    );
                                }
                            }
                        }
                        Ok(())
                    }
                    .await;
                    if let Err(e) = branch_outcome {
                        branch_failures += 1;
                        log(sweep_branch_failed_line(&b.branch, &b.car, &e));
                    }
                }
                // A branch withheld for a still-open car's claim is not
                // swept — it is deferred, and it becomes deletable the
                // moment that car closes. Named here so the train's
                // pending state has a stated reason, and counted so the
                // stamp below cannot close over it.
                let deferred = claim_deferred_branches(&cars, &open_branches);
                for b in &deferred {
                    log(claim_deferred_line(b));
                    report.push(sweep_report_row(b, "kept: a still-open car claims it"));
                }
                // A boarded car that is not terminal is the other reason a
                // train stays pending; name its branch and the step that
                // holds it, so the report is complete, not only correct.
                report.extend(unsettled_rows(&cars));
                // Stamp swept only when EVERY branch was handled: a
                // branch we could not sweep this pass must be revisited,
                // and the stamp is what drops the train off the pending
                // list. Stamping over an un-swept branch leaks it onto
                // the forge forever — the very debt this isolation
                // exists to stop. The report is written EITHER WAY, in
                // the same PUT as the stamp when there is one: an
                // unstamped train says on its own record why it is
                // still pending, and a stamped one says what each
                // branch's delete was observed to do.
                let mut kv: Vec<(&str, Value)> = vec![("sweep_report", json!(report))];
                if sweep_complete(branch_failures, deferred.len(), &cars) {
                    kv.push(("branches_swept", json!("true")));
                }
                self.merge_job_metadata(tid, kv).await?;
                Ok(())
            }
            .await;
            if let Err(e) = outcome {
                log(sweep_train_failed_line(t_id, &e));
            }
        }
        Ok(())
    }

    /// File the arrival report — the landing's final structured entry
    /// — on an arrived train's `arrived` step, once. The sweep is the
    /// conductor's visit to every arrived train, so the report is
    /// composed here from the full job record plus the boarded cars
    /// the sweep already fetched. The step PUT merges metadata (the
    /// same rule `overlay_metadata` pins): the outcome step's own
    /// keys survive the filing.
    async fn file_arrival_report(&self, train: &Value, cars: &[Value]) -> Result<()> {
        let tid = job_id(train)?;
        let Some(step) = find_step(train, "arrived", "Train arrived") else {
            return Ok(());
        };
        let filed = arrival_already_filed(train);
        // Strictly `completed` — never `skipped`: a cancelled train
        // closes with its arrived step SKIPPED, and a landing report
        // on a train that never landed would be fiction.
        let arrived = step.get("status").and_then(Value::as_str) == Some("completed");
        if !arrived || filed {
            return Ok(());
        }
        let report = arrival_report(train, cars);
        let summary = arrival_summary(&report);
        let n = report
            .get("consist")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let total = report
            .get("timings")
            .and_then(|t| t.get("total_s"))
            .and_then(Value::as_i64)
            .map_or_else(|| "?".to_string(), |s| s.to_string());
        if self.cfg.dry {
            log(format!(
                "DRY: would file the arrival report on {}",
                id8(tid)
            ));
            return Ok(());
        }
        // THE REPORT LANDS ON THE JOB, NOT THE STEP (defect f402a681).
        //
        // It used to PUT onto the `arrived` step's metadata — and the
        // guard above requires that step to be `completed`, so the only
        // write this function could ever attempt was a write to a
        // TERMINAL step. Once terminal steps became immutable, every
        // attempt returned 409 "step is terminal — these fields are
        // immutable", and because this returns Err, it took
        // `sweep_landed_branches` down with it on the `?` at the call
        // site: no arrival report AND no branch cleanup, every ten
        // minutes, for weeks.
        //
        // The 409's own hint names the fix: "To correct or annotate it,
        // write to the parent job's metadata (PATCH /api/jobs/{id}/
        // metadata) instead." That endpoint MERGES top-level keys, so
        // no overlay is needed here — the merge is the server's job.
        //
        // The report is a fact ABOUT the train, not a field of the
        // transition that recorded arrival, so the job is where it
        // belonged anyway. `summary` is written as `arrival_summary`
        // because a bare `summary` on job metadata is a name anything
        // could want.
        self.api(
            Method::PATCH,
            &format!("/api/jobs/{tid}/metadata"),
            Some(json!({"arrival_report": report, "arrival_summary": summary})),
        )
        .await?;
        log(format!(
            "arrival report on {} ({n} cars, total {total}s)",
            id8(tid)
        ));
        Ok(())
    }

    /// Housekeeping at arrival: the train's OWN branch comes off the
    /// forge once the landing is on the record — the same forge
    /// delete the cancel path has always used, now owned by the happy
    /// path too (`arrival_branch_to_delete` says when and which).
    /// Infallible by signature: a delete that fails is a journal line
    /// and the arrival stands — a leftover branch is debt, a failed
    /// arrival is an outage.
    async fn clean_arrived_train_branch(&self, train: &Value) {
        let Some(branch) = arrival_branch_to_delete(train, &self.cfg.forge_kind) else {
            return;
        };
        if self.cfg.dry {
            log(format!("DRY: would delete branch {branch} (train arrived)"));
            return;
        }
        let outcome = self.forge.delete_branch(&branch).await;
        if let Some(note) = arrival_cleanup_note(&branch, outcome) {
            log(note);
        }
    }

    /// The branches named by still-open ship-a-change cars — never
    /// deletable, whoever landed on them. Read off the list rows
    /// (the jobs list returns full metadata); an open car with no
    /// branch yet contributes nothing.
    async fn open_car_branches(&self) -> Result<BTreeSet<String>> {
        // Every open car's branch, past page one: an older open car
        // sorts to the tail, and a capped read that misses it would let
        // the sweep delete a branch a still-open car names.
        let listed = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=ship-a-change&status=open&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        Ok(listed
            .iter()
            .filter_map(|j| {
                j.get("metadata")
                    .and_then(|m| m.get("branch"))
                    .and_then(Value::as_str)
                    .filter(|b| !b.is_empty())
                    .map(str::to_string)
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Phase 2 — board this window's train
    // -----------------------------------------------------------------------

    fn ensure_clone(&self) -> Result<()> {
        let clone = &self.cfg.clone;
        if !Path::new(clone).join(".git").is_dir() {
            // A dir left from a partial/interrupted clone — present but
            // with no .git — makes `git clone` refuse ("destination
            // exists and is not empty"). Swallowed by a caller's .ok(),
            // that leaves reconcile with no clone and every superseded
            // train wedged at `converged` (2026-09-04: three trains, and
            // the reconcile ran in 0s because the clone fast-failed).
            // Clear the stale dir so the clone can proceed; a valid clone
            // has .git and never reaches here.
            if Path::new(clone).exists() {
                let _ = fs::remove_dir_all(clone);
            }
            fs::create_dir_all(&self.cfg.home)?;
            sh(&["git", "clone", &self.cfg.upstream_url, clone])?;
            sh(&[
                "git",
                "-C",
                clone,
                "remote",
                "add",
                "fork",
                &self.cfg.fork_url,
            ])?;
            // The merge commits the assembly makes need an author, and the
            // honest one is the machine that made them (a fresh clone has
            // no identity — the first real run failed exactly here).
            sh(&[
                "git",
                "-C",
                clone,
                "config",
                "user.name",
                "BOSS train conductor",
            ])?;
            sh(&[
                "git",
                "-C",
                clone,
                "config",
                "user.email",
                "train-conductor@boss.invalid",
            ])?;
        }
        sh(&["git", "-C", clone, "fetch", "origin", "--prune"])?;
        sh(&["git", "-C", clone, "fetch", "fork", "--prune"])?;
        Ok(())
    }

    /// The parked-ready cars whose branch is actually on the fork,
    /// plus the left-behind record for the ones whose branch is not
    /// — each of those gets its `skip_reason` stamped (the yard's
    /// "LEFT BEHIND" chip) and an entry for the train's own books.
    async fn candidates(&self) -> Result<(Vec<(Value, String)>, Vec<Value>)> {
        let mut out = Vec::new();
        let mut left_behind = Vec::new();
        // EVERY open car, not just page one. A car opened days ago but
        // parked today sorts to the tail (`ORDER BY opened_on DESC`), so
        // a bare `limit=` boards nothing from the tail once the backlog
        // passes a page — the silent starvation this fix exists for.
        let listed = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=ship-a-change&status=open&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        for j0 in listed {
            let jid = job_id(&j0)?.to_string();
            let j = self.get_job(&jid).await?;
            if !parked_ready(&j) {
                continue;
            }
            // The two-strike hold. Without it the auto-cancel above is
            // a loop: the same consist re-boards, goes red, cancels,
            // and burns the night landing nothing.
            if let Some(reason) = car_hold_reason(&j, self.policy.max_red_trains) {
                log(format!("{}: {reason} — leaving behind", id8(&jid)));
                left_behind.push(json!({"car_id_short": id8(&jid), "reason": reason.as_str()}));
                if !self.cfg.dry {
                    self.merge_job_metadata(&jid, vec![("skip_reason", json!(reason))])
                        .await?;
                }
                continue;
            }
            // THE DECLARED ORDERING EDGE (d3320278). Judged here, beside
            // the two-strike hold, because both answer "this car is green
            // and still must not ride yet" — a question about the car's
            // WORLD, not its content — and both are cheaper than the git
            // work below. A car with no edge costs nothing: no read is
            // made at all, which is what keeps the regression surface of
            // this change to the cars that opt in.
            if let Some(hold) = self.edge_hold(&j, &jid).await {
                log(format!("{}: {} — leaving behind", id8(&jid), hold.reason));
                left_behind.push(json!({
                    "car_id_short": id8(&jid),
                    "reason": hold.reason.as_str(),
                    EDGE_HOLD: hold.kind,
                }));
                if !self.cfg.dry {
                    self.merge_job_metadata(&jid, vec![("skip_reason", json!(hold.reason))])
                        .await?;
                }
                continue;
            }
            let branch = j
                .get("metadata")
                .and_then(|m| m.get("branch"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let ok = sh_unchecked(&[
                "git",
                "-C",
                &self.cfg.clone,
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("fork/{branch}"),
            ])?;
            // RECOVER RATHER THAN SKIP. A car is parked at review by its
            // author pushing the branch; the natural place to push is
            // the upstream the author cloned, and the fork is an
            // implementation detail of how this conductor assembles a
            // train. On 2026-08-14 that gap silently held NINE cars for
            // a whole session: the dock reported 12 parked while the
            // boardable count was 0, because `parked_ready` asks
            // "branch declared, review ready" and this asks "branch on
            // the fork" — two predicates for one question, and only the
            // first is on any dashboard.
            //
            // So if the branch exists upstream, put it on the fork and
            // board the car. Copying a ref the author already published
            // is not a judgement call; refusing to, and reporting a
            // dock depth that cannot board, is the surprising
            // behaviour. A branch that exists in NEITHER place is still
            // a real skip — that car was never pushed at all.
            // ABSENT **OR STALE**. Existence is not the question: a
            // branch already on the forge is never refreshed, so a car
            // fixed after a red train boards the commit that failed.
            let fork_sha = if ok.status.success() {
                Some(String::from_utf8_lossy(&ok.stdout).trim().to_string())
            } else {
                None
            };
            let want = car_head(&self.cfg.clone, &branch)?;
            let stale = matches!((&fork_sha, &want), (Some(f), Some(w)) if f != w);
            let mut ok = ok;
            if (!ok.status.success() || stale)
                && !self.cfg.dry
                && publish_car_branch(&self.cfg.clone, &branch)?
            {
                log(format!(
                    "{}: branch {branch} was not on the fork — published it",
                    id8(&jid)
                ));
                ok = sh_unchecked(&[
                    "git",
                    "-C",
                    &self.cfg.clone,
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("fork/{branch}"),
                ])?;
            }
            if !ok.status.success() {
                let reason = skip_reason_branch_missing(&branch);
                log(format!("{}: {reason} — leaving behind", id8(&jid)));
                left_behind.push(json!({"car_id_short": id8(&jid), "reason": reason.as_str()}));
                if !self.cfg.dry {
                    // Loud on the Job, not just in the journal: the author
                    // parked this at review believing it would board.
                    self.merge_job_metadata(&jid, vec![("skip_reason", json!(reason))])
                        .await?;
                }
                continue;
            }
            // The receipt spot-check (742d1faa): the head this car will
            // actually board must be the head its gate receipt vouches
            // for, and the receipt must be green on a clean tree.
            //
            // THE HEAD THAT BOARDS IS THE ONE ON THE FORK. The consist is
            // assembled from `fork/{branch}` (`rerail_onto_consist`) and
            // the boarded head is stamped from it, so that ref — not the
            // conductor clone's own `refs/heads` — is what the receipt
            // has to match. `car_head` prefers the LOCAL branch, which is
            // the right question for "is there anything newer to publish"
            // and the wrong answer to "what will ride". The two come
            // apart when a car is rebased and re-pushed, which is the
            // normal repair: the clone keeps the pre-rebase commit, the
            // push cannot fast-forward past it, the fork rightly keeps
            // the gated commit, and comparing the receipt against the
            // local head leaves a correctly-gated car behind for "gated,
            // then changed". Read on 2026-08-29 from a live dock — car
            // c6531868 was held out with its receipt (56b817eb) matching
            // the fork exactly, against a local ref eight hours older.
            //
            // Read AFTER the publish attempt above, so a branch that was
            // just published is judged on what actually landed there
            // rather than on what was offered.
            let boards = fork_head(&self.cfg.clone, &branch)?;
            if let Some(reason) = receipt_skip_reason(&j, boards.as_deref()) {
                log(format!("{}: {reason} — leaving behind", id8(&jid)));
                left_behind.push(json!({"car_id_short": id8(&jid), "reason": reason.as_str()}));
                if !self.cfg.dry {
                    self.merge_job_metadata(&jid, vec![("skip_reason", json!(reason))])
                        .await?;
                }
                continue;
            }
            out.push((j, branch));
        }
        Ok((out, left_behind))
    }

    /// Does this car's DECLARED ORDERING EDGE hold it back?
    /// `None` = board it (no edge, a satisfied edge, or an edge this pass
    /// could not judge).
    ///
    /// INFALLIBLE BY SIGNATURE, deliberately, and that is the whole of
    /// the safety argument. This runs inside the loop that boards every
    /// train; a `?` here would let one unreadable packet refuse the
    /// entire window, and the gate never runs the conductor, so nothing
    /// before production would have caught it (CLAUDE.md: a fallible
    /// write in reconcile froze ALL landings). So every failure becomes
    /// `Predecessor::Unreadable`, which boards the car and journals WHY
    /// — loud, per CLAUDE.md §Diagnosis, because a swallowed read is the
    /// next diagnosis paid for in advance.
    ///
    /// The 404 is separated from the blips on purpose: "there is no such
    /// Job" is an ANSWER and a hold a person must fix, while "I could not
    /// ask" is neither. `api` has already exhausted its retry budget by
    /// the time either reaches here.
    async fn edge_hold(&self, car: &Value, jid: &str) -> Option<EdgeHold> {
        let declared = declared_predecessor(car)?;
        let pred = match self
            .api(Method::GET, &format!("/api/jobs/{declared}"), None)
            .await
        {
            Ok(Some(p)) => Predecessor::Found(p),
            // A success with no body is the same fact as a 404 for this
            // question: the system of record served nothing for that id.
            Ok(None) => Predecessor::Absent,
            Err(e) if is_no_such_job(&e) => Predecessor::Absent,
            Err(e) => Predecessor::Unreadable(short_cause(&e, self.policy.blip_cause_budget)),
        };
        match boards_after_outcome(&declared, &pred) {
            EdgeOutcome::Board => None,
            EdgeOutcome::BoardUnjudged(note) => {
                log(format!("{}: {note}", id8(jid)));
                None
            }
            EdgeOutcome::Hold(h) => Some(h),
        }
    }

    async fn open_train_job(&self, train_branch: &str, window: &str) -> Result<Option<Value>> {
        // THE PIN. The train records the policy version it is departing
        // under, so an edit made while it is in flight cannot rewrite
        // the rules it left on — the same promise a packet gets from the
        // workflow version it was admitted under. Nothing is stamped
        // when the conductor fell back to compiled values: there is no
        // version, and a record that claimed one would be lying.
        let mut metadata = Map::new();
        metadata.insert("actor".to_string(), json!(ACTOR));
        for (k, v) in delivery_policy::pin_stamps(&self.policy) {
            metadata.insert(k.to_string(), v);
        }
        let payload = json!({
            "kind": "pr-train",
            "subject": {"subject_kind": "custom", "id": train_branch},
            "title": format!("PR train {window}"),
            // The conductor is a machine and says so. `resolve_owner`
            // reads any colon-bearing id as automation and places the
            // Job on an active holder of the kind's `owner_role`
            // (`platform-admin` for pr-train) — so the responsible
            // human is whoever actually holds the role today.
            //
            // This used to name `emp-bootstrap-admin` outright, which
            // survived only because that row happened to be the
            // deployment's admin. Once the bootstrap identity is
            // retired in favour of a named person, a hardcoded owner
            // is a dead id that resolution has to quietly override —
            // right by accident rather than by construction.
            "owner_id": ACTOR,
            "status": "open",
            "priority": "standard",
            "metadata": metadata,
            "tags": ["train"],
        });
        if self.cfg.dry {
            log(format!("DRY: would open train Job for {train_branch}"));
            return Ok(None);
        }
        let created = self.api(Method::POST, "/api/jobs", Some(payload)).await?;
        let jid = created
            .as_ref()
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let jid = match jid {
            Some(id) => id,
            None => {
                // some create paths return the row wrapped
                let listed = rows(
                    self.api(
                        Method::GET,
                        "/api/jobs?kind=pr-train&status=open&limit=5",
                        None,
                    )
                    .await?,
                )?;
                job_id(
                    listed
                        .first()
                        .ok_or_else(|| anyhow!("no open pr-train Job found after create"))?,
                )?
                .to_string()
            }
        };
        Ok(Some(self.get_job(&jid).await?))
    }

    /// The CI host's boarding verdict, from the estate's host-scope
    /// observation series. `BOSS_TRAIN_CI_HOST` names the estate node
    /// id of the box CI runs on; a deployment that has not configured
    /// it gets exactly the old behaviour, minus silence — one journal
    /// line says the check did not run.
    async fn ci_host_readiness(&self, now: DateTime<Utc>) -> host_readiness::Readiness {
        use crate::host_readiness::Readiness;
        let Some(host) = self.cfg.ci_host.as_deref() else {
            log("ci host check skipped — BOSS_TRAIN_CI_HOST unset");
            return Readiness::Proceed;
        };
        // `scope=host` so the page is not spent by the faster cluster
        // series (the reader's own lesson, 2026-09-02); `limit=50` is
        // its hard cap, depth enough to find this host among the other
        // host-scope observers.
        let fetched = self
            .api(
                Method::GET,
                "/api/estate/observations?scope=host&limit=50",
                None,
            )
            .await;
        match fetched {
            Ok(Some(body)) => host_readiness::host_readiness(
                &body,
                host,
                self.policy.ci_host_floor_gb,
                host_readiness::max_observation_age(),
                now,
            ),
            Ok(None) => Readiness::Unverifiable {
                reason: "the observations reader answered nothing".to_string(),
            },
            Err(e) => Readiness::Unverifiable {
                reason: format!("the observations reader is unreachable ({e})"),
            },
        }
    }

    pub(super) async fn board(&self, now: DateTime<Utc>) -> Result<()> {
        // Minute precision, not an AM/PM half-day. Boardings fire on
        // dock depth (min 4, 120m cooldown), not a twice-daily clock, so
        // the old "{date} AM/PM" label both COLLIDED — two trains carried
        // an identical "PM" the night of 2026-08-31 — and implied a
        // schedule the system does not run (21d4f433). Mirrors the
        // train_branch stamp on the next line.
        let window = now.format("%Y-%m-%d %H:%M").to_string();
        let train_branch = format!("train/{}", now.format("%Y%m%d-%H%M"));

        // THE TRACK — one train MERGING at a time. The cadence loop
        // holds a departure before it claims a window (cadence::decide),
        // so this is the backstop for a hand-run `boss train board` and
        // for two conductors racing: an open PRE-MERGE pr-train packet
        // means the previous consist has not landed on main, and a
        // second consist assembled now would merge onto a main the
        // first is about to change (a8c6773b). A MERGED train waiting
        // to deploy or converge does not hold it (`holds_the_track`):
        // the next consist merges on top of its content and converges
        // it by ancestry — holding for it deadlocked delivery twice on
        // 2026-09-07 (f3796323). No packet is opened for a hold — it is
        // not a refusal, the yard is not empty, and the next tick after
        // the track clears departs.
        //
        // Every page: the list rows carry `steps` (http/jobs.rs enriches
        // each row), which is what the predicate reads, and the one
        // pre-merge train that matters may sit behind merged ones
        // waiting to converge — a limit is not a filter.
        let on_track = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!("/api/jobs?kind=pr-train&status=open&limit={PAGE_LIMIT}&offset={offset}"),
                None,
            )
            .await
        })
        .await?;
        if let Some(occupant) = track_occupied_by(&on_track) {
            log(format!("BOARDING HELD — track occupied by {occupant}"));
            return Ok(());
        }

        // THE HOST CHECK — before anything is assembled. On 2026-09-03
        // the conductor boarded two consists onto a CI host whose disk
        // was full, and each burned a full CI cycle discovering it; the
        // locomotive's run-start floor had even PASSED at 01:26,
        // because a start-of-run check cannot see a consist's
        // mid-flight consumption. So the question is asked here, from
        // the estate's observed series, before the first merge is
        // attempted (David, 2026-09-03: "protocol should actually
        // verify before anyone bothers to even start").
        //
        // Only a POSITIVE "the host is short" refuses. Unverifiable —
        // an absent, stale, or unreadable series — proceeds with one
        // loud line, deliberately FAIL-OPEN: the host-scope observer
        // (infra/estate/observe-host.sh) is not yet installed anywhere,
        // and landing this check must not stop all boarding on the day
        // the series does not exist yet. Once the series is live,
        // tightening stale-to-refuse is a policy question, not a
        // rebuild.
        match self.ci_host_readiness(now).await {
            host_readiness::Readiness::Refuse { reason } => {
                // The refusal is the journal's, not a packet's (see "A
                // BOARD THAT DEPARTS NO TRAIN OPENS NO PACKET"). The
                // condition itself — a host short of disk — is already
                // a packet: the estate observer files and refreshes one
                // for the host, and it does not arrive once a minute.
                log(no_departure_line(&NoDeparture::HostShort { reason }));
                return Ok(());
            }
            host_readiness::Readiness::Unverifiable { reason } => {
                log(format!(
                    "ci host unverifiable — {reason} — boarding anyway (fail-open until \
                     the host observation series exists)"
                ));
            }
            host_readiness::Readiness::Proceed => {}
        }

        self.ensure_clone()?;
        let (cands, mut left_behind) = self.candidates().await?;
        if self.cfg.dry {
            log(format!("DRY: candidates: {}", py_pairs(&cands)));
            log(format!(
                "DRY: would assemble {train_branch} and, if the consist check passes, open its \
                 train Job for {window}"
            ));
            return Ok(());
        }

        if cands.is_empty() {
            // NOT NECESSARILY AN IDLE WINDOW. `NothingParked` says "an
            // idle window, not a failure", which is a lie when the dock is
            // full of cars the ordering filter held — and whether this
            // window is self-clearing or waiting on a person is precisely
            // what an operator reads the line to learn.
            log(no_departure_line(&empty_dock_refusal(&left_behind)));
            return Ok(());
        }

        let clone = &self.cfg.clone;
        sh(&[
            "git",
            "-C",
            clone,
            "checkout",
            "-B",
            &train_branch,
            "origin/main",
        ])?;
        // (car, branch, boarded head) — the head is WHAT boarded, and
        // the sweep's licence to delete the branch later depends on it
        // (car 23923b40). Read from the fetched `fork/<branch>` ref,
        // which is precisely the commit the merge below carries.
        let mut boarded: Vec<(Value, String, String)> = Vec::new();
        let mut skipped: Vec<(Value, String)> = Vec::new();
        for (j, branch) in cands {
            let head_out = sh(&["git", "-C", clone, "rev-parse", &format!("fork/{branch}")])?;
            let head = stdout_str(&head_out).trim().to_string();
            let r = sh_unchecked(&[
                "git",
                "-C",
                clone,
                "merge",
                "--no-ff",
                "-m",
                &format!("train: merge {branch}"),
                &format!("fork/{branch}"),
            ])?;
            if r.status.success() {
                boarded.push((j, branch, head));
            } else {
                let diff =
                    sh_unchecked(&["git", "-C", clone, "diff", "--name-only", "--diff-filter=U"])?;
                let conflicted: Vec<String> = stdout_str(&diff)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
                sh_unchecked(&["git", "-C", clone, "merge", "--abort"])?;

                // Before abandoning it, try re-railing.
                //
                // The commonest conflict here is not a real one. The
                // repo squash-merges, so a car cut before the last
                // train — or stacked on a car that has since landed —
                // carries commits whose CHANGES are already in main but
                // whose SHAS are not ancestors of it. Merging re-applies
                // landed hunks on top of themselves and collides.
                //
                // `git rebase` is the tool that knows the difference: it
                // drops a patch already present upstream. So replay the
                // car's own commits onto the consist as it stands and
                // merge that instead. A car with a GENUINE conflict
                // fails the rebase too and is skipped exactly as before.
                //
                // Measured cost of not doing this: four cars re-railed
                // by hand in one evening (2026-08-15), each one a fresh
                // branch name, a repointed `metadata.branch` and a wait
                // for the next window — and the same by hand on 08-12
                // and 08-14. The conductor already knows everything it
                // needs; it just gave up one step early.
                if let Some(rerailed) = rerail_onto_consist(clone, &train_branch, &branch)? {
                    let retry = sh_unchecked(&[
                        "git",
                        "-C",
                        clone,
                        "merge",
                        "--no-ff",
                        "-m",
                        &format!("train: merge {branch} (re-railed)"),
                        &rerailed,
                    ])?;
                    if retry.status.success() {
                        log(format!(
                            "{branch}: re-railed onto the consist — its base was no longer an \
                             ancestor of main"
                        ));
                        // The ORIGINAL head is still what boarded: the
                        // sweep's licence to delete the branch compares
                        // against the ref the car names, and re-railing
                        // changed the shas we merged, not the car.
                        boarded.push((j, branch, head));
                        continue;
                    }
                    sh_unchecked(&["git", "-C", clone, "merge", "--abort"])?;
                }
                // ONE reason string, journal and Job alike — the chip
                // the yard renders and the line the operator greps
                // must never tell different stories.
                let reason = skip_reason_conflict(&conflicted, self.policy.skip_reason_file_budget);
                log(format!("{branch}: {reason} — left for the next train"));
                left_behind.push(json!({
                    "car_id_short": id8(job_id(&j)?),
                    "reason": reason.as_str(),
                }));
                self.merge_job_metadata(job_id(&j)?, vec![("skip_reason", json!(reason))])
                    .await?;
                skipped.push((j, branch));
            }
        }

        let skipped_names = skipped
            .iter()
            .map(|(_, b)| b.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        if boarded.is_empty() {
            log(no_departure_line(&NoDeparture::AllConflicted {
                branches: skipped_names.clone(),
            }));
            return Ok(());
        }

        // THE CONSIST CHECK — the assembled tree answers the cheap
        // questions before the train spends anything on the expensive
        // ones. See the section comment above `consist_check` for the
        // arrival-rate numbers that bought it; the short version is
        // that a per-branch gate cannot see a failure that exists only
        // in the combination, and every failure of the last two days
        // was one of those.
        //
        // Placed BEFORE the push, not merely before the PR: a refused
        // consist should leave nothing behind on the forge to clean up
        // later (the 62 stale `train/*` branches of ab3fa473 are what
        // that debt looks like when nobody owns it).
        //
        // Freshen the trunk ref FIRST. The cheap lints resolve their
        // baseline as `merge-base(origin/main, HEAD)` in this clone,
        // and a train that landed since this board's `ensure_clone`
        // leaves that ref lagging behind the assembled tree — which
        // reads already-landed changes as this consist's own and
        // refuses it (2026-09-06). Best-effort: a failed fetch logs
        // and the lints use the ref as it stands, exactly as before.
        freshen_trunk(clone);
        let verdict = consist_check(Path::new(clone), &self.policy);
        for w in verdict.warnings() {
            log(format!(
                "consist check: {w} — skipping it, a broken check must not hold a train"
            ));
        }
        if let ConsistVerdict::Refuse { failed, ran, .. } = &verdict {
            let reason = consist_refusal_reason(failed, self.policy.skip_reason_file_budget);
            log(format!(
                "consist check: {} of {ran} checks disagree with the assembled tree",
                failed.len()
            ));
            // The output goes in the journal in full, not just the
            // name: what cost 90 minutes was learning ONE bit per
            // attempt, and the bit is in what the check SAID.
            for f in failed {
                log(format!("consist check: {} said —", f.name));
                for line in f.output.lines() {
                    log(format!("consist check:   {line}"));
                }
            }
            // NOBODY'S CAR IS AT FAULT. Each one was green on its own
            // branch; the tree only broke once they were merged
            // together. So: no train packet, no PR, no push, no CI spent
            // — and every car keeps `metadata.train` unset (never
            // boarded, so still `parked_ready`) and `red_trains`
            // untouched. Striking cars for a combination failure is the
            // bug we already know about.
            //
            // THE EVIDENCE RIDES THE CARS, not a cancelled train. It
            // used to live in `consist_check` on a pr-train Job that
            // existed only to be cancelled (4860aff8); the car whose
            // boarding it blocks is both the honest owner of the fact
            // and where an operator is already looking. `skip_reason`
            // names the check and the files; `consist_refusal` carries
            // what each check SAID, in full, because what cost 90
            // minutes on 2026-09-04 was learning one bit per attempt.
            // Both are cleared in the same write that stamps a later
            // boarding, so neither outlives the refusal.
            let refusal = json!({
                "verdict": "refused",
                "checks_run": ran,
                "failed": failed
                    .iter()
                    .map(|f| json!({
                        "lint": f.name,
                        "files": f.files,
                        "output": f.output,
                    }))
                    .collect::<Vec<_>>(),
            });
            for (j, _branch, _head) in &boarded {
                let cid = job_id(j)?;
                self.merge_job_metadata(
                    cid,
                    vec![
                        ("skip_reason", json!(reason)),
                        ("consist_refusal", refusal.clone()),
                    ],
                )
                .await?;
            }
            log(no_departure_line(&NoDeparture::ConsistRefused {
                reason: format!("consist check refused — {reason}"),
                cars: boarded.len(),
            }));
            return Ok(());
        }
        log(format!(
            "consist check: {} cheap lint(s) clean on the assembled tree",
            verdict.ran()
        ));

        sh(&["git", "-C", clone, "push", "fork", &train_branch])?;
        let train_ref_out = sh(&["git", "-C", clone, "rev-parse", "--short", "HEAD"])?;
        let train_ref = stdout_str(&train_ref_out).trim().to_string();

        // THE PACKET OPENS HERE — one call site, after the consist check
        // passed and after the branch is on the forge, so a pr-train Job
        // exists only for a train that is actually departing (4860aff8).
        // AFTER the push on purpose: a push that fails leaves one stale
        // `train/*` branch, while a packet opened for a train that never
        // pushed HOLDS THE TRACK until a human cancels it.
        let Some(train) = self.open_train_job(&train_branch, &window).await? else {
            // `None` is the dry-run answer and a dry run returned long
            // before the clone was touched. Say it, rather than running
            // on with no packet to record anything against.
            log("no train departed — the train Job was not opened; nothing further attempted");
            return Ok(());
        };
        let train_id = job_id(&train)?.to_string();

        let mut lines: Vec<String> = boarded
            .iter()
            .map(|(j, b, _)| {
                format!(
                    "- `{b}` — {} (Job `{}`)",
                    j.get("title").and_then(Value::as_str).unwrap_or_default(),
                    id8(j.get("id").and_then(Value::as_str).unwrap_or("?"))
                )
            })
            .collect();
        if !skipped.is_empty() {
            lines.push(String::new());
            lines.push(format!(
                "Left behind on merge conflicts (next train): {skipped_names}"
            ));
        }
        let body = format!(
            "The {window} train: {} change(s) batched by the conductor.\n\n{}\n\n\
             🤖 opened by `boss train` (pr-train Workflow)",
            boarded.len(),
            lines.join("\n")
        );
        let pr_url = self
            .forge
            .pr_create(
                &self.cfg.gh_repo,
                &train_branch,
                &format!("train: {window} ({} changes)", boarded.len()),
                &body,
            )
            .await?;

        let boarded_ids: Vec<String> = boarded
            .iter()
            .map(|(j, _, _)| job_id(j).map(str::to_string))
            .collect::<Result<_>>()?;
        let skipped_branches: Vec<String> = skipped.iter().map(|(_, b)| b.clone()).collect();
        // THE TRAIN'S CHANNEL — the heaviest of its cars' (data < config
        // < software < infra, the order `channels.rs` resolves a mixed
        // car on), stamped here beside `boarded_jobs` so a reader can
        // tell a config-only train from a software one without opening
        // every car, and the yard can name it ('data train · 1 car').
        // A car with no stamp reads as software, its own default
        // (cffef553, 2026-09-15).
        let train_channel = crate::channels::train_channel(boarded.iter().map(|(j, _, _)| j));
        self.merge_job_metadata(
            &train_id,
            vec![
                ("boarded_jobs", json!(boarded_ids)),
                ("delivery_channel", json!(train_channel)),
                ("skipped_branches", json!(skipped_branches)),
                // The train's own record of who it left behind and
                // why — the arrival report reads THIS, because a
                // car's skip_reason clears the moment a later train
                // boards it.
                ("left_behind", json!(left_behind)),
            ],
        )
        .await?;
        let train = self.get_job(&train_id).await?;
        let boarded_note = boarded
            .iter()
            .map(|(j, b, _)| {
                format!(
                    "{b} ({})",
                    id8(j.get("id").and_then(Value::as_str).unwrap_or("?"))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        // THE SHA EACH CAR CONTRIBUTED, recorded because twice on
        // 2026-08-17 a consist carried a commit nobody intended and
        // nothing said so: `feat/dev-shared-target` was 3370b42
        // locally and 96109f7 on the forge, and the train assembled
        // the stale one silently. The head is already resolved to
        // board the car, so writing it down costs nothing and turns
        // "which commit did this train actually carry" from a hand
        // diff into a field.
        let heads_note = boarded
            .iter()
            .map(|(j, b, _)| {
                // `fork/<branch>` on purpose, not the local ref: this
                // records what the train ASSEMBLED FROM, which is the
                // thing a reader needs when a consist misbehaves.
                let sha = sh_unchecked(&[
                    "git",
                    "-C",
                    &self.cfg.clone,
                    "rev-parse",
                    "--short",
                    "--verify",
                    "--quiet",
                    &format!("fork/{b}"),
                ])
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string());
                format!(
                    "{} {b}@{sha}",
                    id8(j.get("id").and_then(Value::as_str).unwrap_or("?"))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.complete_step(
            &train,
            find_step(&train, "collect", "Collect what is ready to board"),
            &[("boarded", Some(boarded_note))],
        )
        .await?;
        self.complete_step(
            &train,
            find_step(&train, "assemble", "Assemble the train branch"),
            &[
                ("train_ref", Some(format!("{train_branch}@{train_ref}"))),
                ("car_heads", (!heads_note.is_empty()).then_some(heads_note)),
                (
                    "skipped",
                    Some(if skipped_names.is_empty() {
                        "none".to_string()
                    } else {
                        skipped_names.clone()
                    }),
                ),
            ],
        )
        .await?;
        self.complete_step(
            &train,
            find_step(&train, "pr", "Open the batched PR"),
            &[("pr_url", Some(pr_url.clone()))],
        )
        .await?;

        for (j, _branch, head) in &boarded {
            // BOARDING DOES NOT COMPLETE `review` — the merge does.
            //
            // It used to complete it here, and that quietly made
            // cancelling a loaded train impossible. A released car has
            // to become `parked_ready` again, which requires its review
            // step to be ready or active; but a completed step is FROZEN
            // at the row (`update_step_at` pins status, completed_on and
            // metadata on terminal rows, deliberately, so a racing
            // read-modify-write cannot demote it). So the cancel path's
            // reopen was a no-op that returned 204, and every "released
            // car back to the dock" line it logged was false — the car
            // had `train` cleared but stayed unboardable forever. The
            // only reason nobody hit it is that every cancel until now
            // carried zero cars.
            //
            // Boarded-ness does not need the step at all: it is
            // `metadata.train`, which is what `parked_ready` already
            // reads, and which a cancel can clear because metadata is
            // not frozen. So the step keeps meaning what it says —
            // this change is open for review until it lands — and
            // release becomes a metadata write with nothing to reverse.
            // (Requires no workflow edit: the spec still gates the
            // `merged` outcome on `steps.review.done`, and the merge
            // block below is what satisfies it.)
            //
            // skip_reason cleared on boarding, in the same update that
            // stamps the train: an earlier window's skip note must not
            // outlive the skip — the key is REMOVED (Null), not left
            // behind as "". `consist_refusal` — the lint output a
            // refused consist leaves on the car it blocked — comes off
            // in the same write, for the same reason.
            //
            // `boarded_head` rides here too, and lives on the CAR
            // rather than in a second list on the train: the sweep
            // already fetches every boarded car, so the fact stays in
            // one place (guideline 9a) and costs no extra call. It is
            // rewritten on every boarding, so a car that rides a later
            // train carries that train's head, not the first one's.
            self.merge_job_metadata(
                job_id(j)?,
                vec![
                    ("train", json!(train_id.as_str())),
                    ("boarded_head", json!(head.as_str())),
                    ("skip_reason", Value::Null),
                    ("consist_refusal", Value::Null),
                ],
            )
            .await?;
        }
        log(format!(
            "train {} boarded {}, PR {pr_url}",
            id8(&train_id),
            boarded.len()
        ));

        // THE TRAIN GATE, FILED IN THIS PASS (backlog 95c349a5). Until
        // 2026-09-14 the gate was filed only from the ci-step block in
        // `reconcile`, so every train waited for the next tick before its
        // Rust checks started — 8m30s, 4m29s, 6m30s, 2m30s on the last
        // four, on the critical path of every landing. `train_gate` is
        // the same launch path the ci block uses and is idempotent on
        // KEY_RUN, so that block stays the retry for a launch that fails
        // here (the gate bound, kubectl, the API). BEST-EFFORT, after
        // the cars are stamped: nothing in boarding may abort on it — a
        // train that boarded is a train, gate or no gate. The instant is
        // `Utc::now()`, not the pass's `now`: the boarding pass can run
        // minutes, and `train_gate_launched_at` before the pr step's own
        // `completed_at` would be a record nobody could read.
        match self.get_job(&train_id).await {
            Ok(mut fresh) if gate_due_at_boarding(&fresh) => {
                self.train_gate(&mut fresh, &train_id, Utc::now()).await;
            }
            Ok(_) => log(format!(
                "train {}: not filing its gate at boarding — it already carries one, or its record is incomplete; the reconcile pass files it",
                id8(&train_id)
            )),
            Err(e) => log(format!(
                "train {}: could not re-read the train to file its gate at boarding ({e}) — the reconcile pass files it",
                id8(&train_id)
            )),
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Cancel — the operator's judgment on a train that will not arrive
    // -----------------------------------------------------------------------

    /// Cancel an open train (David's ask: trains that don't arrive
    /// were cleaned up by hand, and cancellation orphaned the cars).
    /// Car-release comes FIRST, so a crash mid-cancel leaves cars
    /// free rather than orphaned:
    ///   1. release every still-open boarded car back to the dock —
    ///      review step back to `ready` (the dock predicate requires
    ///      it; clearing metadata alone re-boards nothing),
    ///      `metadata.train` removed, `skip_reason` saying why;
    ///   2. close the PR unmerged;
    ///   3. complete the `cancelled` terminal with the reason —
    ///      jobs-api then closes the Job with outcome=cancelled and
    ///      skips the remaining steps;
    ///   4. delete the train's OWN `train/*` branch — never a car's:
    ///      the cars keep their branches (train_branch_to_delete is
    ///      the pin, and it is tested).
    /// The operator's verb. Never counts a red against the cars — an
    /// operator cancels for reasons of their own (a bad consist, a
    /// withdrawn change), and only the automatic red-stall path below
    /// has evidence that the CARS were implicated.
    pub(super) async fn cancel(&self, handle: &str, reason: &str) -> Result<()> {
        self.cancel_train(handle, reason, false).await
    }

    /// Honour an operator's `cancel_requested` stamp — the yard's cancel
    /// button, read by `reconcile`. Returns whether the request has
    /// claimed this train's pass: when it has, the caller skips the rest
    /// of the pass for this train, because a train under a cancel
    /// request must not go on to merge — whether the cancel succeeded,
    /// is dry, or is being retried.
    ///
    /// NON-FATAL BY CONSTRUCTION: this returns `bool`, not `Result`, so
    /// the reconcile loop cannot `?` it. A cancel is forge writes first
    /// (`cancel_train` closes the PR before releasing a car) and the
    /// forge is the flaky half; a refusal there LOGS and the train stays
    /// intact — cars aboard, stamp in place — so the next pass retries.
    /// A fatal write in this loop once froze all landings for ~8h
    /// (boss-conductor-loop-writes-must-not-be-fatal).
    async fn honour_cancel_request(
        &self,
        train: &Value,
        tid: &str,
        pr_state: Option<&str>,
    ) -> bool {
        if let Some(refusal) = operator_cancel_refusal(train) {
            log(format!(
                "train {}: cancel requested but {refusal} — refusing, cars stay landed",
                id8(tid)
            ));
            if let Err(e) = self
                .merge_job_metadata(tid, vec![("cancel_refused", json!(refusal))])
                .await
            {
                log(format!(
                    "train {}: cancel_refused stamp failed (non-fatal, retries next pass): {e}",
                    id8(tid)
                ));
            }
            return false;
        }
        let Some(reason) = operator_cancel_reason(train) else {
            return false;
        };
        if pr_state != Some("OPEN") {
            // Neither merged nor open — closed on the forge by hand, or
            // mid-merge. The same gate the automatic rule keeps: the
            // operator verb (`boss train cancel`) takes it from here.
            log(format!(
                "train {}: cancel requested but its PR is {} — leaving it to `boss train cancel`",
                id8(tid),
                pr_state.unwrap_or("unknown")
            ));
            return false;
        }
        log(format!("train {} cancelling: {reason}", id8(tid)));
        if self.cfg.dry {
            log(format!("DRY: would cancel {} ({reason})", id8(tid)));
        } else if let Err(e) = self.cancel_train(tid, &reason, false).await {
            log(format!(
                "train {}: cancel failed (non-fatal, train intact, retries next pass): {e}",
                id8(tid)
            ));
        }
        true
    }

    async fn cancel_train(&self, handle: &str, reason: &str, count_red: bool) -> Result<()> {
        let listed = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=pr-train&status=open&limit=50",
                None,
            )
            .await?,
        )?;
        let mut trains = Vec::with_capacity(listed.len());
        for t0 in &listed {
            trains.push(self.get_job(job_id(t0)?).await?);
        }
        let train = resolve_train(&trains, handle)?;
        let tid = job_id(train)?;
        // Refuse a train that has no terminal to complete BEFORE any
        // write, and say what was found. On 2026-09-16 pr-train
        // 06e5610f was admitted with nine of its ten steps — a slow
        // database, a client timeout, the `cancelled` terminal never
        // written (backlog f2ba226e; admission is one transaction
        // since) — and this verb refused "step missing on job" only
        // AFTER closing the PR and releasing the cars. The registry's
        // re-evaluation logs the same divergence as "pairing by slug
        // ... unpaired" (registry.rs); this is the operator-facing
        // half of that warning, at the one verb that needed the row.
        if find_step(train, "cancelled", "Cancelled — nothing to board").is_none() {
            let version = train
                .get("workflow_version")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let present: Vec<String> = train
                .get("steps")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(step_label).collect())
                .unwrap_or_default();
            bail!(
                "refusing to cancel train {}: its steps diverged from its workflow spec — \
                 the `cancelled` terminal has no row on this job (pinned pr-train v{version}, \
                 {} step row(s): {}), so it can never be completed. This is the residue of a \
                 partial admission (backlog f2ba226e). Repair door: re-materialise the \
                 unpaired steps of the pinned version onto the packet (a follow-up verb; until \
                 it exists the only door is a status close through the jobs API). Nothing was \
                 written: the PR is still open and the cars are still aboard.",
                id8(tid),
                present.len(),
                present.join(", ")
            );
        }

        let boarded: Vec<String> = train
            .get("metadata")
            .and_then(|m| m.get("boarded_jobs"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let mut cars = Vec::with_capacity(boarded.len());
        for cid in &boarded {
            cars.push(self.get_job(cid).await?);
        }
        // Say what is NOT being released, and why. A car that moved on
        // is the interesting case: silently skipping it would leave the
        // operator with a cancel that released fewer cars than the train
        // claims to carry, and no way to tell whether that was correct.
        for car in &cars {
            if car.get("status").and_then(Value::as_str) != Some("open") {
                continue;
            }
            let owner = car
                .get("metadata")
                .and_then(|m| m.get("train"))
                .and_then(Value::as_str);
            match owner {
                Some(t) if t == tid => {}
                Some(other) => log(format!(
                    "car {} now rides {} — not releasing it",
                    id8(job_id(car)?),
                    id8(other)
                )),
                None => log(format!(
                    "car {} was already released — leaving its record alone",
                    id8(job_id(car)?)
                )),
            }
        }

        let pr_url = find_step(train, "pr", "Open the batched PR")
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("pr_url"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        // Cancel the CI BEFORE closing the PR. Closing first leaves a
        // window where the run is still burning the single-concurrency
        // runner for a PR that is already gone, which is the state
        // 89b27e60 measured 27 minutes into.
        let train_head = train_ref_of(train)
            .and_then(|r| r.rsplit('@').next())
            .unwrap_or_default()
            .to_string();
        if !pr_url.is_empty() || !train_head.is_empty() {
            // Last path segment of the PR url is its number on both
            // forges; kept inline rather than reaching for a
            // Forgejo-specific helper from forge-blind code.
            let idx = pr_url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            if self.cfg.dry {
                log(format!(
                    "DRY: would cancel CI runs for PR #{idx} / {train_head}"
                ));
            } else {
                match self.forge.cancel_ci_runs(&idx, &train_head).await {
                    Ok(0) => log("cancel: no CI runs were still active".to_string()),
                    Ok(n) => log(format!("cancel: cancelled {n} in-flight CI run(s)")),
                    Err(e) => log(format!("cancel: CI cancellation failed, continuing: {e}")),
                }
            }
        }

        if !pr_url.is_empty() {
            if self.cfg.dry {
                log(format!("DRY: would close {pr_url} unmerged"));
            } else {
                self.forge.close_pr(pr_url).await?;
                log(format!("closed {pr_url} unmerged"));
            }
        }

        // RELEASE THE CARS ONLY AFTER THE FORGE WRITES SUCCEED. Cancel
        // does two kinds of write: releasing a car is a jobs-API metadata
        // write (reliable, local), closing the PR is a forge write (the
        // flaky one — an unreachable forge, a read-only token). Releasing
        // FIRST left "half-cancelled" trains: the cars back on the dock
        // but the PR still open, because close_pr's `?` returned Err with
        // the release already done (10bb1e1a; the comment at the top of
        // this file's cancel path names the two it stranded). Doing the
        // flaky writes first means a forge failure aborts here with the
        // train fully intact — cars still aboard, PR still open — so a
        // re-run is clean, and the car release only happens once the PR is
        // actually closed.
        for car in releasable_cars(&cars, tid) {
            let cid = job_id(car)?;
            // NOTHING TO REOPEN. Releasing a car is a metadata write and
            // only a metadata write, because boarding no longer completes
            // its `review` step — see the boarding loop. This used to PUT
            // the step back to `ready`, which the row silently refused
            // (terminal steps are frozen in `update_step_at`) and which
            // now 409s out loud, taking the whole cancel with it. A car
            // that predates this change still carries a completed review
            // and cannot be released; those were translated into fresh
            // packets by hand on 2026-08-15 rather than reversed.
            self.merge_job_metadata(cid, release_stamps(car, reason, count_red))
                .await?;
            log(format!("released car {} back to the dock", id8(cid)));
        }

        // The cancelled terminal is gated (blocked_by) on collect; a
        // train that died mid-assembly never completed it. Close that
        // gate honestly first — nothing boarded on the record.
        let collect = find_step(train, "collect", "Collect what is ready to board");
        if !step_done(collect) {
            self.complete_step(
                train,
                collect,
                &[(
                    "boarded",
                    Some("nothing — train cancelled before boarding completed".to_string()),
                )],
            )
            .await?;
        }
        self.complete_step(
            train,
            find_step(train, "cancelled", "Cancelled — nothing to board"),
            &[("reason", Some(reason.to_string()))],
        )
        .await?;

        if let Some(branch) = train_branch_to_delete(train) {
            if self.cfg.dry {
                log(format!(
                    "DRY: would delete branch {branch} (train cancelled)"
                ));
            } else if self.forge.delete_branch(&branch).await? {
                log(format!("deleted branch {branch} (train cancelled)"));
            } else {
                log(format!("branch {branch} already gone (train cancelled)"));
            }
        }
        log(format!("train {} cancelled: {reason}", id8(tid)));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::train::test_support::*;

    // -- the arrival branch cleanup ----------------------------------------
    //
    // Cancel has deleted its train's branch since the verb existed;
    // nothing owned the branch after a HAPPY landing, and 62 stale
    // train/* branches accumulated on the forge between 08-13 and
    // 08-20 — squash merges mean ancestry can never classify them
    // after the fact (ab3fa473). The arrival record is the proof, and
    // the cleanup reads it at exactly the right moment.

    /// The forge as a call recorder: `delete_branch` notes the branch
    /// it was asked for and answers as told; every other verb is
    /// unreachable in these tests. The seam the Forge trait exists
    /// for, pointed at the cleanup.
    struct FakeForge {
        deleted: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        fail_deletes: bool,
    }

    #[async_trait]
    impl Forge for FakeForge {
        async fn pr_info(&self, _url: &str) -> Result<Value> {
            bail!("not exercised")
        }
        async fn pr_create(
            &self,
            _repo: &str,
            _head_branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<String> {
            bail!("not exercised")
        }
        async fn merge(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn close_pr(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn delete_branch(&self, branch: &str) -> Result<bool> {
            self.deleted.lock().unwrap().push(branch.to_string());
            if self.fail_deletes {
                bail!("HTTP 500: forge down");
            }
            Ok(true)
        }
        async fn branch_head(&self, _branch: &str) -> Result<Option<String>> {
            bail!("not exercised")
        }
        async fn cancel_ci_runs(&self, _pr_index: &str, _head_sha: &str) -> Result<usize> {
            bail!("not exercised")
        }
    }

    /// The tree root the config fixtures name.
    ///
    /// `scratch_path` rather than a fixed `/tmp/boss-train-test`, and it
    /// creates nothing — no test here touches the filesystem. The name
    /// still carries the uid and the pid, because a fixed name under the
    /// 1777 shared temp root is the shape that has bitten this repo
    /// fourteen times, and the next test that DOES touch this path would
    /// inherit the collision silently.
    fn train_test_home() -> std::path::PathBuf {
        boss_testing::scratch::scratch_path("boss-train-test")
    }

    /// A conductor whose config is fixtures and whose forge is the
    /// recorder — the cleanup touches neither the jobs API nor the
    /// tree, so nothing else needs to exist.
    fn cleanup_conductor(forge_kind: &str, forge: Box<dyn Forge>) -> Conductor {
        let home = train_test_home();
        Conductor {
            cfg: Config {
                jobs: "http://jobs.invalid".into(),
                gh_repo: "example/boss".into(),
                head_owner: "example".into(),
                fork_url: "https://github.com/example/boss-fork.git".into(),
                upstream_url: "https://github.com/example/boss.git".into(),
                home: home.display().to_string(),
                clone: home.join("repo").display().to_string(),
                forge_kind: forge_kind.into(),
                auto_merge: false,
                allow_local_jobs: true,
                ci_hours: 2,
                converge_alarm_mins: 30,
                stranded_alarm_mins: 45,
                auto_park_grace_mins: 10,
                auto_cancel: false,
                ci_host: None,
                gate_manifest: "/nonexistent/gate-runner.yaml".to_string(),
                gate_namespace: "boss-dev".to_string(),
                gate_required: false,
                dry: false,
            },
            http: reqwest::Client::new(),
            forge,
            owner: crate::owner::resolver("http://jobs.invalid"),
            policy: policy(),
        }
    }

    #[tokio::test]
    async fn a_happy_arrival_requests_deletion_of_the_trains_own_branch() {
        let deleted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let forge = Box::new(FakeForge {
            deleted: std::sync::Arc::clone(&deleted),
            fail_deletes: false,
        });
        let c = cleanup_conductor("forgejo", forge);
        c.clean_arrived_train_branch(&arrived_train_with_branch())
            .await;
        assert_eq!(
            *deleted.lock().unwrap(),
            vec!["train/20260820-0600".to_string()]
        );
    }

    #[tokio::test]
    async fn a_failed_delete_does_not_fail_the_arrival() {
        let deleted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let forge = Box::new(FakeForge {
            deleted: std::sync::Arc::clone(&deleted),
            fail_deletes: true,
        });
        let c = cleanup_conductor("forgejo", forge);
        // Returns () — there is no Result to fail: the forge blowing
        // up costs a journal line and nothing else. A leftover branch
        // is debt; a failed arrival is an outage.
        c.clean_arrived_train_branch(&arrived_train_with_branch())
            .await;
        // And the delete WAS attempted — the line narrates a real event.
        assert_eq!(
            *deleted.lock().unwrap(),
            vec!["train/20260820-0600".to_string()]
        );
    }

    #[tokio::test]
    async fn only_a_forgejo_happy_arrival_cleans_its_branch() {
        let deleted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        // Under the github adapter the repo auto-deletes merged
        // train/* PR heads — nothing to own, nothing requested.
        let c = cleanup_conductor(
            "github",
            Box::new(FakeForge {
                deleted: std::sync::Arc::clone(&deleted),
                fail_deletes: false,
            }),
        );
        c.clean_arrived_train_branch(&arrived_train_with_branch())
            .await;
        // A cancelled train closes with `arrived` SKIPPED — its
        // branch was the cancel verb's, deleted at cancel time, and
        // the arrival cleanup asks for nothing.
        let mut cancelled = arrived_train_with_branch();
        cancelled["steps"][3]["status"] = json!("skipped");
        let c2 = cleanup_conductor(
            "forgejo",
            Box::new(FakeForge {
                deleted: std::sync::Arc::clone(&deleted),
                fail_deletes: false,
            }),
        );
        c2.clean_arrived_train_branch(&cancelled).await;
        assert!(deleted.lock().unwrap().is_empty());
        // Cancel's own pin is untouched by the arrival filter: the
        // cancelled train's branch is still exactly the one the
        // cancel path deletes.
        assert_eq!(
            train_branch_to_delete(&cancelled),
            Some("train/20260820-0600".to_string())
        );
    }

    // -- cancel releases cars only after the forge write succeeds -----
    struct CancelForge {
        close_called: std::sync::Arc<std::sync::Mutex<bool>>,
    }
    #[async_trait::async_trait]
    impl Forge for CancelForge {
        async fn pr_info(&self, _url: &str) -> Result<Value> {
            bail!("not exercised")
        }
        async fn pr_create(
            &self,
            _repo: &str,
            _head_branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<String> {
            bail!("not exercised")
        }
        async fn merge(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn close_pr(&self, _url: &str) -> Result<()> {
            *self.close_called.lock().unwrap() = true;
            bail!("HTTP 403: forge write refused")
        }
        async fn delete_branch(&self, _branch: &str) -> Result<bool> {
            bail!("not exercised")
        }
        async fn branch_head(&self, _branch: &str) -> Result<Option<String>> {
            bail!("not exercised")
        }
        async fn cancel_ci_runs(&self, _pr_index: &str, _head_sha: &str) -> Result<usize> {
            Ok(0)
        }
    }

    /// 10bb1e1a: releasing a car is a jobs-API metadata write; closing
    /// the PR is the flaky forge write. Releasing FIRST left
    /// "half-cancelled" trains — cars back on the dock, PR still open —
    /// when close_pr's `?` returned Err with the release already done.
    /// This drives cancel_train against a real in-process jobs server
    /// with a forge whose close_pr FAILS, and asserts NO car was
    /// released: the release now happens only after the PR is closed.
    #[tokio::test]
    async fn cancel_does_not_release_cars_when_close_pr_fails() {
        use axum::extract::Path;
        use axum::routing::get;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        let train = json!({
            "id": "t1", "kind": "pr-train", "status": "open",
            "metadata": { "boarded_jobs": ["c1"], "train_ref": "train/x@abcdef1" },
            "steps": [
                {"id":"s-pr","spec_slug":"pr","title":"Open the batched PR","status":"completed","metadata":{"pr_url":"https://forge.example/david/boss/pulls/9"}},
                {"id":"s-collect","spec_slug":"collect","title":"Collect what is ready to board","status":"completed","metadata":{}},
                {"id":"s-cancelled","spec_slug":"cancelled","title":"Cancelled — nothing to board","status":"ready","metadata":{}}
            ]
        });
        let car = json!({
            "id": "c1", "kind": "ship-a-change", "status": "open",
            "metadata": { "train": "t1", "branch": "fix/x" },
            "steps": [{"id":"c-rev","spec_slug":"review","title":"Open for review","status":"ready","metadata":{}}]
        });

        // Every PUT the conductor makes; a release is a PUT /api/jobs/{car}.
        let puts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let train_list = train.clone();
        let train_one = train.clone();
        let car_one = car.clone();
        let puts_route = puts.clone();
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move || {
                    let train = train_list.clone();
                    async move { Json(json!({ "data": [train] })) }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let (t, c) = (train_one.clone(), car_one.clone());
                    async move { Json(if id == "t1" { t } else { c }) }
                })
                .put(move |Path(id): Path<String>, _b: Json<Value>| {
                    let puts = puts_route.clone();
                    async move {
                        puts.lock().unwrap().push(id);
                        Json(json!({}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let close_called = Arc::new(Mutex::new(false));
        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(CancelForge {
                close_called: close_called.clone(),
            }),
        );
        c.cfg.jobs = format!("http://{addr}");

        let res = c.cancel_train("t1", "forge unreachable", false).await;
        assert!(res.is_err(), "cancel must surface the close_pr failure");
        assert!(
            *close_called.lock().unwrap(),
            "close_pr must have been attempted"
        );
        assert!(
            !puts.lock().unwrap().contains(&"c1".to_string()),
            "the car was released despite close_pr failing — a half-cancelled train"
        );
    }

    /// f2ba226e: pr-train 06e5610f was admitted with nine of its ten
    /// steps — the `cancelled` terminal never written — and `boss train
    /// cancel` refused with "step missing on job", AFTER it had already
    /// closed the PR and released the cars. The refusal now comes
    /// FIRST, before any forge or jobs-API write, and names what it
    /// found: the graph diverged from the pinned version, which step
    /// is unpaired, and the door that repairs it.
    #[tokio::test]
    async fn cancel_refuses_a_train_whose_terminal_is_missing_before_any_write() {
        // The 06e5610f shape: `cancelled` has no row on this job.
        let mut train = requested_open_train();
        train["workflow_version"] = json!(10);
        train["steps"]
            .as_array_mut()
            .unwrap()
            .retain(|s| s["spec_slug"] != "cancelled");
        let (jobs, job_puts, step_puts) =
            cancel_request_jobs_api(train, struck_boarded_car()).await;
        let (c, close_called) = cancel_request_conductor(jobs, true);

        let err = c
            .cancel_train("t1", "bad consist", false)
            .await
            .expect_err("a train with no terminal cannot be cancelled");
        let msg = err.to_string();
        assert!(
            msg.contains("diverged from its workflow spec"),
            "names the divergence: {msg}"
        );
        assert!(
            msg.contains("cancelled") && msg.contains("v10"),
            "names the unpaired step and the pinned version: {msg}"
        );
        assert!(
            msg.contains("re-materialise") && msg.contains("f2ba226e"),
            "names the repair door and the item: {msg}"
        );
        assert!(
            !msg.contains("step missing on job"),
            "the old, uninformative refusal: {msg}"
        );

        assert!(
            !*close_called.lock().unwrap(),
            "refused BEFORE the forge write — the PR is still open"
        );
        assert!(job_puts.lock().unwrap().is_empty(), "no car was released");
        assert!(
            step_puts.lock().unwrap().is_empty(),
            "no step was completed"
        );
    }

    // -- the yard's cancel button, honoured non-fatally -------------------

    /// The forge the cancel button meets: `close_pr` answers as told and
    /// records the call; CI cancellation and branch deletion are the
    /// no-ops a cancel tolerates.
    struct OperatorCancelForge {
        close_ok: bool,
        close_called: std::sync::Arc<std::sync::Mutex<bool>>,
    }
    #[async_trait::async_trait]
    impl Forge for OperatorCancelForge {
        async fn pr_info(&self, _url: &str) -> Result<Value> {
            bail!("not exercised")
        }
        async fn pr_create(
            &self,
            _repo: &str,
            _head_branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<String> {
            bail!("not exercised")
        }
        async fn merge(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn close_pr(&self, _url: &str) -> Result<()> {
            *self.close_called.lock().unwrap() = true;
            if self.close_ok {
                Ok(())
            } else {
                bail!("HTTP 502: forge unreachable")
            }
        }
        async fn delete_branch(&self, _branch: &str) -> Result<bool> {
            Ok(true)
        }
        async fn branch_head(&self, _branch: &str) -> Result<Option<String>> {
            bail!("not exercised")
        }
        async fn cancel_ci_runs(&self, _pr_index: &str, _head_sha: &str) -> Result<usize> {
            Ok(0)
        }
    }

    /// An open train carrying the operator's stamp and one boarded car
    /// that already took a strike on an earlier consist.
    fn requested_open_train() -> Value {
        json!({
            "id": "t1", "kind": "pr-train", "status": "open",
            "metadata": {
                "boarded_jobs": ["c1"], "train_ref": "train/x@abcdef1",
                "cancel_requested": {"by": "emp-david", "reason": "bad consist", "at": "2026-09-07T01:00:00Z"}
            },
            "steps": [
                {"id":"s-pr","spec_slug":"pr","title":"Open the batched PR","status":"completed","metadata":{"pr_url":"https://forge.example/david/boss/pulls/9"}},
                {"id":"s-collect","spec_slug":"collect","title":"Collect what is ready to board","status":"completed","metadata":{}},
                {"id":"s-cancelled","spec_slug":"cancelled","title":"Cancelled — nothing to board","status":"ready","metadata":{}},
                {"id":"s-merged","spec_slug":"merged","title":"Merged into main","status":"ready","metadata":{}}
            ]
        })
    }
    fn struck_boarded_car() -> Value {
        json!({
            "id": "c1", "kind": "ship-a-change", "status": "open",
            "metadata": { "train": "t1", "branch": "fix/x", "red_trains": 1 },
            "steps": [{"id":"c-rev","spec_slug":"review","title":"Open for review","status":"ready","metadata":{}}]
        })
    }

    type JobPuts = std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>;
    type StepPuts = std::sync::Arc<std::sync::Mutex<Vec<(String, String, Value)>>>;

    /// An in-process jobs API holding one train and one car, recording
    /// every write. Serves the open pr-train list and both fetches;
    /// every other list answers empty.
    async fn cancel_request_jobs_api(train: Value, car: Value) -> (String, JobPuts, StepPuts) {
        use axum::extract::{Path, RawQuery};
        use axum::routing::{get, put};
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        let job_puts: JobPuts = Arc::new(Mutex::new(Vec::new()));
        let step_puts: StepPuts = Arc::new(Mutex::new(Vec::new()));
        let (train_list, train_one) = (train.clone(), train);
        let (jp, sp) = (job_puts.clone(), step_puts.clone());
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |RawQuery(q): RawQuery| {
                    let train = train_list.clone();
                    async move {
                        let open_trains =
                            q.unwrap_or_default().contains("kind=pr-train&status=open");
                        let data: Vec<Value> = if open_trains { vec![train] } else { vec![] };
                        Json(json!({ "data": data }))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let (t, c) = (train_one.clone(), car.clone());
                    async move { Json(if id == "t1" { t } else { c }) }
                })
                .put(move |Path(id): Path<String>, Json(body): Json<Value>| {
                    let jp = jp.clone();
                    async move {
                        jp.lock().unwrap().push((id, body));
                        Json(json!({}))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                put(
                    move |Path((id, sid)): Path<(String, String)>, Json(body): Json<Value>| {
                        let sp = sp.clone();
                        async move {
                            sp.lock().unwrap().push((id, sid, body));
                            Json(json!({}))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), job_puts, step_puts)
    }

    fn cancel_request_conductor(
        jobs: String,
        close_ok: bool,
    ) -> (Conductor, std::sync::Arc<std::sync::Mutex<bool>>) {
        let close_called = std::sync::Arc::new(std::sync::Mutex::new(false));
        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(OperatorCancelForge {
                close_ok,
                close_called: close_called.clone(),
            }),
        );
        c.cfg.jobs = jobs;
        (c, close_called)
    }

    /// The button, honoured: an open train stamped `cancel_requested` is
    /// cancelled the way the operator verb cancels — the car released
    /// UNSTRUCK (`red_trains` untouched) with the operator's reason as
    /// its `skip_reason`, the `cancelled` terminal completed with that
    /// reason — and the request claims the train's pass.
    #[tokio::test]
    async fn a_cancel_request_on_an_open_train_releases_its_cars_unstruck() {
        let (jobs, job_puts, step_puts) =
            cancel_request_jobs_api(requested_open_train(), struck_boarded_car()).await;
        let (c, close_called) = cancel_request_conductor(jobs, true);

        assert!(
            c.honour_cancel_request(&requested_open_train(), "t1", Some("OPEN"))
                .await
        );
        assert!(*close_called.lock().unwrap(), "the PR is closed unmerged");

        let puts = job_puts.lock().unwrap();
        let (_, car) = puts
            .iter()
            .find(|(id, _)| id == "c1")
            .expect("the car was released");
        let md = &car["metadata"];
        assert!(
            md.get("train").is_none(),
            "released: the train stamp is gone"
        );
        assert_eq!(
            md["skip_reason"].as_str().unwrap_or_default(),
            "returned to dock: train cancelled (operator cancel: bad consist (by emp-david))"
        );
        assert_eq!(
            md["red_trains"],
            json!(1),
            "an operator's cancel strikes no car"
        );

        let steps = step_puts.lock().unwrap();
        let (_, _, cancelled) = steps
            .iter()
            .find(|(id, sid, _)| id == "t1" && sid == "s-cancelled")
            .expect("the cancelled terminal was completed");
        assert_eq!(
            cancelled["metadata"]["reason"],
            json!("operator cancel: bad consist (by emp-david)")
        );
    }

    /// The forge refuses the close: the train stays intact — no car
    /// released, no terminal completed — the request still claims the
    /// pass (a train under a cancel request must not go on to merge),
    /// and the method RETURNS, because it cannot fail: the reconcile
    /// loop has nothing to `?` and the other trains continue.
    #[tokio::test]
    async fn a_forge_failure_leaves_the_train_intact_and_the_pass_alive() {
        let (jobs, job_puts, step_puts) =
            cancel_request_jobs_api(requested_open_train(), struck_boarded_car()).await;
        let (c, close_called) = cancel_request_conductor(jobs, false);

        assert!(
            c.honour_cancel_request(&requested_open_train(), "t1", Some("OPEN"))
                .await
        );
        assert!(*close_called.lock().unwrap(), "close_pr was attempted");
        assert!(
            job_puts.lock().unwrap().is_empty(),
            "no car released, nothing stamped — a retry next pass is clean"
        );
        assert!(
            step_puts.lock().unwrap().is_empty(),
            "no terminal completed"
        );
    }

    /// A request that arrives after the merge is refused on the record,
    /// once, and does not claim the pass — the landed train goes on to
    /// deploy and converge.
    #[tokio::test]
    async fn a_cancel_request_on_a_merged_train_is_stamped_refused() {
        let mut train = requested_open_train();
        train["steps"][3]["status"] = json!("completed");
        train["steps"][3]["metadata"]["merge_ref"] = json!("abc1234def56");
        let (jobs, job_puts, step_puts) =
            cancel_request_jobs_api(train.clone(), struck_boarded_car()).await;
        let (c, close_called) = cancel_request_conductor(jobs, true);

        assert!(!c.honour_cancel_request(&train, "t1", Some("MERGED")).await);
        assert!(!*close_called.lock().unwrap(), "nothing closed");
        assert!(
            step_puts.lock().unwrap().is_empty(),
            "no terminal completed"
        );
        let puts = job_puts.lock().unwrap();
        assert_eq!(puts.len(), 1, "one stamp on the train, nothing on the car");
        let (id, body) = &puts[0];
        assert_eq!(id, "t1");
        assert_eq!(
            body["metadata"]["cancel_refused"],
            json!("already merged at abc1234def56")
        );
    }

    /// A three-car train where car 2's close write fails. Cars 1 and 3
    /// must STILL get their review closed and `metadata.merged=true` (the
    /// marker the dispatcher watches to close the car Job); only the one
    /// bad car counts as a failure, and the pass does not abort. This is
    /// the orphan bug: the pre-fix loop used `?` and completed `merged`
    /// first, so one bad car left the rest as open residue forever —
    /// inflating the open-car count and starving boarding.
    #[tokio::test]
    async fn a_bad_car_does_not_orphan_the_rest_of_the_train() {
        use axum::extract::Path;
        use axum::routing::{get, put};
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        fn car(id: &str) -> Value {
            json!({
                "id": id, "kind": "ship-a-change", "status": "open",
                "metadata": { "train": "t1", "branch": format!("fix/{id}") },
                "steps": [{"id": format!("{id}-rev"), "spec_slug": "review",
                           "title": "Open for review", "status": "ready", "metadata": {}}]
            })
        }
        let cars = json!({ "c1": car("c1"), "c2": car("c2"), "c3": car("c3") });

        // (car id, endpoint) of every WRITE the conductor made.
        let writes: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

        let cars_get = cars.clone();
        let writes_step = writes.clone();
        let writes_meta = writes.clone();
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let cars = cars_get.clone();
                    async move { Json(cars.get(&id).cloned().unwrap_or(Value::Null)) }
                })
                .put(move |Path(id): Path<String>, _b: Json<Value>| {
                    let writes = writes_meta.clone();
                    async move {
                        // Car 2's metadata write is the one the SoR refuses
                        // (422 — an answer, not a blip, so it is not retried).
                        if id == "c2" {
                            return (
                                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                                Json(json!({"error": "no"})),
                            );
                        }
                        writes.lock().unwrap().push((id, "meta".into()));
                        (axum::http::StatusCode::OK, Json(json!({})))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                put(
                    move |Path((id, _sid)): Path<(String, String)>, _b: Json<Value>| {
                        let writes = writes_step.clone();
                        async move {
                            if id == "c2" {
                                return (
                                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                                    Json(json!({"error": "no"})),
                                );
                            }
                            writes.lock().unwrap().push((id, "review".into()));
                            (axum::http::StatusCode::OK, Json(json!({})))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(FakeForge {
                deleted: Arc::new(Mutex::new(Vec::new())),
                fail_deletes: false,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");

        let boarded = vec!["c1".to_string(), "c2".to_string(), "c3".to_string()];
        let failures = c
            .close_boarded_cars("t1", &boarded, "abcdef123456", "https://forge/pulls/9")
            .await;

        assert_eq!(failures, 1, "exactly the one bad car (c2) is a failure");
        let w = writes.lock().unwrap();
        for good in ["c1", "c3"] {
            assert!(
                w.contains(&(good.to_string(), "review".to_string())),
                "car {good} must still have its review closed — a bad car must not orphan it"
            );
            assert!(
                w.contains(&(good.to_string(), "meta".to_string())),
                "car {good} must still get metadata.merged — the dispatcher's close marker"
            );
        }
        assert!(
            !w.contains(&("c2".to_string(), "meta".to_string())),
            "c2's write failed, so its close marker must NOT be recorded"
        );
    }

    /// A partial pass leaves the failed car for the next reconcile, and
    /// the re-run is idempotent for the cars that already closed. Pass 1:
    /// car 2's write fails (the other two close). Pass 2: the server now
    /// reports the already-closed reviews as `completed` and car 2's write
    /// succeeds — so `close_boarded_cars` returns 0, re-closes only car 2,
    /// and issues NO duplicate review write for cars 1 and 3.
    #[tokio::test]
    async fn a_partial_close_retries_the_failed_car_idempotently() {
        use axum::extract::Path;
        use axum::routing::{get, put};
        use axum::{Json, Router};
        use std::collections::HashSet;
        use std::sync::{Arc, Mutex};

        // Reviews the server has seen closed; drives idempotence — a car
        // in here reports `review: completed`, so `complete_step`
        // early-returns and issues no second write.
        let reviewed: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        // Car 2 heals between passes.
        let heal_c2 = Arc::new(Mutex::new(false));
        // (car id, endpoint) of every WRITE, across both passes.
        let writes: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

        let reviewed_get = reviewed.clone();
        let reviewed_step = reviewed.clone();
        let heal_step = heal_c2.clone();
        let writes_step = writes.clone();
        let writes_meta = writes.clone();
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let reviewed = reviewed_get.clone();
                    async move {
                        let status = if reviewed.lock().unwrap().contains(&id) {
                            "completed"
                        } else {
                            "ready"
                        };
                        Json(json!({
                            "id": id, "kind": "ship-a-change", "status": "open",
                            "metadata": { "train": "t1", "branch": format!("fix/{id}") },
                            "steps": [{"id": format!("{id}-rev"), "spec_slug": "review",
                                       "title": "Open for review", "status": status, "metadata": {}}]
                        }))
                    }
                })
                .put(move |Path(id): Path<String>, _b: Json<Value>| {
                    let (heal, writes) = (heal_step.clone(), writes_meta.clone());
                    async move {
                        if id == "c2" && !*heal.lock().unwrap() {
                            return (
                                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                                Json(json!({"error": "no"})),
                            );
                        }
                        writes.lock().unwrap().push((id, "meta".into()));
                        (axum::http::StatusCode::OK, Json(json!({})))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                put(
                    move |Path((id, _sid)): Path<(String, String)>, _b: Json<Value>| {
                        let (reviewed, writes) = (reviewed_step.clone(), writes_step.clone());
                        async move {
                            writes.lock().unwrap().push((id.clone(), "review".into()));
                            reviewed.lock().unwrap().insert(id);
                            (axum::http::StatusCode::OK, Json(json!({})))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(FakeForge {
                deleted: Arc::new(Mutex::new(Vec::new())),
                fail_deletes: false,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");
        let boarded = vec!["c1".to_string(), "c2".to_string(), "c3".to_string()];

        // Pass 1: car 2's metadata write is refused — but c2's review PUT
        // still lands first, so only its `meta` write is missing.
        let pass1 = c
            .close_boarded_cars("t1", &boarded, "abcdef123456", "https://forge/pulls/9")
            .await;
        assert_eq!(pass1, 1, "car 2 fails its metadata write on pass 1");

        // Car 2 heals; retry.
        *heal_c2.lock().unwrap() = true;
        let pass2 = c
            .close_boarded_cars("t1", &boarded, "abcdef123456", "https://forge/pulls/9")
            .await;
        assert_eq!(pass2, 0, "the retry recovers car 2 — nothing left orphaned");

        let w = writes.lock().unwrap();
        let review_writes = |id: &str| {
            w.iter()
                .filter(|(cid, ep)| cid == id && ep == "review")
                .count()
        };
        assert_eq!(
            review_writes("c1"),
            1,
            "car 1's review is written once — the retry must NOT re-close a done step"
        );
        assert_eq!(
            review_writes("c3"),
            1,
            "car 3's review is written once — the retry is idempotent"
        );
        assert!(
            w.contains(&("c2".to_string(), "meta".to_string())),
            "car 2's close marker lands on the retry"
        );
    }

    /// The forge as a call recorder for the WHOLE sweep: `delete_branch`
    /// notes the branch, `branch_head` answers a fixed head (so the
    /// guard reads `Delete`), everything else is unreachable. The seam
    /// the sweep's per-branch isolation is proven through.
    struct SweepForge {
        deleted: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        head: String,
        /// An honest forge forgets a branch it deleted, so the read-back
        /// after DELETE answers None. `false` models the 2026-09-11
        /// forge: answers the delete, keeps the branch (1096b1a4).
        performs_deletes: bool,
    }
    #[async_trait::async_trait]
    impl Forge for SweepForge {
        async fn pr_info(&self, _url: &str) -> Result<Value> {
            bail!("not exercised")
        }
        async fn pr_create(
            &self,
            _repo: &str,
            _head_branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<String> {
            bail!("not exercised")
        }
        async fn merge(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn close_pr(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn delete_branch(&self, branch: &str) -> Result<bool> {
            self.deleted.lock().unwrap().push(branch.to_string());
            Ok(true)
        }
        async fn branch_head(&self, branch: &str) -> Result<Option<String>> {
            if self.performs_deletes && self.deleted.lock().unwrap().iter().any(|b| b == branch) {
                return Ok(None);
            }
            Ok(Some(self.head.clone()))
        }
        async fn cancel_ci_runs(&self, _pr_index: &str, _head_sha: &str) -> Result<usize> {
            bail!("not exercised")
        }
    }

    /// THE FLEET-LEVEL ISOLATION, end to end. Two arrived trains are
    /// pending a sweep; train A's arrival-report write (a PATCH to the
    /// jobs API) returns 500 every pass — the exact shape of a boarded
    /// car deleted (404), a malformed report, or a forge blip. Before
    /// the fix, the `?` on that write aborted the WHOLE sweep, so every
    /// LATER pending train went unswept and its landed branch
    /// accumulated on the forge (recurring disk debt). The sweep is now
    /// best-effort per train: A is isolated and B is still swept.
    ///
    /// Ordered A-then-B deliberately — A is processed first, so an
    /// abort takes B down with it under the old code. The assertion is
    /// simply that B's branch WAS deleted.
    #[tokio::test]
    async fn one_trains_sweep_failing_does_not_block_the_next_train() {
        use axum::extract::{Path, RawQuery};
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::get;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        // A closed (arrived) train, `arrived` completed, one boarded
        // car — the shape the sweep filters in as pending.
        let train = |tid: &str| {
            json!({
                "id": tid, "kind": "pr-train", "status": "closed",
                "metadata": { "boarded_jobs": [format!("car-{tid}")] },
                "steps": [
                    {"id":"s-arr","spec_slug":"arrived","title":"Train arrived","status":"completed","metadata":{}}
                ]
            })
        };
        // A landed car: closed + merged, its branch and boarded head on
        // record, so `deletable_branches` yields it and the guard reads
        // Delete.
        let car = |tid: &str, branch: &str| {
            json!({
                "id": format!("car-{tid}"), "kind": "ship-a-change", "status": "closed",
                "metadata": { "train": tid, "branch": branch, "outcome": "merged", "boarded_head": HEAD },
                "steps": []
            })
        };

        let train_a = train("tA");
        let train_b = train("tB");
        let car_a = car("tA", "fix/a");
        let car_b = car("tB", "fix/b");

        // A-then-B: the failing train is swept first, so an all-or-
        // nothing abort strands B.
        let closed_list = json!({ "data": [train_a.clone(), train_b.clone()], "total": 2 });

        let by_id: std::collections::HashMap<String, Value> = [
            ("tA".to_string(), train_a),
            ("tB".to_string(), train_b),
            ("car-tA".to_string(), car_a),
            ("car-tB".to_string(), car_b),
        ]
        .into_iter()
        .collect();
        let by_id = Arc::new(by_id);

        let list_route = closed_list.clone();
        let by_id_get = by_id.clone();
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |RawQuery(q): RawQuery| {
                    let list = list_route.clone();
                    async move {
                        let q = q.unwrap_or_default();
                        // pr-train closed → the pending trains; the open
                        // ship-a-change list (open_car_branches) → none.
                        if q.contains("pr-train") {
                            Json(list)
                        } else {
                            Json(json!({ "data": [], "total": 0 }))
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = by_id_get.clone();
                    async move { Json(by_id.get(&id).cloned().unwrap_or(json!({}))) }
                })
                .put(|Path(_id): Path<String>, _b: Json<Value>| async move { Json(json!({})) }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(|Path(id): Path<String>, _b: Json<Value>| async move {
                    // Train A's arrival report cannot be written — the
                    // persistent per-train failure this test isolates.
                    if id == "tA" {
                        (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response()
                    } else {
                        Json(json!({})).into_response()
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let deleted = Arc::new(Mutex::new(Vec::new()));
        // github forge_kind: the train's OWN branch cleanup is a no-op
        // (the repo auto-deletes merged PR heads), keeping the test on
        // the CAR-branch sweep the isolation guards.
        let mut c = cleanup_conductor(
            "github",
            Box::new(SweepForge {
                deleted: deleted.clone(),
                head: HEAD.to_string(),
                performs_deletes: true,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");

        // The sweep stays green — housekeeping is best-effort — and B's
        // branch is deleted despite A failing.
        c.sweep_landed_branches().await.unwrap();

        let deleted = deleted.lock().unwrap().clone();
        assert!(
            deleted.contains(&"fix/b".to_string()),
            "train B's landed branch went unswept because train A failed first — \
             one bad train stranded the fleet (disk debt): {deleted:?}"
        );
        assert!(
            !deleted.contains(&"fix/a".to_string()),
            "train A aborted before its branch loop, so its branch is untouched \
             this pass and retried next: {deleted:?}"
        );
    }

    /// A forge that ANSWERS the delete and does not perform it: the
    /// 2026-09-11 shape (backlog 1096b1a4) — six landed branches on the
    /// forge the next morning under trains stamped swept. The sweep
    /// must read the branch back, count it a failure, leave the train
    /// UNSTAMPED, and write a `sweep_report` on the train that names
    /// the branch and what the forge said — the evidence that used to
    /// live only in a journal outside anyone's reach.
    #[tokio::test]
    async fn a_delete_the_forge_answered_but_did_not_perform_keeps_the_train_pending() {
        use axum::extract::{Path, RawQuery};
        use axum::routing::get;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        const HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let train = json!({
            "id": "tP", "kind": "pr-train", "status": "closed",
            "metadata": { "boarded_jobs": ["car-tP"] },
            "steps": [
                {"id":"s-arr","spec_slug":"arrived","title":"Train arrived","status":"completed","metadata":{}}
            ]
        });
        let car = json!({
            "id": "car-tP", "kind": "ship-a-change", "status": "closed",
            "metadata": { "train": "tP", "branch": "fix/stays", "outcome": "merged", "boarded_head": HEAD },
            "steps": []
        });
        let closed_list = json!({ "data": [train.clone()], "total": 1 });
        let by_id: std::collections::HashMap<String, Value> =
            [("tP".to_string(), train), ("car-tP".to_string(), car)]
                .into_iter()
                .collect();
        let by_id = Arc::new(by_id);
        let patches: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));

        let list_route = closed_list.clone();
        let by_id_get = by_id.clone();
        let patches_w = patches.clone();
        let puts_w = patches.clone();
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |RawQuery(q): RawQuery| {
                    let list = list_route.clone();
                    async move {
                        if q.unwrap_or_default().contains("pr-train") {
                            Json(list)
                        } else {
                            Json(json!({ "data": [], "total": 0 }))
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = by_id_get.clone();
                    async move { Json(by_id.get(&id).cloned().unwrap_or(json!({}))) }
                })
                // merge_job_metadata writes the WHOLE job back with a PUT;
                // the stamp and the report both arrive through this door.
                .put(move |Path(id): Path<String>, Json(b): Json<Value>| {
                    let puts = puts_w.clone();
                    async move {
                        puts.lock().unwrap().push((id, b));
                        Json(json!({}))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(move |Path(id): Path<String>, Json(b): Json<Value>| {
                    let patches = patches_w.clone();
                    async move {
                        patches.lock().unwrap().push((id, b));
                        Json(json!({}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // SweepForge answers every DELETE with Ok(true) and every
        // branch_head with the same head — a forge that says "deleted"
        // and keeps the branch.
        let deleted = Arc::new(Mutex::new(Vec::new()));
        let mut c = cleanup_conductor(
            "github",
            Box::new(SweepForge {
                deleted: deleted.clone(),
                head: HEAD.to_string(),
                performs_deletes: false,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");
        c.sweep_landed_branches().await.unwrap();

        assert!(
            deleted.lock().unwrap().contains(&"fix/stays".to_string()),
            "the delete was attempted"
        );
        let patches = patches.lock().unwrap().clone();
        let train_patches: Vec<&Value> = patches
            .iter()
            .filter(|(id, _)| id == "tP")
            .map(|(_, b)| b)
            .collect();
        let stamped = train_patches.iter().any(|b| {
            truthy(
                b.pointer("/metadata/branches_swept")
                    .or_else(|| b.get("branches_swept")),
            )
        });
        assert!(
            !stamped,
            "the train was stamped swept while its branch is still on the forge: {train_patches:?}"
        );
        let report = train_patches
            .iter()
            .find_map(|b| {
                b.pointer("/metadata/sweep_report")
                    .or_else(|| b.get("sweep_report"))
            })
            .expect("the sweep wrote a report on the train even though it did not stamp it");
        let text = report.to_string();
        assert!(
            text.contains("fix/stays")
                && text.contains("still present")
                && text.contains("deleted"),
            "the report names the branch, that it is still present, and what the forge said: {text}"
        );
    }

    /// END TO END, the branch the sweep could never see (packet
    /// 473fda1b, generator 1). One arrived train, one landed car that a
    /// rerail had moved onto `feat/x-rerail` — and the original
    /// `feat/x`, which was never a car of its own. The train deletes
    /// what it MERGED, so before this fix the original survived every
    /// sweep forever; now the car's recorded origin is swept in the
    /// same pass, through the same head guard.
    #[tokio::test]
    async fn the_sweep_deletes_a_landed_rerail_original_alongside_its_twin() {
        use axum::extract::{Path, RawQuery};
        use axum::routing::get;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        const HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        let train = json!({
            "id": "t1", "kind": "pr-train", "status": "closed",
            "metadata": { "boarded_jobs": ["car-1"] },
            "steps": [
                {"id":"s-arr","spec_slug":"arrived","title":"Train arrived","status":"completed","metadata":{}}
            ]
        });
        // The car as `boss rerail` leaves it: riding the rerail branch,
        // with the branch it was moved off and that branch's head on the
        // record.
        let car = json!({
            "id": "car-1", "kind": "ship-a-change", "status": "closed",
            "metadata": {
                "train": "t1", "branch": "feat/x-rerail", "outcome": "merged",
                "boarded_head": HEAD,
                "rerail_origins": [{ "branch": "feat/x", "head": HEAD }]
            },
            "steps": []
        });

        let by_id: std::collections::HashMap<String, Value> = [
            ("t1".to_string(), train.clone()),
            ("car-1".to_string(), car),
        ]
        .into_iter()
        .collect();
        let by_id = Arc::new(by_id);
        let closed_list = json!({ "data": [train], "total": 1 });

        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |RawQuery(q): RawQuery| {
                    let list = closed_list.clone();
                    async move {
                        if q.unwrap_or_default().contains("pr-train") {
                            Json(list)
                        } else {
                            Json(json!({ "data": [], "total": 0 }))
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = by_id.clone();
                    async move { Json(by_id.get(&id).cloned().unwrap_or(json!({}))) }
                })
                .put(|Path(_id): Path<String>, _b: Json<Value>| async move { Json(json!({})) }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(|Path(_id): Path<String>, _b: Json<Value>| async move {
                    Json(json!({}))
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let deleted = Arc::new(Mutex::new(Vec::new()));
        let mut c = cleanup_conductor(
            "github",
            Box::new(SweepForge {
                deleted: deleted.clone(),
                head: HEAD.to_string(),
                performs_deletes: true,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");
        c.sweep_landed_branches().await.unwrap();

        let deleted = deleted.lock().unwrap().clone();
        assert!(
            deleted.contains(&"feat/x-rerail".to_string()),
            "the branch the train merged must still be swept: {deleted:?}"
        );
        assert!(
            deleted.contains(&"feat/x".to_string()),
            "the rerail original was never a car, so only its record can \
             reach it — leaked forever without this: {deleted:?}"
        );
    }
}
