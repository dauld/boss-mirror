//! WHAT THE CONDUCTOR DECIDES BY. `train.rs` is what it DOES.
//!
//! That sentence is the whole boundary, and it is why this is a separate
//! file (David, 2026-08-24, resolving Q3: "it is part of the platform,
//! but we still want good separation of concerns"). Every number and
//! list in here is a POLICY question somebody could reasonably want
//! answered differently tomorrow; nothing in here merges, pushes, opens
//! a PR, or talks to a forge. `train.rs` does all of that, and now takes
//! its thresholds from a `DeliveryPolicy` handed to it — so a reader can
//! tell at a glance which code decides and which executes.
//!
//! The policy lives in the `delivery_policy` registry
//! (`infra/postgres/schema/202608242117-delivery-policy-registry.sql`),
//! is resolved ONCE per conductor invocation, and is threaded to the
//! decision points. Changing it is a registry write that takes effect on
//! the next boarding — no build, no deploy, no train
//! (docs/design/delivery-as-protocol.md).
//!
//! THREE RULES KEEP IT FROM BECOMING WHAT IT REPLACES.
//!
//!   - THE FALLBACK IS THE OLD BEHAVIOUR, NOT A FAILURE. An unreachable
//!     or empty registry, or a row that does not parse, drops the
//!     conductor onto `DeliveryPolicy::compiled()` — the exact constants
//!     that were in `train.rs` before this file existed — and journals
//!     one loud line. A policy registry must not become a new way to
//!     wedge every train.
//!   - ALL OR NOTHING. A bad row is refused whole. Half a policy would
//!     silently pair registry values with compiled ones and nobody could
//!     afterwards say which rules a train actually ran under.
//!   - IN-FLIGHT TRAINS PIN. The version resolved at boarding is stamped
//!     on the train Job (`metadata.delivery_policy_version`) and
//!     reconcile judges that train against the version it departed
//!     under, exactly as a packet stays on the workflow version it was
//!     admitted under.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use boss_jobs::delivery::DeliveryPolicyRow;

/// The registry row the train conductor reads. One name, because there
/// is one delivery pipeline; the version is what moves.
pub(crate) const POLICY_NAME: &str = "train-conductor";

/// The version stamped on a policy that did not come from the registry.
/// Never pinned on a train — a record that claims a registry read which
/// never happened is worse than a record that claims nothing.
pub(crate) const NO_VERSION: i32 = 0;

// ---------------------------------------------------------------------------
// The compiled fallback — what `train.rs` used to hold, verbatim.
//
// These are not defaults in the "sensible starting value" sense. They
// are the values the pipeline ran on the day the policy moved into the
// registry, kept here so that losing the registry loses no behaviour.
// `db_tests::the_seeded_policy_equals_the_compiled_fallback` pins the
// seed row against them, so the two cannot drift (CLAUDE.md §9a).
// ---------------------------------------------------------------------------

/// How many red trains a car may ride before boarding leaves it behind.
/// Two: one red is bad luck — the fault is usually a neighbour's — and a
/// second aboard a different consist is the car itself.
pub(crate) const COMPILED_MAX_RED_TRAINS: i64 = 2;

/// Hours without a step completion before an open train counts stalled.
pub(crate) const COMPILED_STALL_HOURS: i64 = 6;

/// Wall clock the whole consist check may spend before it stops asking
/// and lets the train go. The measured set costs ~9 seconds, so this is
/// roughly six times headroom — wide enough that a slow box never trips
/// it, narrow enough that "seconds, not minutes" stays true if something
/// expensive lands in `infra/lint/` unannounced.
pub(crate) const COMPILED_CONSIST_BUDGET_SECS: u64 = 60;

/// How much of a failing lint's output goes on the record: enough to act
/// on, bounded so a chatty check cannot bloat a Job's metadata.
pub(crate) const COMPILED_CONSIST_OUTPUT_BUDGET: usize = 1200;

/// How many files a consist refusal names. The reason string is read on
/// a chip in the yard; past a handful it stops being a hint.
pub(crate) const COMPILED_CONSIST_FILES_NAMED: usize = 6;

/// Character budget for the file list in a conflict skip reason. The
/// reason lands on the car Job's `metadata.skip_reason`, which the
/// yard's PacketCard renders as a chip ("LEFT BEHIND — <reason>") — past
/// this budget the list truncates to a count.
pub(crate) const COMPILED_SKIP_REASON_FILE_BUDGET: usize = 96;

/// Character budget for a jobs-API blip's cause in the journal.
pub(crate) const COMPILED_BLIP_CAUSE_BUDGET: usize = 80;

/// GB of free disk the CI host's latest host-scope estate observation
/// must show before boarding assembles a consist (2026-09-03: two
/// consists boarded onto a full CI host and each burned a whole CI
/// cycle discovering it). Unlike its siblings this constant has no
/// pre-registry ancestor in `train.rs` — the check is new — so the
/// number is derived rather than carried over: gate.sh's disk-floor
/// note measured a COLD workspace build at ~74GB on the forge host,
/// and the locomotive's run-START floor of 70 passed that night while
/// the run still died mid-flight, because a start floor cannot see the
/// consist's mid-run consumption. 90 is the measured cold build plus
/// headroom for exactly that growth.
pub(crate) const COMPILED_CI_HOST_FLOOR_GB: i64 = 40;

/// How many gates `boss gate` admits at once before it refuses. Unlike
/// most of its siblings this constant DID have a pre-registry ancestor —
/// `gate.rs`'s `DEFAULT_MAX_CONCURRENT`, kept there now only as the
/// ultimate fallback when neither the env override nor a policy row can
/// be read. Three is the measured comfort zone on the build node (w-1):
/// at FIVE concurrent gates I/O pressure hit 65% and a ~35-minute gate
/// took ~93, so per-verdict latency degraded past two gates' worth of
/// queueing (2026-08-26). Raising it to 4 is a policy edit, not a
/// deploy — which is the whole point of moving it here.
pub(crate) const COMPILED_GATE_MAX_CONCURRENT: i64 = 3;

/// The lints the consist check does not run, each with its reason. All
/// four need something the assembled TREE does not contain — a cargo
/// build, a package manager, or a live database — and a question the
/// tree cannot answer is not a question that can be answered in seconds.
/// Everything else in `infra/lint/` runs, including lints that do not
/// exist yet.
pub(crate) const COMPILED_EXCLUDED_LINTS: &[(&str, &str)] = &[
    (
        "audit-ordering.sh",
        "psql against a live database — same: not a question about a tree",
    ),
    (
        "conservation-invariants.sh",
        "psql + curl against a LIVE deployment — an invariant on the running system, which a \
         tree cannot answer (it has its own systemd timer)",
    ),
    (
        "no-snapshot-arrays.sh",
        "reads the built `boss-ports-list` binary; with no target/ it can only report \
         'not found', and building it is exactly what CI is for",
    ),
    (
        "svelte-check.sh",
        "runs `bun install --frozen-lockfile` and a typecheck — minutes, plus a network fetch, \
         and it exits 1 outright on a box without bun",
    ),
];

// ---------------------------------------------------------------------------
// The resolved policy
// ---------------------------------------------------------------------------

/// One lint the consist check skips, and why. The reason travels with
/// the entry because an unexplained exemption is how a check quietly
/// stops covering anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExcludedLint {
    pub(crate) script: String,
    pub(crate) reason: String,
}

/// The delivery policy in force for one conductor invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliveryPolicy {
    /// The registry version this came from, or `NO_VERSION` for the
    /// compiled fallback.
    pub(crate) version: i32,
    pub(crate) max_red_trains: i64,
    pub(crate) stall_hours: i64,
    /// Sorted by script name, so two reads of one row produce one
    /// roster in one order.
    pub(crate) excluded_lints: Vec<ExcludedLint>,
    pub(crate) consist_budget: Duration,
    pub(crate) consist_output_budget: usize,
    pub(crate) consist_files_named: usize,
    pub(crate) skip_reason_file_budget: usize,
    pub(crate) blip_cause_budget: usize,
    pub(crate) ci_host_floor_gb: i64,
    /// The concurrency bound `boss gate` enforces. Read here so the
    /// number the CLI obeys and the number the yard draws are one.
    pub(crate) gate_max_concurrent: i64,
}

impl DeliveryPolicy {
    /// The constants `train.rs` held before the registry existed.
    pub(crate) fn compiled() -> Self {
        DeliveryPolicy {
            version: NO_VERSION,
            max_red_trains: COMPILED_MAX_RED_TRAINS,
            stall_hours: COMPILED_STALL_HOURS,
            excluded_lints: COMPILED_EXCLUDED_LINTS
                .iter()
                .map(|(script, reason)| ExcludedLint {
                    script: (*script).to_string(),
                    reason: (*reason).to_string(),
                })
                .collect(),
            consist_budget: Duration::from_secs(COMPILED_CONSIST_BUDGET_SECS),
            consist_output_budget: COMPILED_CONSIST_OUTPUT_BUDGET,
            consist_files_named: COMPILED_CONSIST_FILES_NAMED,
            skip_reason_file_budget: COMPILED_SKIP_REASON_FILE_BUDGET,
            blip_cause_budget: COMPILED_BLIP_CAUSE_BUDGET,
            ci_host_floor_gb: COMPILED_CI_HOST_FLOOR_GB,
            gate_max_concurrent: COMPILED_GATE_MAX_CONCURRENT,
        }
    }

    /// Did this come out of the registry, or off the fallback?
    pub(crate) fn is_from_registry(&self) -> bool {
        self.version != NO_VERSION
    }

    /// Is `script` one the consist check skips?
    pub(crate) fn excludes(&self, script: &str) -> bool {
        self.excluded_lints.iter().any(|e| e.script == script)
    }
}

// ---------------------------------------------------------------------------
// Parsing — the conductor owns it, because it owns the consequence
// ---------------------------------------------------------------------------

fn positive_i64(field: &str, v: i32) -> Result<i64> {
    if v <= 0 {
        bail!("{field} must be positive, got {v}");
    }
    Ok(v as i64)
}

fn positive_usize(field: &str, v: i32) -> Result<usize> {
    usize::try_from(positive_i64(field, v)?)
        .map_err(|_| anyhow!("{field} does not fit this machine's usize: {v}"))
}

fn excluded_lints(raw: &Value) -> Result<Vec<ExcludedLint>> {
    let entries = raw
        .as_array()
        .ok_or_else(|| anyhow!("consist_excluded_lints must be a JSON array, got {raw}"))?;
    let mut out: Vec<ExcludedLint> = Vec::with_capacity(entries.len());
    for e in entries {
        let script = e
            .get("script")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("consist_excluded_lints entry names no script: {e}"))?;
        // The reason is required, not decorative: an exemption nobody
        // explained is one nobody can later judge.
        let reason = e
            .get("reason")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("consist_excluded_lints entry {script} gives no reason"))?;
        out.push(ExcludedLint {
            script: script.to_string(),
            reason: reason.to_string(),
        });
    }
    out.sort_by(|a, b| a.script.cmp(&b.script));
    Ok(out)
}

/// A registry row becomes a policy, or it does not become one at all.
pub(crate) fn parse(row: DeliveryPolicyRow) -> Result<DeliveryPolicy> {
    Ok(DeliveryPolicy {
        version: row.version,
        max_red_trains: positive_i64("max_red_trains", row.max_red_trains)?,
        stall_hours: positive_i64("stall_hours", row.stall_hours)?,
        excluded_lints: excluded_lints(&row.consist_excluded_lints)?,
        consist_budget: Duration::from_secs(positive_i64(
            "consist_budget_secs",
            row.consist_budget_secs,
        )? as u64),
        consist_output_budget: positive_usize("consist_output_budget", row.consist_output_budget)?,
        consist_files_named: positive_usize("consist_files_named", row.consist_files_named)?,
        skip_reason_file_budget: positive_usize(
            "skip_reason_file_budget",
            row.skip_reason_file_budget,
        )?,
        blip_cause_budget: positive_usize("blip_cause_budget", row.blip_cause_budget)?,
        ci_host_floor_gb: positive_i64("ci_host_floor_gb", row.ci_host_floor_gb)?,
        gate_max_concurrent: positive_i64("gate_max_concurrent", row.gate_max_concurrent)?,
    })
}

/// Turn whatever the registry read produced into the policy to run on.
/// Never fails: the compiled fallback is always available, and every
/// path onto it says so exactly once, in the caller's journal idiom.
pub(crate) fn resolve_from(
    fetched: Result<Option<DeliveryPolicyRow>>,
    journal: &dyn Fn(&str),
) -> DeliveryPolicy {
    let fell_back = |why: String| -> DeliveryPolicy {
        journal(&format!(
            "delivery policy: {why} — running on the compiled defaults \
             (hold {COMPILED_MAX_RED_TRAINS}, stall {COMPILED_STALL_HOURS}h); \
             no policy version will be pinned on this train"
        ));
        DeliveryPolicy::compiled()
    };
    match fetched {
        Err(e) => fell_back(format!("registry unreachable ({})", one_line(&e))),
        Ok(None) => fell_back(format!("no active `{POLICY_NAME}` row")),
        Ok(Some(row)) => match parse(row) {
            Ok(p) => p,
            Err(e) => fell_back(format!("active row is unusable ({e})")),
        },
    }
}

/// The first line of an error chain — the journal already implies the
/// url, and a wrapped cause reads as three copies of one fact.
fn one_line(e: &anyhow::Error) -> String {
    let text = format!("{e}");
    text.lines().next().unwrap_or_default().trim().to_string()
}

// ---------------------------------------------------------------------------
// Pinning — a train is judged by the rules it departed under
// ---------------------------------------------------------------------------

/// What a departing train records about the policy it left on. Empty
/// when the conductor ran on the compiled fallback: there is no version
/// to name, and inventing one would make the record lie.
pub(crate) fn pin_stamps(policy: &DeliveryPolicy) -> Vec<(&'static str, Value)> {
    if !policy.is_from_registry() {
        return Vec::new();
    }
    vec![("delivery_policy_version", json!(policy.version))]
}

/// The policy version a train departed under, if it recorded one.
/// Trains that departed before pinning existed have none, and are
/// judged by whatever is in force — the only honest answer available.
pub(crate) fn pinned_version(train: &Value) -> Option<i32> {
    train
        .get("metadata")?
        .get("delivery_policy_version")?
        .as_i64()?
        .try_into()
        .ok()
}

/// Which version reconcile must go and fetch for this train — `None`
/// when the active policy already IS the pinned one, which is the
/// ordinary case and costs no call.
pub(crate) fn version_to_fetch(train: &Value, active: &DeliveryPolicy) -> Option<i32> {
    let pinned = pinned_version(train)?;
    (pinned != active.version).then_some(pinned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scripts(p: &DeliveryPolicy) -> Vec<String> {
        p.excluded_lints.iter().map(|e| e.script.clone()).collect()
    }

    // -- the equality that makes this car a no-op -----------------------

    #[test]
    fn the_compiled_fallback_is_the_conductors_previous_constants() {
        let p = DeliveryPolicy::compiled();
        assert_eq!(p.max_red_trains, 2, "train.rs MAX_RED_TRAINS");
        assert_eq!(p.stall_hours, 6, "Config::stall_hours default");
        assert_eq!(p.consist_budget, Duration::from_secs(60));
        assert_eq!(p.consist_output_budget, 1200);
        assert_eq!(p.consist_files_named, 6);
        assert_eq!(p.skip_reason_file_budget, 96);
        assert_eq!(p.blip_cause_budget, 80);
        assert_eq!(
            p.ci_host_floor_gb, 40,
            "policy v3 (approval d99b198d, 2026-09-05): the forge runs lean CI green at \
             60-70GB free and the locomotive's own door is 70, so the boarding floor sits \
             under it; the old 90 measured a FULL gate build that now runs in-cluster"
        );
        assert_eq!(
            p.gate_max_concurrent, 3,
            "gate.rs DEFAULT_MAX_CONCURRENT — the measured comfort zone on w-1"
        );
        assert_eq!(
            scripts(&p),
            vec![
                "audit-ordering.sh".to_string(),
                "conservation-invariants.sh".to_string(),
                "no-snapshot-arrays.sh".to_string(),
                "svelte-check.sh".to_string(),
            ],
            "the four lints that need more than a tree"
        );
        assert_eq!(
            p.version, NO_VERSION,
            "the compiled fallback is not a registry version and must never \
             be pinned on a train as if it were"
        );
    }

    // -- parsing a row -------------------------------------------------

    fn row() -> DeliveryPolicyRow {
        DeliveryPolicyRow {
            name: POLICY_NAME.to_string(),
            version: 7,
            max_red_trains: 3,
            stall_hours: 9,
            consist_excluded_lints: json!([
                {"script": "svelte-check.sh", "reason": "needs bun"},
            ]),
            consist_budget_secs: 30,
            consist_output_budget: 400,
            consist_files_named: 2,
            skip_reason_file_budget: 50,
            blip_cause_budget: 40,
            ci_host_floor_gb: 120,
            gate_max_concurrent: 4,
        }
    }

    #[test]
    fn a_well_formed_row_becomes_the_policy_it_states() {
        let p = parse(row()).expect("a complete row parses");
        assert_eq!(p.version, 7);
        assert_eq!(p.max_red_trains, 3);
        assert_eq!(p.stall_hours, 9);
        assert_eq!(p.consist_budget, Duration::from_secs(30));
        assert_eq!(p.consist_output_budget, 400);
        assert_eq!(p.consist_files_named, 2);
        assert_eq!(p.skip_reason_file_budget, 50);
        assert_eq!(p.blip_cause_budget, 40);
        assert_eq!(p.ci_host_floor_gb, 120);
        assert_eq!(p.gate_max_concurrent, 4);
        assert_eq!(scripts(&p), vec!["svelte-check.sh".to_string()]);
        assert!(p.excludes("svelte-check.sh"));
        assert!(!p.excludes("migration-numbers-unique.sh"));
    }

    #[test]
    fn an_empty_exclusion_list_is_legal() {
        // "Run every lint in the tree" is a policy someone may choose,
        // and it must not read as a broken row.
        let mut r = row();
        r.consist_excluded_lints = json!([]);
        assert!(
            parse(r)
                .expect("empty is not malformed")
                .excluded_lints
                .is_empty()
        );
    }

    #[test]
    fn the_exclusion_roster_comes_back_sorted() {
        // Two reads of one row must ask the same questions in the same
        // sequence, whatever order the row was authored in.
        let mut r = row();
        r.consist_excluded_lints = json!([
            {"script": "svelte-check.sh", "reason": "bun"},
            {"script": "audit-ordering.sh", "reason": "psql"},
        ]);
        assert_eq!(
            scripts(&parse(r).unwrap()),
            vec![
                "audit-ordering.sh".to_string(),
                "svelte-check.sh".to_string()
            ]
        );
    }

    #[test]
    fn a_non_positive_number_is_refused_rather_than_obeyed() {
        // The schema's CHECKs say the same thing, but the conductor
        // reads over HTTP and must not depend on having been served by
        // the database it thinks it was.
        let mut r = row();
        r.consist_files_named = 0;
        let e = parse(r).expect_err("zero is not a budget");
        assert!(
            format!("{e}").contains("consist_files_named"),
            "the complaint names the field so the fix is obvious: {e}"
        );
    }

    #[test]
    fn a_non_positive_ci_host_floor_is_refused() {
        // A zero floor would wave every boarding through and read as
        // "the check ran"; that is worse than no check at all.
        let mut r = row();
        r.ci_host_floor_gb = 0;
        let e = parse(r).expect_err("zero is not a floor");
        assert!(
            format!("{e}").contains("ci_host_floor_gb"),
            "the complaint names the field: {e}"
        );
    }

    #[test]
    fn a_non_positive_gate_max_concurrent_is_refused() {
        // Zero would refuse every gate forever, which is a
        // misconfiguration, not a policy — the parse rejects it the same
        // way the CLI's env-var parser rejects BOSS_GATE_MAX_CONCURRENT=0.
        let mut r = row();
        r.gate_max_concurrent = 0;
        let e = parse(r).expect_err("zero admits no gates");
        assert!(
            format!("{e}").contains("gate_max_concurrent"),
            "the complaint names the field: {e}"
        );
    }

    #[test]
    fn an_exclusion_that_is_not_a_script_name_is_refused() {
        let mut r = row();
        r.consist_excluded_lints = json!([{"reason": "no script key"}]);
        let e = parse(r).expect_err("an entry with no script names nothing");
        assert!(
            format!("{e}").contains("consist_excluded_lints"),
            "the complaint names the field: {e}"
        );
    }

    #[test]
    fn an_exclusion_with_no_reason_is_refused() {
        let mut r = row();
        r.consist_excluded_lints = json!([{"script": "svelte-check.sh"}]);
        let e = parse(r).expect_err("an unexplained exemption is not reviewable");
        assert!(format!("{e}").contains("svelte-check.sh"), "{e}");
    }

    // -- resolution, and the loud fallback -----------------------------

    /// Journal capture — the conductor's loud line is part of the
    /// contract, not decoration. A registry that silently stops
    /// answering must not look like a registry that agrees with the
    /// compiled defaults.
    fn journalled(fetched: Result<Option<DeliveryPolicyRow>>) -> (DeliveryPolicy, Vec<String>) {
        let lines = std::cell::RefCell::new(Vec::new());
        let policy = resolve_from(fetched, &|m: &str| lines.borrow_mut().push(m.to_string()));
        (policy, lines.into_inner())
    }

    #[test]
    fn a_readable_row_is_the_policy_and_says_nothing() {
        let (p, lines) = journalled(Ok(Some(row())));
        assert_eq!(p.version, 7);
        assert!(lines.is_empty(), "the happy path is quiet: {lines:?}");
    }

    #[test]
    fn an_unreachable_registry_falls_back_loudly() {
        let (p, lines) = journalled(Err(anyhow!("Connection refused (os error 61)")));
        assert_eq!(
            p,
            DeliveryPolicy::compiled(),
            "a policy registry must not become a new way to wedge every train"
        );
        assert_eq!(lines.len(), 1, "exactly one line, not a per-decision drip");
        assert!(
            lines[0].contains("compiled defaults") && lines[0].contains("Connection refused"),
            "the line says what was used and why: {lines:?}"
        );
    }

    #[test]
    fn an_empty_registry_falls_back_loudly() {
        let (p, lines) = journalled(Ok(None));
        assert_eq!(p, DeliveryPolicy::compiled());
        assert!(
            lines.iter().any(|l| l.contains("no active")),
            "an empty registry reads differently from an unreachable one: {lines:?}"
        );
    }

    #[test]
    fn a_malformed_row_falls_back_loudly_rather_than_half_applying() {
        let mut r = row();
        r.stall_hours = -1;
        let (p, lines) = journalled(Ok(Some(r)));
        assert_eq!(
            p,
            DeliveryPolicy::compiled(),
            "half a policy is worse than none — the good fields would silently \
             pair with compiled ones and nobody could say which rules ran"
        );
        assert!(lines.iter().any(|l| l.contains("stall_hours")), "{lines:?}");
    }

    // -- pinning -------------------------------------------------------

    /// §9a pin across crates: the yard read-model draws its gate slots
    /// at `boss_jobs::yard::COMPILED_GATE_MAX_CONCURRENT` when no policy
    /// answers, and `boss gate` refuses at THIS crate's
    /// `COMPILED_GATE_MAX_CONCURRENT` on the same no-policy path. If the
    /// two drifted the page would draw a different number of slots than a
    /// gate would honour — the exact folklore this car retires. They
    /// cannot be one constant (different crates), so equality is the
    /// mechanism.
    #[test]
    fn the_yard_no_policy_capacity_equals_the_cli_compiled_bound() {
        assert_eq!(
            i64::from(boss_jobs::yard::COMPILED_GATE_MAX_CONCURRENT),
            COMPILED_GATE_MAX_CONCURRENT,
            "the yard's no-policy slot count drifted from the gate CLI's compiled bound"
        );
    }

    #[test]
    fn a_departing_train_carries_the_version_it_left_under() {
        let stamps = pin_stamps(&parse(row()).unwrap());
        assert_eq!(stamps, vec![("delivery_policy_version", json!(7))]);
    }

    #[test]
    fn a_train_departing_on_the_compiled_fallback_pins_nothing() {
        // There is no version to pin, and stamping a fake one would make
        // the record claim a registry read that never happened.
        assert!(pin_stamps(&DeliveryPolicy::compiled()).is_empty());
    }

    #[test]
    fn the_pin_is_read_back_off_the_trains_metadata() {
        let train = json!({"metadata": {"delivery_policy_version": 7}});
        assert_eq!(pinned_version(&train), Some(7));
    }

    #[test]
    fn a_train_that_departed_before_pinning_existed_has_no_pin() {
        assert_eq!(pinned_version(&json!({"metadata": {}})), None);
        assert_eq!(pinned_version(&json!({})), None);
        // A non-numeric stamp is not a version — reading it as one would
        // send reconcile hunting for a policy nobody wrote.
        assert_eq!(
            pinned_version(&json!({"metadata": {"delivery_policy_version": "7"}})),
            None
        );
    }

    #[test]
    fn a_pin_matching_the_active_version_needs_no_second_read() {
        let active = parse(row()).unwrap();
        let train = json!({"metadata": {"delivery_policy_version": 7}});
        assert_eq!(
            version_to_fetch(&train, &active),
            None,
            "the common case is that nothing changed mid-flight; a fetch per \
             train per tick would be pure cost"
        );
    }

    #[test]
    fn a_pin_that_differs_from_the_active_version_is_fetched() {
        let active = parse(row()).unwrap();
        let train = json!({"metadata": {"delivery_policy_version": 5}});
        assert_eq!(
            version_to_fetch(&train, &active),
            Some(5),
            "an edit landed while this train was running — it is judged by the \
             rules it departed under, not the ones in force now"
        );
    }

    #[test]
    fn an_unpinned_train_is_judged_by_whatever_is_in_force() {
        let active = parse(row()).unwrap();
        assert_eq!(version_to_fetch(&json!({"metadata": {}}), &active), None);
    }
}

// ---------------------------------------------------------------------------
// DB-backed test — the pin that makes this car a no-op by construction.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod db_tests {
    use super::*;
    use boss_jobs::delivery::{DeliveryPolicyRepository, PgDeliveryPolicy};

    /// THE CLAIM OF THE WHOLE CAR: moving the numbers into a registry
    /// changed no behaviour. The seeded row, read through the same port
    /// the jobs API serves it from and parsed by the same parser the
    /// conductor uses, must equal the constants `train.rs` held before —
    /// version aside, which the fallback cannot have.
    ///
    /// This is the collapse CLAUDE.md §9a asks for. The numbers are
    /// written down twice by necessity (a migration cannot read Rust and
    /// a Rust fallback cannot read SQL), so equality is the mechanism
    /// rather than a comment asking the next person to keep them in
    /// step.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_seeded_policy_equals_the_compiled_fallback() {
        let db = boss_testing::TestDb::new().await;
        let repo = PgDeliveryPolicy::new(db.pool.clone());
        let row = repo
            .active_policy(POLICY_NAME)
            .await
            .unwrap()
            .expect("the seed migration leaves one active policy");
        let seeded = parse(row).expect("the seeded row parses");

        let compiled = DeliveryPolicy::compiled();
        assert_eq!(
            DeliveryPolicy {
                version: NO_VERSION,
                ..seeded.clone()
            },
            compiled,
            "the seeded delivery policy drifted from the conductor's compiled \
             fallback — one of them is now changing behaviour the other does \
             not, and which one runs depends on whether the registry answered"
        );
        assert!(
            seeded.is_from_registry(),
            "the seed is a real version, so a train departing under it pins one"
        );
    }
}
