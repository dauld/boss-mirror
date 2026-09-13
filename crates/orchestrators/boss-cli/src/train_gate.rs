//! THE TRAIN GATE — the train's Rust checks run where the warm seed is.
//!
//! Design 128b5496 (David, 2026-09-12, all three questions accepted as
//! proposed). CI's `test` job runs `infra/gate.sh` cold on the forge's
//! one runner slot in 11–13 minutes; the cluster gate runs the same
//! script warm on the reflink seed in 3–5. So when the conductor opens a
//! train's PR it also files a gate-run for the TRAIN BRANCH — the
//! assembled tree, which is what a merge would put on main — and the
//! train's verdict is the two read together: the forge's combined
//! status AND this gate-run's verdict, off the system of record, never
//! posted back to the forge (the forge stays a one-way peer).
//!
//! What each reading means for the train (`combined_verdict`):
//!   - no gate yet, or a gate still running → the train is `pending`,
//!     whatever the forge says — a green CI does not merge a train whose
//!     gate has not spoken;
//!   - a red gate is `failing`: it strikes the cars aboard exactly as a
//!     red CI does (the gate's receipt names the failing check; the
//!     conductor's strike path reads the same rollup shape);
//!   - a REFUSED gate (disk floor, network, an unstarted pod) is
//!     infrastructure, not a verdict on the cars: the conductor files the
//!     gate again, up to `MAX_RELAUNCHES` times, and only then reads the
//!     train as `aborted` — released unstruck, the CI-refusal path;
//!   - a LOST gate (the Job died without a verdict) is treated as a
//!     refusal: relaunched, then aborted.
//!
//! The gate-run packet carries `train_gate: true`, `train: <id>` and a
//! `hold`, so the stranded-green sweep reads it as HELD (no car ever
//! parks from it) and the auto-park handler, seeing no park intent,
//! leaves it alone. The packet, the Job and the verdict are the same
//! shapes `boss gate` produces — the conductor is one more caller of
//! that door, not a second gate.

use serde_json::{Value, json};

/// Refusals (and lost Jobs) the conductor absorbs by filing the gate
/// again before it reads the train as aborted. Three: one is weather,
/// two is a bad hour, three in a row is the cluster saying no.
pub(crate) const MAX_RELAUNCHES: u32 = 3;

/// Where the conductor finds the runner manifest `boss gate` renders:
/// the cluster image copies infra/gate-runner there, and the boss-gcp
/// checkout has it at the same path.
pub(crate) const MANIFEST_ENV: &str = "BOSS_GATE_MANIFEST";
pub(crate) const DEFAULT_MANIFEST: &str = "/opt/boss/infra/gate-runner/gate-runner.yaml";
pub(crate) const NAMESPACE_ENV: &str = "BOSS_GATE_NAMESPACE";
pub(crate) const DEFAULT_NAMESPACE: &str = "boss-dev";

/// The metadata keys the train carries about its gate.
pub(crate) const KEY_RUN: &str = "train_gate_run";
pub(crate) const KEY_LAUNCHED_AT: &str = "train_gate_launched_at";
pub(crate) const KEY_RELAUNCHES: &str = "train_gate_relaunches";
pub(crate) const KEY_REFUSALS: &str = "train_gate_refusals";
pub(crate) const KEY_LAUNCH_FAILURES: &str = "train_gate_launch_failures";
pub(crate) const KEY_FALLBACK: &str = "train_gate_fallback";

/// Launch attempts the conductor makes (one per reconcile pass, a minute
/// apart) before a gate it cannot file is read as unavailable.
pub(crate) const MAX_LAUNCH_FAILURES: u32 = 3;

/// Whether a train may merge on CI alone when its gate cannot be filed
/// (BOSS_TRAIN_GATE_REQUIRED). UNTIL CI STOPS RUNNING THE RUST CHECKS
/// (design 128b5496, car 3) the forge still covers the tree, so an
/// unavailable gate falls back to CI — loudly, stamped on the train —
/// rather than holding every train while nobody is watching. Car 3
/// sets this to 1 in the conductor's ConfigMap in the same change that
/// drops CI's `test`/`fast` jobs, and from then on a train waits for its
/// gate, however long.
pub(crate) const REQUIRED_ENV: &str = "BOSS_TRAIN_GATE_REQUIRED";

/// Where a train's gate-run stands, read off its packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Standing {
    /// No verdict step completed yet.
    Pending,
    Green,
    /// `verdict: failed` — the checks ran and one did not pass.
    Failed,
    /// The receipt says `verdict: refused`: infrastructure, not the tree.
    Refused(String),
    /// `verdict: lost` with no refusal on the receipt — the Job died
    /// without saying why.
    Lost,
    /// The conductor could not FILE the gate `MAX_LAUNCH_FAILURES` passes
    /// running (no kubectl, no RBAC, the manifest missing, the cluster at
    /// its gate bound for too long). Read as pending while the gate is
    /// required; as "CI alone, stamped" while it is not.
    Unavailable(String),
}

/// PURE: the standing of one gate-run packet, from its `record-verdict`
/// step (`verdict` green|failed|lost) and the receipt it carries (a
/// JSON string; `verdict: refused` + `refused_because` for a refusal).
pub(crate) fn standing(gate_run: &Value) -> Standing {
    let step = gate_run
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("record-verdict"));
    let md = step.and_then(|s| s.get("metadata"));
    let receipt: Option<Value> = md.and_then(|m| m.get("receipt")).and_then(|r| match r {
        Value::String(s) => serde_json::from_str(s).ok(),
        Value::Object(_) => Some(r.clone()),
        _ => None,
    });
    if let Some(r) = &receipt
        && r.get("verdict").and_then(Value::as_str) == Some("refused")
    {
        let why = r
            .get("refused_because")
            .and_then(Value::as_str)
            .unwrap_or("the gate refused before any check ran (no reason recorded)")
            .to_string();
        return Standing::Refused(why);
    }
    match md.and_then(|m| m.get("verdict")).and_then(Value::as_str) {
        Some("green") => Standing::Green,
        Some("failed") => Standing::Failed,
        Some("lost") => Standing::Lost,
        _ => Standing::Pending,
    }
}

/// PURE: the train's verdict from CI's (`green|pending|failing|aborted`,
/// as `ci_verdict` reads the forge) and the gate's standing. `None`
/// means no gate has been filed for this train yet.
pub(crate) fn combined_verdict(
    ci: &str,
    gate: Option<&Standing>,
    relaunches: u32,
    required: bool,
) -> &'static str {
    // A red or aborted CI settles the train on its own; the gate need
    // not be waited for to release the cars.
    if ci == "failing" {
        return "failing";
    }
    if ci == "aborted" {
        return "aborted";
    }
    match gate {
        None | Some(Standing::Pending) => "pending",
        Some(Standing::Unavailable(_)) => {
            if required {
                "pending"
            } else {
                // CI alone — the forge still runs the Rust checks until
                // car 3; the train carries KEY_FALLBACK saying so.
                match ci {
                    "green" => "green",
                    _ => "pending",
                }
            }
        }
        Some(Standing::Green) => {
            if ci == "green" {
                "green"
            } else {
                "pending"
            }
        }
        Some(Standing::Failed) => "failing",
        Some(Standing::Refused(_)) | Some(Standing::Lost) => {
            if relaunches >= MAX_RELAUNCHES {
                "aborted"
            } else {
                "pending"
            }
        }
    }
}

/// PURE: does this standing call for filing the gate again? A refusal
/// or a lost Job, while relaunches remain.
pub(crate) fn wants_relaunch(gate: &Standing, relaunches: u32) -> bool {
    matches!(gate, Standing::Refused(_) | Standing::Lost) && relaunches < MAX_RELAUNCHES
}

/// The metadata the gate-run packet carries beyond `boss gate`'s own
/// body: which train it gates, and the hold that keeps the stranded
/// sweep and the auto-park handler off it.
pub(crate) fn packet_marks(train_id: &str, train_title: &str) -> Value {
    json!({
        "train_gate": true,
        "train": train_id,
        "hold": format!(
            "train gate for {train_title} ({}) — the conductor reads this verdict off the packet; no car parks from it (design 128b5496)",
            &train_id[..8.min(train_id.len())]
        ),
    })
}

/// The one line the conductor logs and stamps when it reads the gate.
pub(crate) fn describe(gate: Option<&Standing>, relaunches: u32) -> String {
    match gate {
        None => "train gate: not filed yet".to_string(),
        Some(Standing::Pending) => "train gate: running".to_string(),
        Some(Standing::Green) => "train gate: green".to_string(),
        Some(Standing::Failed) => "train gate: RED — strikes the cars aboard".to_string(),
        Some(Standing::Refused(why)) => format!(
            "train gate: REFUSED ({why}) — {}",
            if relaunches < MAX_RELAUNCHES {
                format!(
                    "filing it again ({} of {MAX_RELAUNCHES} relaunches used)",
                    relaunches
                )
            } else {
                format!(
                    "{MAX_RELAUNCHES} relaunches spent; the train reads aborted, cars released unstruck"
                )
            }
        ),
        Some(Standing::Lost) => format!(
            "train gate: LOST (the Job ended without a verdict) — {}",
            if relaunches < MAX_RELAUNCHES {
                format!(
                    "filing it again ({} of {MAX_RELAUNCHES} relaunches used)",
                    relaunches
                )
            } else {
                format!(
                    "{MAX_RELAUNCHES} relaunches spent; the train reads aborted, cars released unstruck"
                )
            }
        ),
        Some(Standing::Unavailable(why)) => format!(
            "train gate: UNAVAILABLE — could not be filed {MAX_LAUNCH_FAILURES} passes running ({why}); \
             CI alone judges this train while the gate is not required ({REQUIRED_ENV})"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(verdict: Option<&str>, receipt: Option<Value>) -> Value {
        let mut md = json!({});
        if let Some(v) = verdict {
            md["verdict"] = json!(v);
        }
        if let Some(r) = receipt {
            md["receipt"] = json!(r.to_string());
        }
        json!({ "id": "g1", "steps": [
            { "spec_slug": "launch", "status": "completed", "metadata": {} },
            { "spec_slug": "record-verdict", "status": if verdict.is_some() { "completed" } else { "ready" }, "metadata": md }
        ]})
    }

    #[test]
    fn a_gate_run_is_read_off_its_verdict_step_and_receipt() {
        assert_eq!(standing(&run(None, None)), Standing::Pending);
        assert_eq!(standing(&run(Some("green"), None)), Standing::Green);
        assert_eq!(standing(&run(Some("failed"), None)), Standing::Failed);
        assert_eq!(standing(&run(Some("lost"), None)), Standing::Lost);
        assert_eq!(
            standing(&run(
                Some("lost"),
                Some(json!({"verdict": "refused", "refused_because": "disk floor: 61GB free"}))
            )),
            Standing::Refused("disk floor: 61GB free".into())
        );
        // A receipt already parsed to an object reads the same.
        let mut r = run(Some("lost"), None);
        r["steps"][1]["metadata"]["receipt"] =
            json!({"verdict": "refused", "refused_because": "no route"});
        assert_eq!(standing(&r), Standing::Refused("no route".into()));
    }

    #[test]
    fn a_green_ci_does_not_merge_a_train_whose_gate_has_not_spoken() {
        assert_eq!(combined_verdict("green", None, 0, true), "pending");
        assert_eq!(
            combined_verdict("green", Some(&Standing::Pending), 0, true),
            "pending"
        );
        assert_eq!(
            combined_verdict("pending", Some(&Standing::Green), 0, true),
            "pending"
        );
        assert_eq!(
            combined_verdict("green", Some(&Standing::Green), 0, true),
            "green"
        );
    }

    #[test]
    fn a_red_gate_strikes_like_a_red_ci_and_a_red_ci_settles_the_train_alone() {
        assert_eq!(
            combined_verdict("green", Some(&Standing::Failed), 0, true),
            "failing"
        );
        assert_eq!(
            combined_verdict("pending", Some(&Standing::Failed), 0, true),
            "failing"
        );
        assert_eq!(combined_verdict("failing", None, 0, true), "failing");
        assert_eq!(
            combined_verdict("failing", Some(&Standing::Green), 0, true),
            "failing"
        );
        assert_eq!(
            combined_verdict("aborted", Some(&Standing::Pending), 0, true),
            "aborted"
        );
    }

    #[test]
    fn a_refusal_is_relaunched_three_times_and_then_aborts_the_train_unstruck() {
        let refused = Standing::Refused("disk floor".into());
        for n in 0..MAX_RELAUNCHES {
            assert!(wants_relaunch(&refused, n), "relaunch {n}");
            assert_eq!(
                combined_verdict("green", Some(&refused), n, true),
                "pending"
            );
        }
        assert!(!wants_relaunch(&refused, MAX_RELAUNCHES));
        assert_eq!(
            combined_verdict("green", Some(&refused), MAX_RELAUNCHES, true),
            "aborted"
        );
        assert!(wants_relaunch(&Standing::Lost, 0));
        assert!(!wants_relaunch(&Standing::Green, 0) && !wants_relaunch(&Standing::Failed, 0));
        assert_eq!(
            combined_verdict("green", Some(&Standing::Lost), MAX_RELAUNCHES, true),
            "aborted"
        );
    }

    /// A gate the conductor cannot file: while the gate is required the
    /// train waits; while it is not (CI still runs the Rust checks), CI
    /// alone judges it — green merges, red strikes, pending waits.
    #[test]
    fn an_unavailable_gate_holds_a_required_train_and_falls_back_to_ci_otherwise() {
        let u = Standing::Unavailable("no kubectl".into());
        assert_eq!(combined_verdict("green", Some(&u), 0, true), "pending");
        assert_eq!(combined_verdict("green", Some(&u), 0, false), "green");
        assert_eq!(combined_verdict("pending", Some(&u), 0, false), "pending");
        assert_eq!(combined_verdict("failing", Some(&u), 0, false), "failing");
        assert!(!wants_relaunch(&u, 0));
        let d = describe(Some(&u), 0);
        assert!(
            d.contains("UNAVAILABLE") && d.contains("no kubectl") && d.contains(REQUIRED_ENV),
            "{d}"
        );
    }

    #[test]
    fn the_packet_marks_hold_it_out_of_the_stranded_sweep_and_name_the_train() {
        let m = packet_marks("0123456789abcdef", "PR train 2026-09-13 01:00");
        assert_eq!(m["train_gate"], true);
        assert_eq!(m["train"], "0123456789abcdef");
        let hold = m["hold"].as_str().unwrap();
        assert!(
            hold.contains("PR train 2026-09-13 01:00") && hold.contains("01234567"),
            "{hold}"
        );
        assert!(
            boss_jobs::stranded::hold_reason(&m).is_some(),
            "a hold is what keeps the stranded sweep off it"
        );
        assert!(
            !boss_jobs::stranded::park_intent(&m),
            "no park intent: the auto-park handler leaves it alone"
        );
    }

    #[test]
    fn the_description_says_what_the_conductor_will_do_next() {
        assert!(describe(None, 0).contains("not filed"));
        assert!(describe(Some(&Standing::Failed), 0).contains("strikes"));
        let d = describe(Some(&Standing::Refused("no disk".into())), 1);
        assert!(d.contains("no disk") && d.contains("1 of 3"), "{d}");
        let d = describe(Some(&Standing::Lost), MAX_RELAUNCHES);
        assert!(d.contains("aborted"), "{d}");
    }
}
