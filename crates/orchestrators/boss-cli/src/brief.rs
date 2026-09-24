//! `boss brief <packet>` — the handover a builder is dispatched with:
//! the packet verbatim from the system of record, and the invariants
//! derived from the files that decide them.
//!
//! THE CASE (backlog cc9ddc5d, measured on this pipeline 2026-09-11).
//! About ten builder briefs were written in one session. Every one
//! restated the same invariants from memory — the gate's check list, the
//! uid to verify under, fixture paths, the push-before-check order, the
//! base check, the forbidden database, and `CARGO_BUILD_JOBS` — and
//! every one retyped them. That is CLAUDE.md §9a at its most expensive:
//! not a fact living twice but a fact living ten times, in prose nothing
//! can check.
//!
//! It had already cost. The uid invariant was WRONG IN ALL TEN: "verify
//! in a workspace owned by uid 65534" is not achievable on this pod,
//! because `CapEff` is 0xc0 — CAP_SETUID and CAP_SETGID, no CAP_CHOWN —
//! so `chown 65534` fails on /tmp, /scratch and /work alike. Three
//! builders resolved that three different ways in one day, because the
//! instruction named a requirement and not a method.
//!
//! And a second, separate damage: briefs SUMMARISED PACKETS from memory
//! instead of pointing at them. Two were materially wrong about the
//! packet's own defect class, and one was built around a code shape that
//! has zero instances on main.
//!
//! ## What this verb does about each
//!
//! Two halves, one for each measured failure.
//!
//! **The packet half** reads the packet live and prints every metadata
//! key VERBATIM AND UNTRUNCATED. This is the one thing it does that
//! `boss job get` deliberately does not: that verb FITS values to the
//! terminal, which is right for a list and wrong here, because a
//! truncated claim is exactly what sends a brief-writer back to memory
//! for the rest of the sentence. The contract is different, so the
//! renderer is separate — not a second copy of a shared one.
//!
//! **The invariant half** derives every statement from a file in the
//! tree and PRINTS THE FILE NEXT TO IT. Nothing here is a sentence
//! somebody typed about the gate: the uid and gid are read out of the
//! gate-runner manifest, the cargo bound out of the one env file that
//! holds it, the forbidden database out of `TestDb`'s own default, the
//! phase list out of `infra/gate.sh`'s `check` call sites, and the
//! pre-flight roster by ASKING `infra/gate.sh --roster` rather than
//! re-deriving what it already derives. A derivation cannot drift from
//! its authority; a paragraph can, and did, ten times.
//!
//! The method for the uid — the fact that was wrong every time — is not
//! described at all. It is a command: `infra/dev/as-gate-uid.sh`, which
//! has the target uid CREATE the workspace (the only route that works
//! without CAP_CHOWN) and prints what it did, so a receipt's `verified`
//! field can quote it instead of claiming it.
//!
//! **The rules half** (design c87fb59b car 2, backlog 39d0b528) prints
//! the profile's document from `infra/platform/documents/` after the
//! invariants — the same tree the invariants are derived from, read the
//! same way (`documents.rs`). Which profile is the step's own
//! `agent_profile`, projected from its Workflow row's `agent` block;
//! `builder` when it declares none. `boss dispatch` prints the same
//! rendering as its prompt, through the one `render`.
//!
//! Read-only, and it writes nothing. The packet read goes through
//! `gate::api`, so it inherits the no-default `BOSS_JOBS_URL` rule and
//! is signed as the caller.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The lanes an invariant can be filed under, and a profile's document
/// declares which one it is briefed in (`lane:` in its front-matter,
/// `boss_cli::documents::lane_of`).
///
/// `car` is the lane that ships one: base, gate, push. `step` is the
/// lane whose deliverable is the step itself — it opens no worktree and
/// enters no gate, so the gate's phase list, uid, fixture paths and
/// probe-time rule are forty lines it cannot act on (backlog c8faa7f3,
/// measured on analyst run d5e0f287).
pub(crate) const LANE_CAR: &str = "car";
pub(crate) const LANE_STEP: &str = "step";
pub(crate) const LANES: [&str; 2] = [LANE_CAR, LANE_STEP];

/// How an invariant's lines RELATE to the file named beside them
/// (backlog c94ddc6f, 2026-09-19).
///
/// Until this, the only check on `authority` was that the named path
/// EXISTS; nothing asked whether the file had anything to do with the
/// text next to it. The base-check invariant carried a hand-typed
/// `git diff` command under a label naming `freshness.rs` — a file
/// that refuses a stale base but runs no such command — and that
/// derived-looking label misled TWO readers in ONE day into reporting
/// the defect's location as that file: the builder of 8d054cb2, and
/// the operator afterwards in a triage evidence field marked
/// `verified`.
///
/// Prose wearing derived clothes is the hole cc9ddc5d closed one level
/// up. Most invariants here really are read out of their authority, so
/// the reader cannot tell the one that is not — unless the rendering
/// says so, which is what this decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Grounding {
    /// The values this invariant SUBSTITUTED into its lines, each
    /// naming the file it was read out of. Every one is pinned to be
    /// present both in that file and in the lines printed, so the
    /// label cannot outlive the reading it claims.
    Derived(Vec<Reading>),
    /// The authority ENFORCES the rule; the sentences are ours. Marked
    /// in the rendered output with `WRITTEN_MARK`, because a written
    /// claim wearing a derived label is worse than one wearing none —
    /// a reader trusts it.
    Written,
}

/// What a `Written` invariant prints beside its authority. The file is
/// still named — it is the right file to go read — but the reader is
/// told the sentences did not come out of it.
pub(crate) const WRITTEN_MARK: &str = "— written here, not read from that file";

/// ONE SUBSTITUTED VALUE and the file it was read out of (backlog
/// d334116c, 2026-09-20).
///
/// An invariant names ONE authority, and until this every value it
/// printed rode under that one name. `verify as the gate` printed
/// `uid 65534 / gid 1500` beside `infra/dev/as-gate-uid.sh`: the
/// script contains `65534` four times and `1500` zero times, because
/// BOTH numbers are read at runtime out of the gate-runner manifest
/// (the script's own `ids()` awk pass over `$MANIFEST`). So under a
/// plain derived-looking label one number was checkable against the
/// named file and the other was not, and nothing said which.
///
/// That is the cc9ddc5d trap one layer in — the reader stops trusting
/// memory and trusts the label instead, and the label was right about
/// half of it. A value now carries its own source, the pin checks each
/// value against THAT file, and a source that is not the invariant's
/// authority is printed, so a mixed invariant reads as two
/// attributions rather than one misleading one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reading {
    /// The exact text substituted into the lines a reader sees.
    pub(crate) value: String,
    /// Repo-relative path of the file it came out of. Checked to
    /// exist and to contain `value`.
    pub(crate) from: String,
}

impl Reading {
    /// A value read out of `from`. Both halves are stated at the call
    /// site: the point of this type is that neither is inferred.
    pub(crate) fn read(from: &str, value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            from: from.to_string(),
        }
    }
}

/// How a reading whose source is NOT its invariant's authority is
/// printed, under the invariant's lines: the values, then their file.
pub(crate) fn foreign_source_line(values: &[String], from: &str) -> String {
    format!("({} read from {from})", values.join(", "))
}

/// One invariant a brief can reference instead of restating, and the
/// file in the tree that DECIDES it.
///
/// `authority` is repo-relative and is checked to exist: an invariant
/// whose authority is missing is not a weaker invariant, it is a
/// sentence, and this verb exists to stop printing those.
///
/// `lanes` is which readers it is TRUE FOR. An invariant printed to a
/// profile that cannot act on it is not free: it crowds out the rules
/// that profile does act on, and it teaches the reader to skim the
/// section (c8faa7f3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Invariant {
    pub(crate) name: &'static str,
    pub(crate) authority: String,
    pub(crate) lines: Vec<String>,
    pub(crate) lanes: Vec<&'static str>,
    pub(crate) grounding: Grounding,
}

/// The gate container's uid and gid, read from the gate-runner manifest.
///
/// Scoped to the container named `gate`: the postgres sidecar above it
/// runs as 999/999, and a scan that took the first `runAsUser` in the
/// file would confidently answer with the database's uid. Stops at the
/// next container so a third one added later cannot be read either.
pub(crate) fn gate_ids(manifest: &str) -> Option<(u32, u32)> {
    let mut uid = None;
    let mut gid = None;
    let mut inside = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with("- name:") {
            if inside {
                break;
            }
            inside = t == "- name: gate";
            continue;
        }
        if !inside {
            continue;
        }
        if let Some(v) = t.strip_prefix("runAsUser:") {
            uid = v.trim().parse().ok();
        } else if let Some(v) = t.strip_prefix("runAsGroup:") {
            gid = v.trim().parse().ok();
        }
    }
    Some((uid?, gid?))
}

/// The cargo job bound, read from `infra/dev/pod-build.env` — the one
/// file that holds it, and the file a builder sources rather than
/// retyping the number.
pub(crate) fn cargo_jobs(env_file: &str) -> Option<String> {
    env_file
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("CARGO_BUILD_JOBS="))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The manifest that DECLARES the dev pod's cgroup — the pod every
/// builder works in, and the ceiling a bare `cargo` drives to.
pub(crate) const DEV_MANIFEST: &str = "infra/cluster/manifests/boss-dev.yaml";

/// The resource limits the dev container declares, verbatim — the
/// cgroup a builder is actually inside (backlog 28fc3a39, 2026-09-22).
///
/// Returned as the manifest's own line rather than parsed into numbers
/// and reassembled: the figure printed to a builder is then the same
/// string the declaration holds, so `Reading` can pin it against the
/// file and neither unit nor spelling can drift between them. The two
/// typed copies it replaces said 16 GiB where the manifest declares
/// 32Gi, and one of them said 8 CPU where it declares 16 — the pod was
/// resized for two builders and an operator (5ee0ff2a) and both copies
/// stayed at the old size.
///
/// Scoped to the container named `dev`, for the same reason `gate_ids`
/// is scoped: the postgres and reclaim sidecars declare limits of their
/// own, and a scan that took the first (or the last) would confidently
/// answer with a database's 4Gi.
pub(crate) fn pod_cgroup_limits(manifest: &str) -> Option<String> {
    let mut inside = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with("- name:") {
            if inside {
                break;
            }
            inside = t == "- name: dev";
            continue;
        }
        if inside && t.starts_with("limits:") {
            return Some(t.to_string());
        }
    }
    None
}

/// The admin URL `TestDb` connects to by default — i.e. the one a test
/// reaches when nothing overrides it, which is the address a
/// `kubectl port-forward` turns into the production cluster database.
///
/// Read from the constant rather than stated, so the prohibition names
/// whatever `TestDb` actually binds today.
pub(crate) fn test_db_admin_url(test_db_rs: &str) -> Option<String> {
    test_db_rs
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("const DEFAULT_ADMIN_URL"))
        .and_then(|l| l.split('"').nth(1))
        .map(str::to_string)
}

/// The phases a full gate runs, in the order their `check` calls appear
/// in `infra/gate.sh`, deduplicated.
///
/// The call sites ARE the authority — `check "<name>"` is how a phase
/// comes to exist — so a phase added tomorrow turns up here with no edit.
/// One name is skipped: the gate's own stdin self-test installs a
/// fixture through the real `check` and then removes it from the receipt
/// ledgers, so it is not a phase a car pays for. A fixture that names
/// itself a self-test is the only thing filtered, and the test below
/// pins both halves — the real phases present, the fixture absent.
pub(crate) fn gate_phases(gate_sh: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in gate_sh.lines() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("check \"") else {
            continue;
        };
        let Some(name) = rest.split('"').next() else {
            continue;
        };
        if name.contains("self-test") || name.is_empty() {
            continue;
        }
        if !out.iter().any(|p| p == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// How many lints the pre-flight roster runs, by ASKING the gate.
///
/// `infra/gate.sh --roster` already derives the roster (the lint
/// directory minus a named exclusion set) and boss-testing's `gate_sh`
/// pin asks it the same way. Re-deriving it in Rust would be a second
/// copy of the rule §9a forbids, so this shells out: one `ls` and an
/// awk pass, well under a second.
fn preflight_lint_count(repo: &Path) -> Result<usize> {
    let out = std::process::Command::new("bash")
        .arg("infra/gate.sh")
        .arg("--roster")
        .current_dir(repo)
        .output()
        .context("could not run infra/gate.sh --roster")?;
    if !out.status.success() {
        bail!(
            "infra/gate.sh --roster exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count())
}

/// The system of record's address, read from the ONE tree file that
/// spells it (`infra/estate/estate.toml`, backlog 5222163e).
///
/// Parsed rather than retyped for the same reason as every other
/// authority here, and for one more: the lint
/// `the-estate-address-lives-once` refuses that literal anywhere but
/// the source, so a brief that stated the address could not exist.
pub(crate) fn sor_url(estate_toml: &str) -> Option<String> {
    estate_toml
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("sor_url"))
        .and_then(|l| l.split('"').nth(1))
        .map(str::to_string)
        .filter(|v| !v.is_empty())
}

/// The pre-flight door, read out of CLAUDE.md §Doors (backlog
/// 5d919334, 2026-09-22).
///
/// §Doors is where a session looks for the command to run before a
/// push, and its `Before pushing` entry is the one place in this tree
/// that DECIDES which mode that is. The builder document restated it
/// instead, as `--quick`, and CLAUDE.md had already moved to `--lint`
/// — the same pre-flight plus a scoped clippy — after `--quick` alone
/// cost two gates to clippy errors (410e21e2). So every brief rendered
/// on 2026-09-22 handed a builder the weaker door; two builders noticed
/// the disagreement within one hour, judged the tree correct, and ran
/// the stronger one anyway. A builder who did not notice would pay for
/// it at the gate.
///
/// Patching the literal would leave the same copy one word later, so
/// the door is parsed here and QUOTED by the document — CLAUDE.md 9a's
/// collapse rather than its holding action.
pub(crate) fn preflight_door(claude_md: &str) -> Option<String> {
    claude_md
        .split("- **Before pushing")
        .nth(1)?
        .split("\n\n- **")
        .next()?
        .split('`')
        .nth(1)
        .map(str::to_string)
        .filter(|v| !v.is_empty())
}

fn read(repo: &Path, rel: &str) -> Result<String> {
    std::fs::read_to_string(repo.join(rel)).with_context(|| format!("reading {rel}"))
}

/// Every invariant, derived. The authorities are read here and nowhere
/// else, so there is exactly one place in this repo that turns them into
/// sentences.
pub(crate) fn invariants(repo: &Path) -> Result<Vec<Invariant>> {
    let manifest = "infra/gate-runner/gate-runner.yaml";
    let env_file = "infra/dev/pod-build.env";
    let method = "infra/dev/as-gate-uid.sh";
    let test_db = "crates/core/boss-testing/src/test_db.rs";
    let fixture_lint = "infra/lint/a-fixture-path-cannot-be-a-literal.sh";
    let scratch = "crates/core/boss-testing/src/scratch.rs";
    let gate = "infra/gate.sh";
    let freshness = "crates/orchestrators/boss-cli/src/freshness.rs";
    let probe_rules = "crates/core/boss-jobs/src/probe.rs";

    let (uid, gid) = gate_ids(&read(repo, manifest)?)
        .with_context(|| format!("{manifest} does not name the gate container's uid and gid"))?;
    let jobs = cargo_jobs(&read(repo, env_file)?)
        .with_context(|| format!("{env_file} does not set CARGO_BUILD_JOBS"))?;
    // THE BOUND'S OWN REASON, read from the declaration (28fc3a39).
    // What a bare `cargo` would drive to its ceiling was typed here,
    // and in the builder rules, and both had stayed at the pod's old
    // size — half the memory in both, half the CPU in one.
    let cgroup = pod_cgroup_limits(&read(repo, DEV_MANIFEST)?)
        .with_context(|| format!("{DEV_MANIFEST} does not declare the dev container's limits"))?;
    let admin_url = test_db_admin_url(&read(repo, test_db)?)
        .with_context(|| format!("{test_db} does not declare DEFAULT_ADMIN_URL"))?;
    let phases = gate_phases(&read(repo, gate)?);
    if phases.is_empty() {
        bail!("{gate} has no `check \"…\"` call sites — the phase list cannot be derived");
    }
    let lints = preflight_lint_count(repo)?;

    let mut out = vec![
        Invariant {
            name: "cargo jobs",
            authority: env_file.to_string(),
            lines: vec![
                // ONE PLAIN COMMAND (65cea113): the `set -a; . file`
                // spelling this line carried is refused by a
                // worktree-isolated builder's harness, and wt-cargo
                // already sources the file itself.
                format!(
                    "wt-cargo <cargo args>     # reads CARGO_BUILD_JOBS={jobs} from {env_file}"
                ),
                "Nothing else bounds cargo here (there is no .cargo/config.toml), so a bare".into(),
                "cargo's default is one job per CPU — 32 on this pod, because nproc reads the NODE"
                    .into(),
                format!("and not the cgroup, which the dev pod declares as {cgroup}."),
            ],
            lanes: vec![LANE_CAR],
            grounding: Grounding::Derived(vec![
                Reading::read(env_file, format!("CARGO_BUILD_JOBS={jobs}")),
                Reading::read(DEV_MANIFEST, cgroup.clone()),
            ]),
        },
        Invariant {
            name: "verify as the gate",
            authority: method.to_string(),
            lines: vec![
                format!("{method} <your command>"),
                format!(
                    "The gate runs as uid {uid} / gid {gid}, so a check that reads git or file"
                ),
                "ownership answers differently there. This script is the METHOD, not a".into(),
                "requirement to re-derive: there is no CAP_CHOWN on this pod, so it has".into(),
                format!("uid {uid} CREATE the workspace (bundle -> fetch) and prints what it did."),
                "Quote its last line in a receipt's `verified` field.".into(),
            ],
            lanes: vec![LANE_CAR],
            // The METHOD is this script; the two NUMBERS are the
            // manifest's, which is where the script's own awk pass
            // reads them from. Attributing them to the script would
            // pin them to its prose — `65534` appears there four
            // times, all in comments, and `1500` not at all
            // (d334116c).
            grounding: Grounding::Derived(vec![
                Reading::read(manifest, uid.to_string()),
                Reading::read(manifest, gid.to_string()),
            ]),
        },
        Invariant {
            name: "gate uid / gid",
            authority: manifest.to_string(),
            lines: vec![format!(
                "uid {uid}, gid {gid} — read from the `gate` container, not from prose"
            )],
            lanes: vec![LANE_CAR],
            grounding: Grounding::Derived(vec![
                Reading::read(manifest, uid.to_string()),
                Reading::read(manifest, gid.to_string()),
            ]),
        },
        Invariant {
            name: "fixture paths",
            authority: fixture_lint.to_string(),
            lines: vec![
                format!("boss_testing::scratch (see {scratch}) — pid AND uid in the path"),
                "A fixed /tmp path is shared with every account on this long-lived pod;".into(),
                "the lint named above is in the pre-flight roster and refuses one.".into(),
            ],
            lanes: vec![LANE_CAR],
            grounding: Grounding::Derived(vec![Reading::read(
                fixture_lint,
                "boss_testing::scratch",
            )]),
        },
        Invariant {
            name: "the database",
            authority: test_db.to_string(),
            lines: vec![
                format!("TestDb's default admin URL is {admin_url}"),
                "Never point a test at that host through a port-forward: the forward".into(),
                "makes it the PRODUCTION cluster database, and it answers instead of".into(),
                "erroring.".into(),
            ],
            lanes: vec![LANE_CAR],
            grounding: Grounding::Derived(vec![Reading::read(test_db, admin_url.clone())]),
        },
        Invariant {
            name: "order",
            authority: gate.to_string(),
            lines: vec![
                "failing test -> watch it fail -> fix -> cargo fmt -> COMMIT AND PUSH".into(),
                "-> then any optional local check. The gate is the compile authority;".into(),
                "a cold local build that is never pushed parks a car with nothing on it.".into(),
            ],
            lanes: vec![LANE_CAR],
            // WRITTEN: the gate runs these checks, it does not spell
            // this order for a builder's afternoon (c94ddc6f).
            grounding: Grounding::Written,
        },
        // Three dots on the diff, and not as a matter of taste: two-dot
        // diffs the two TIPS, so it is right only while the line above
        // it has already exited 0 — origin/main an ancestor of HEAD is
        // exactly where the two forms agree. That made the pair
        // ORDER-dependent with nothing holding the order, and the
        // failing guard is the case a builder actually reads: one
        // commit behind, car 6e738252 saw 47 files / 5235 deletions
        // from the tips and 7 files / 16 from the merge-base. Three-dot
        // is correct whether or not the guard ran (9843aeb9,
        // 2026-09-19; 8d054cb2 made the same correction in the rules).
        Invariant {
            name: "base check",
            authority: freshness.to_string(),
            lines: vec![
                "git merge-base --is-ancestor origin/main HEAD     # must exit 0".into(),
                "git diff --numstat origin/main...HEAD             # only your files".into(),
                "A branch on an old base merges clean and reverts landed work.".into(),
            ],
            lanes: vec![LANE_CAR],
            // WRITTEN, and this is the invariant that bought the
            // distinction: `freshness.rs` REFUSES a branch whose base
            // is behind main — which is why it is the right file to
            // name — but it runs neither of these two commands, and
            // the derived-looking label sent two readers to it for a
            // defect that was here (c94ddc6f).
            grounding: Grounding::Written,
        },
        Invariant {
            name: "gate phases",
            authority: gate.to_string(),
            lines: vec![
                format!("{} pre-flight lints, then: {}", lints, phases.join(", ")),
                "Derived from the gate's own `check` call sites and `--roster`, so this".into(),
                "list cannot fall behind the gate that judges the car.".into(),
            ],
            lanes: vec![LANE_CAR],
            grounding: Grounding::Derived(phases.iter().map(|p| Reading::read(gate, p)).collect()),
        },
        // The tokens come from the constant the refusal reads, not from
        // prose: a brief that named a token the gate does not refuse is
        // the drift §9a is about (c0ac92b8).
        Invariant {
            name: "probe time",
            authority: probe_rules.to_string(),
            lines: vec![
                format!(
                    "boss_jobs::probe::reads_git_time_with_an_offset refuses {} in a --park-probe",
                    boss_jobs::probe::GIT_TIME_WITH_AN_OFFSET.join(" / ")
                ),
                "A -07:00 committer date compared as a STRING against the SoR's UTC".into(),
                "timestamps answered FAILED for a not-yet (car 746a1fac). Compare epochs:".into(),
                format!(
                    "${} on one side (this car's own merge, handed to the probe — NOT",
                    boss_jobs::probe::CAR_CONVERGED_AT_VAR
                ),
                "a HEAD that moves with every train, a92571a6; git log -1 --format=%ct only".into(),
                "for a NAMED ref), date -u -d \"$ts\" +%s on the other, -gt between them —".into(),
                "and guard the empty case FIRST, because date -d ''".into(),
                "answers midnight rather than an error.".into(),
            ],
            lanes: vec![LANE_CAR],
            grounding: Grounding::Derived(
                boss_jobs::probe::GIT_TIME_WITH_AN_OFFSET
                    .iter()
                    .map(|t| t.to_string())
                    .chain([
                        boss_jobs::probe::CAR_CONVERGED_AT_VAR.to_string(),
                        "reads_git_time_with_an_offset".to_string(),
                    ])
                    .map(|v| Reading::read(probe_rules, v))
                    .collect(),
            ),
        },
    ];
    // THE STEP LANE'S OWN INVARIANT (c8faa7f3). A profile that ships no
    // car still has one fact it must not get from memory: WHICH
    // INSTANCE it is reading. A wrong or dark target answers `total: 0`
    // rather than erroring (CLAUDE.md §Doors), which is the analyst
    // failure mode the rules document calls a confident empty answer —
    // so the address is derived here from the file that spells it, and
    // the control read is named beside it.
    let estate = "infra/estate/estate.toml";
    let sor = sor_url(&read(repo, estate)?)
        .with_context(|| format!("{estate} does not spell sor_url"))?;
    out.push(Invariant {
        name: "the system of record",
        authority: estate.to_string(),
        lines: vec![
            format!("BOSS_JOBS_URL={sor} — the one spelling, read from this file"),
            "`boss-api METHOD /api/path` pins it and signs as the actor running it.".into(),
            "A wrong or dark instance answers total: 0 instead of erroring, and so does".into(),
            "a denied policy scope: before reporting that something does not exist, run".into(),
            "a control read on the same connection whose answer you already know, and".into(),
            "say beside the finding what the control returned.".into(),
        ],
        lanes: vec![LANE_STEP],
        grounding: Grounding::Derived(vec![Reading::read(estate, sor.clone())]),
    });

    // THE PRE-FLIGHT DOOR (backlog 5d919334, 2026-09-22). CLAUDE.md
    // §Doors is where a session looks for the command to run before a
    // push, and it is the one file that decides which mode that is.
    // The builder rules restated it as `--quick` and §Doors had moved
    // to `--lint`; the copy is read here instead, so the rules can
    // quote it.
    let doors = "CLAUDE.md";
    let door = preflight_door(&read(repo, doors)?)
        .with_context(|| format!("{doors} §Doors does not carry a `Before pushing` door"))?;
    out.push(Invariant {
        name: "pre-flight",
        authority: doors.to_string(),
        lines: vec![
            // ONE PLAIN COMMAND (65cea113): a trailing `; echo $?`
            // made the line a list, which a worktree-isolated builder's
            // harness refuses to run; the exit code it echoed is the
            // command's own, and the harness reports that.
            format!("bash {door} > <your scratch dir>/preflight.log 2>&1"),
            "Its exit status is the verdict — the tool reports it; in a shell, run".into(),
            "echo $? as the NEXT command, never chained to this one on the same line.".into(),
            "The `Before pushing` door of CLAUDE.md §Doors, read out of that entry:".into(),
            "the whole build-free pre-flight PLUS clippy scoped to the crates this tree".into(),
            "changed, seconds against the ~11 minutes a gate costs. It is not a gate —".into(),
            "the build and the suites stay unproven — and it is judged by its EXIT CODE,".into(),
            "never by its last line: a pipe through tail succeeds while the run fails,".into(),
            "and an && chain behind one pushed a red car (2026-09-20).".into(),
        ],
        lanes: vec![LANE_CAR],
        grounding: Grounding::Derived(vec![Reading::read(doors, door.clone())]),
    });

    out.sort_by_key(|i| i.name);

    // EVERY authority must exist. An invariant whose file is gone is a
    // sentence, and a sentence is what this verb replaces.
    for inv in &out {
        let p = repo.join(&inv.authority);
        if !p.is_file() {
            bail!(
                "the {:?} invariant names {} as its authority and that file does not exist",
                inv.name,
                inv.authority
            );
        }
        // And every SOURCE a substituted value names, for the same
        // reason: a value attributed to a file that is gone is a
        // sentence wearing a citation (d334116c).
        if let Grounding::Derived(readings) = &inv.grounding {
            for r in readings {
                if !repo.join(&r.from).is_file() {
                    bail!(
                        "the {:?} invariant says it read {:?} out of {} and that file is not there",
                        inv.name,
                        r.value,
                        r.from
                    );
                }
            }
        }
    }
    Ok(out)
}

/// The readings of `inv` that came out of some file OTHER than its
/// authority, grouped by that file, first-appearance order kept so two
/// renders of one tree read the same.
pub(crate) fn foreign_sources(inv: &Invariant) -> Vec<(String, Vec<String>)> {
    let Grounding::Derived(readings) = &inv.grounding else {
        return Vec::new();
    };
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for r in readings.iter().filter(|r| r.from != inv.authority) {
        match out.iter_mut().find(|(f, _)| f == &r.from) {
            Some((_, values)) => {
                if !values.contains(&r.value) {
                    values.push(r.value.clone());
                }
            }
            None => out.push((r.from.clone(), vec![r.value.clone()])),
        }
    }
    out
}

/// The invariant half, rendered FOR ONE LANE: the invariants that lane
/// can act on, and no line from the other (c8faa7f3).
///
/// A lane with no invariant in this tree gets a line saying so rather
/// than an empty header — silence reads as "nothing to know here",
/// which is a claim this function is in no position to make.
pub(crate) fn invariant_section(invs: &[Invariant], lane: &str) -> String {
    let mine: Vec<&Invariant> = invs.iter().filter(|i| i.lanes.contains(&lane)).collect();
    let mut out = format!(
        "== THE INVARIANTS — for the `{lane}` lane, each read out of the file named \
         after it (a value read elsewhere names its own file), or marked as written ==\n"
    );
    if mine.is_empty() {
        out.push_str(&format!(
            "\nNo invariant in this tree is filed under the `{lane}` lane.\n"
        ));
        return out;
    }
    for inv in mine {
        // A written invariant says so beside its authority: the file
        // is still the one to go read, and the reader is told the
        // sentences did not come out of it (c94ddc6f).
        let mark = match inv.grounding {
            Grounding::Written => format!(" {WRITTEN_MARK}"),
            Grounding::Derived(_) => String::new(),
        };
        out.push_str(&format!("\n{}   [{}{mark}]\n", inv.name, inv.authority));
        for l in block(inv) {
            out.push_str(&format!("    {l}\n"));
        }
    }
    out
}

/// What a reader sees of `inv` under its header, wherever it is read:
/// its lines, then one attribution per file OTHER than its authority
/// that a substituted value came out of, grouped by that file and in
/// the order the values were read. One authority standing for every
/// value is what let the gate's gid ride under a file that does not
/// contain it (d334116c).
///
/// Both renderers read this — the invariants section and a rules
/// document's `{{invariant:<name>}}` quote — because until 2026-09-22
/// the quote printed only the lines, and builder rule 2 handed the dev
/// pod's cgroup to a reader with no file to check it against, on the
/// day the same figure was found stale in four places (a7469d74). One
/// function, so the value and its source cannot part by which of the
/// two a reader reads.
pub(crate) fn block(inv: &Invariant) -> Vec<String> {
    inv.lines
        .iter()
        .cloned()
        .chain(
            foreign_sources(inv)
                .into_iter()
                .map(|(from, values)| foreign_source_line(&values, &from)),
        )
        .collect()
}

/// The packet half: the envelope, the step it is at, EVERY metadata key
/// verbatim, and every key of every completed step's metadata likewise.
///
/// Untruncated on purpose. `boss job get` fits values to the terminal,
/// which is right for scanning a list; here a clipped claim is the
/// defect — it is what sent two briefs to memory for the rest of the
/// sentence and got the packet's own class wrong. Keys are sorted so two
/// reads of one packet print in the same order.
pub(crate) fn packet_section(job: &Value) -> String {
    let g = |k: &str| job.get(k).and_then(Value::as_str).unwrap_or("-");
    let mut out = format!(
        "== THE PACKET — verbatim from the system of record, not summarised ==\n\n\
         {}\n{}   kind {}   status {}   priority {}   opened {}\n",
        crate::envelope::job_title(job).unwrap_or("-"),
        crate::envelope::job_id(job).unwrap_or("-"),
        g("kind"),
        g("status"),
        g("priority"),
        g("opened_on"),
    );
    match now_step(job) {
        // THE STEP IS NAMED THE WAY THE SURFACE NAMES IT (c8faa7f3).
        // This line used to print the `spec_slug` alone — "now at:
        // measure (ready)" — and there is no step TITLED measure: the
        // title is "Inventory the page and the department's needs",
        // and the slug is a key no page shows. A reader who cannot
        // find the thing the brief named learns to mistrust the brief,
        // so both are printed, the title first, with the handle beside
        // it.
        Some(step) => {
            let l = crate::envelope::step_line(step);
            out.push_str(&format!("now at: {}\n", l.title));
            out.push_str(&format!(
                "        step `{}`, kind {}, status {}, {}{}\n",
                l.slug,
                l.kind,
                l.status,
                match &l.assignee {
                    Some(a) => format!("held by {a}"),
                    None => "unassigned".to_string(),
                },
                match &l.authority_role {
                    Some(r) => format!(", authority role {r}"),
                    None => String::new(),
                },
            ));
        }
        None => out.push_str("now at: no ready or active step\n"),
    }

    let md = sorted_metadata(job);
    out.push_str(&format!("\nmetadata ({} key(s)), in full:\n", md.len()));
    out.push_str(&key_blocks(&md));

    // EVERY COMPLETED STEP'S METADATA, IN FULL (backlog 3cdad35a,
    // measured by builder run df220512 on 3bc896be, 2026-09-23). The
    // record of what a packet has already decided lives on its steps,
    // not in its own metadata: triage's `evidence` and `disposition`,
    // the `design_id` of the approved design that answers it. That
    // brief showed an item as an open question while its steps carried
    // the design approved two days earlier, and every dispatch since
    // has carried an operator note telling the builder to GET the
    // steps by hand. Completed steps only — a skipped step recorded no
    // work, and the step the packet is AT is THE STEP section's — and
    // every key, uncurated, for the reason the job's own are.
    for step in crate::envelope::steps(job)
        .into_iter()
        .filter(|s| s.get("status").and_then(Value::as_str) == Some("completed"))
    {
        let l = crate::envelope::step_line(step);
        let s = |k: &str| step.get(k).and_then(Value::as_str);
        let md = sorted_metadata(step);
        out.push_str(&format!(
            "\ncompleted step `{}` — {}, kind {}{}{}, metadata ({} key(s)), in full:\n",
            l.slug,
            l.title,
            l.kind,
            s("completed_by")
                .map(|b| format!(", by {b}"))
                .unwrap_or_default(),
            s("completed_at")
                .map(|t| format!(" at {t}"))
                .unwrap_or_default(),
            md.len(),
        ));
        // Each correction the server handed this step, printed under
        // the field it corrects (design 4105b020): the original stays
        // in full, and the correction arrives beside it rather than in
        // a job-metadata key the reader would have to think to open.
        let corrections: Vec<&Value> = step
            .get("corrections")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        out.push_str(&key_blocks_corrected(&md, &corrections));
    }
    out
}

/// [`key_blocks`], with each key followed by the corrections that name
/// it as their `field`.
fn key_blocks_corrected(md: &BTreeMap<String, Value>, corrections: &[&Value]) -> String {
    md.iter()
        .map(|(k, v)| key_blocks([(k, v)]) + &correction_lines(k, corrections))
        .collect()
}

/// The lines a reader of `field` owes the corrections of it, in the
/// list's order. A correction later withdrawn says by which entry; a
/// withdrawal names the entry it withdraws. `reads` / `should read` /
/// `why` keep their own lines, because the prose here is exactly the
/// kind that was damaged once already.
fn correction_lines(field: &str, corrections: &[&Value]) -> String {
    let s = |c: &Value, k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let index = |c: &Value| c.get("index").and_then(Value::as_u64);
    let mut out = String::new();
    for c in corrections
        .iter()
        .filter(|c| c.get("field").and_then(Value::as_str) == Some(field))
    {
        let i = index(c)
            .map(|i| i.to_string())
            .unwrap_or_else(|| "?".into());
        let signed = format!("by {} at {}", s(c, "by"), s(c, "at"));
        if let Some(target) = c.get("withdraws").and_then(Value::as_u64) {
            out.push_str(&format!(
                "    ↳ correction [{i}] withdraws [{target}], {signed}: {}\n",
                s(c, "why")
            ));
            continue;
        }
        let withdrawn_by = corrections
            .iter()
            .find(|w| {
                index(c).is_some_and(|i| w.get("withdraws").and_then(Value::as_u64) == Some(i))
            })
            .and_then(|w| index(w))
            .map(|w| format!(" (withdrawn by [{w}])"))
            .unwrap_or_default();
        out.push_str(&format!(
            "    ↳ correction [{i}]{withdrawn_by}, {signed}:\n"
        ));
        for (label, key) in [
            ("reads:       ", "reads"),
            ("should read: ", "should_read"),
            ("why:         ", "why"),
        ] {
            let text = s(c, key);
            if key == "why" && text.is_empty() {
                continue;
            }
            let mut lines = text.lines();
            out.push_str(&format!("        {label} {}\n", lines.next().unwrap_or("")));
            for more in lines {
                out.push_str(&format!(
                    "        {:width$} {more}\n",
                    "",
                    width = label.len()
                ));
            }
        }
    }
    out
}

/// A row's `metadata`, keys sorted so two reads print in one order.
fn sorted_metadata(row: &Value) -> BTreeMap<String, Value> {
    row.get("metadata")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

/// Each key and its value verbatim — a string as its own lines, any
/// other value as JSON — indented under the key. The one "in full"
/// shape every metadata block in the brief prints.
fn key_blocks<'a>(md: impl IntoIterator<Item = (&'a String, &'a Value)>) -> String {
    let mut out = String::new();
    for (k, v) in md {
        let rendered = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out.push_str(&format!("\n  {k}:\n"));
        for line in rendered.lines() {
            out.push_str(&format!("    {line}\n"));
        }
        if rendered.is_empty() {
            out.push_str("    (empty)\n");
        }
    }
    out
}

/// The step the packet is AT — ready or active — if it has one.
pub(crate) fn now_step(job: &Value) -> Option<&Value> {
    crate::envelope::steps(job)
        .into_iter()
        .find(|s| crate::envelope::step_line(s).now)
}

/// The keys of a step's metadata that are NOT its specification: the
/// agent block (the run section already states model, budget and
/// effort) and the two the `now at` line already printed.
fn not_the_spec(key: &str) -> bool {
    key.starts_with("agent_") || key == "authority_role" || key == "audience"
}

/// THE STEP'S OWN SPECIFICATION, VERBATIM — the half a brief withheld
/// (backlog c8faa7f3, measured on analyst run d5e0f287, 2026-09-19).
///
/// The packet half prints JOB metadata. The thing an executor works
/// FROM lives on the STEP: its `procedure` (1915 characters on the
/// page-audit `measure` step) and its `fields`, which are what
/// required-at-done will refuse a completion for. Neither was printed,
/// so the brief's own premise — that it saves the reader a fetch of
/// the packet — was false for every profile whose deliverable is the
/// step.
///
/// Rendered whenever the step HAS a specification, not when a profile
/// is guessed to want one: a `build` step that carries only its agent
/// block has nothing to show and prints nothing, which is why a
/// builder's brief is unchanged by this.
pub(crate) fn step_section(job: &Value) -> Option<String> {
    let step = now_step(job)?;
    let l = crate::envelope::step_line(step);
    let fields: Vec<&Value> = step
        .get("fields")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let spec: BTreeMap<String, Value> = step
        .get("metadata")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter(|(k, _)| !not_the_spec(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    if fields.is_empty() && spec.is_empty() {
        return None;
    }

    let mut out = format!(
        "== THE STEP — its own specification, verbatim from the step ==\n\n{} — step `{}`, \
         kind {}, status {}\n",
        l.title, l.slug, l.kind, l.status
    );
    if !fields.is_empty() {
        out.push_str("\nrequired at done — the step's own `fields`:\n");
        for f in fields {
            let g = |k: &str| f.get(k).and_then(Value::as_str).unwrap_or("?");
            let required = match f.get("required").and_then(Value::as_bool) {
                Some(true) => "REQUIRED",
                _ => "optional",
            };
            out.push_str(&format!(
                "  {}   {}   {}   filled by {}\n",
                g("name"),
                g("field_type"),
                required,
                g("filled_by"),
            ));
        }
    }
    out.push_str(&key_blocks(&spec));
    Some(out)
}

/// The stored `procedure` on the step the packet is AT, with the slug
/// it belongs to — the text [`step_section`] renders as the executor's
/// specification.
fn stored_procedure(job: &Value) -> Option<(String, String)> {
    let step = now_step(job)?;
    let text = step
        .get("metadata")?
        .get(PROCEDURE_KEY)?
        .as_str()?
        .to_string();
    Some((crate::envelope::step_line(step).slug, text))
}

/// The `procedure` the CURRENT Workflow row authors for that step —
/// `steps[].metadata_defaults`, which is the place a step's procedure
/// is copied FROM at materialisation.
fn authored_procedure(row: &Value, slug: &str) -> Option<String> {
    row.get("steps")?
        .as_array()?
        .iter()
        .find(|s| s.get("title").and_then(Value::as_str) == Some(slug))?
        .get("metadata_defaults")?
        .get(PROCEDURE_KEY)?
        .as_str()
        .map(str::to_string)
}

/// The one key this section dates. Spelled once here rather than at
/// each of the three reads.
const PROCEDURE_KEY: &str = "procedure";

/// WHICH VERSION OF THE SPECIFICATION BELOW THIS PACKET IS RUNNING
/// (backlog 794e8d61).
///
/// A step's `procedure` is copied out of the Workflow row's
/// `metadata_defaults` at MATERIALISATION, so a packet carries the
/// prose that was current when it was OPENED, permanently: publishing
/// v3 changes what future packets materialise and nothing else. The
/// brief renders that stored text as the executor's specification and
/// said nothing about its age, so the lag could not be seen, measured,
/// or decided about — 47 page-audit packets were open on v2 when this
/// was filed, and roughly 94 completions were due to run against
/// superseded instructions.
///
/// THE VERSION PAIR ALONE OVER-REPORTS, so the verdict is the
/// comparison: a packet two versions behind whose own step was never
/// edited is running current prose. `active` is the registry's active
/// row (`GET /api/workflows/{kind}`), read best-effort by the caller —
/// what is NOT known is said rather than assumed, because a brief that
/// silently claims currency is this defect wearing a different face.
///
/// It reports a TEXT difference, not an edit: a `{token}` in the
/// authored default expands at materialisation, so the two strings can
/// differ without the protocol having moved. Naming the two versions
/// is what makes the difference checkable by hand.
pub(crate) fn protocol_section(job: &Value, active: Option<&Value>) -> Option<String> {
    let pinned = job.get("workflow_version").and_then(Value::as_i64)?;
    let kind = job.get("kind").and_then(Value::as_str).unwrap_or("-");
    let mut out = String::from(
        "== THE PROTOCOL — which version of the specification below this packet is running \
         ==\n\n",
    );
    let current = active
        .and_then(|r| r.get("version"))
        .and_then(Value::as_i64);
    let Some(current) = current else {
        out.push_str(&format!(
            "{kind} v{pinned}, pinned at admission. The registry's current version was not \
             read, so whether the specification below is still the published one is \
             unknown.\n"
        ));
        return Some(out);
    };
    if current <= pinned {
        out.push_str(&format!(
            "{kind} v{pinned}, pinned at admission, and v{current} is the registry's active \
             version — the specification below is current.\n"
        ));
        return Some(out);
    }
    let behind = current - pinned;
    out.push_str(&format!(
        "{kind} v{pinned}, pinned at admission. The registry's active version is v{current}, \
         so this packet is {behind} {} behind it — in-flight packets stay on the version \
         they were admitted under.\n",
        if behind == 1 { "version" } else { "versions" }
    ));
    if let Some((slug, stored)) = stored_procedure(job) {
        match active.and_then(|r| authored_procedure(r, &slug)) {
            Some(authored) if authored == stored => out.push_str(&format!(
                "The `{slug}` procedure below is the v{pinned} text and v{current}'s text for \
                 that step is identical — the published versions did not touch it.\n"
            )),
            Some(_) => out.push_str(&format!(
                "The `{slug}` procedure below is the v{pinned} text and v{current}'s text for \
                 that step is DIFFERENT, so an edit published since this packet opened did \
                 NOT reach it: a procedure is copied out of the Workflow row at \
                 materialisation and never re-read. Read v{current}'s at \
                 /api/workflows/{kind} before completing this step.\n"
            )),
            None => out.push_str(&format!(
                "v{current} authors no `{PROCEDURE_KEY}` for step `{slug}`, so the text below \
                 has no current counterpart to be compared against.\n"
            )),
        }
    }
    Some(out)
}

/// The line that tells the brief-writer what to do with this output: a
/// brief REFERENCES the invariants and lets the builder read the packet,
/// rather than carrying a retyped copy of either.
pub(crate) const HOW_TO_USE: &str = "\
A brief built on this output is three paragraphs of packet-specific reasoning
plus one reference: `boss brief <packet>` — and nothing retyped. Restating
an invariant here in a brief is how the uid fact came to be wrong ten times
(backlog cc9ddc5d).";

/// The repo whose files the invariants are derived from: the worktree
/// the caller is standing in. Asked of git rather than guessed, so a
/// call from a subdirectory or a worktree lands on the right root.
pub(crate) fn repo_root() -> Result<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("could not run git rev-parse --show-toplevel")?;
    if !out.status.success() {
        bail!(
            "not inside a git worktree, so the invariants have no tree to be derived from: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if root.is_empty() {
        bail!("git rev-parse --show-toplevel answered nothing");
    }
    Ok(PathBuf::from(root))
}

/// The profile a packet's current step is briefed under, read through
/// the ONE reader `boss dispatch` uses ([`crate::dispatch::settings_for`]):
/// the step's own projection, else the block the active Workflow row
/// declares for it, else [`crate::documents::DEFAULT_PROFILE`]. A
/// packet with no open step — or no packet at all, or no row read —
/// is briefed as the default, because the rules document is what the
/// reader came for and a blank brief helps nobody.
///
/// Measured 2026-09-19 on page-audit c0d2caf0: the live `measure` step
/// carries `procedure`, `audience` and `authority_role` but NO agent
/// block — the projection is not on these packets — so reading only the
/// step answered `builder` for a step the Workflow row declares
/// `analyst`, and the brief a human read before dispatching was the
/// other lane's. The row is best effort at the call site: an extra
/// call that cannot be made still leaves a brief worth printing.
pub(crate) fn profile_for(job: Option<&Value>, row: Option<&Value>) -> String {
    profile_on_step(job, row).unwrap_or_else(|| crate::documents::DEFAULT_PROFILE.to_string())
}

/// The profile declared for the packet's current step, by either half
/// of the one reader, if anything declares one.
pub(crate) fn profile_on_step(job: Option<&Value>, row: Option<&Value>) -> Option<String> {
    crate::dispatch::settings_for(job.and_then(now_step)?, row).map(|s| s.profile)
}

/// The registry's ACTIVE Workflow row for this packet's kind, read
/// best-effort. Two readers now — the lane fallback above and
/// [`protocol_section`] — so it is ONE call, not two (794e8d61).
pub(crate) async fn active_row(http: &reqwest::Client, job: &Value) -> Option<Value> {
    let kind = job.get("kind").and_then(Value::as_str)?;
    crate::gate::api(
        http,
        reqwest::Method::GET,
        &format!("/api/workflows/{kind}"),
        None,
    )
    .await
    .ok()?
}

/// The whole brief, rendered FOR A PROFILE: the packet (when there is
/// one), the step's own specification (when it has one), the
/// invariants OF THAT PROFILE'S LANE, the line that says how to use
/// them, and the rules document. One function so `boss brief` and the
/// prompt `boss dispatch` prints cannot drift apart (CLAUDE.md 9a).
///
/// The profile decides ONE thing here — the lane — and it decides it
/// through the document bundle, so what a profile is briefed with and
/// what it is told are edited in one place (c8faa7f3). There is no
/// second renderer: a lane that files no invariant of its own simply
/// prints none.
pub(crate) fn render(
    repo: &Path,
    job: Option<&Value>,
    profile: &str,
    active: Option<&Value>,
    // The dispatching session's own commit trailer, when it supplied
    // one (`documents::TRAILER_ENV`, backlog 89d1572c) — read at the
    // verb's edge, so this renderer stays a function of its arguments.
    trailer: Option<&str>,
) -> Result<String> {
    let invs = invariants(repo)?;
    let lane = crate::documents::lane(repo, profile)?;
    let mut out = String::new();
    if let Some(job) = job {
        out.push_str(&packet_section(job));
        out.push('\n');
        // ABOVE the specification it dates (794e8d61): a reader meets
        // the age of the prose before the prose.
        if let Some(protocol) = protocol_section(job, active) {
            out.push_str(&protocol);
            out.push('\n');
        }
        if let Some(step) = step_section(job) {
            out.push_str(&step);
            out.push('\n');
        }
    }
    out.push_str(&invariant_section(&invs, &lane));
    out.push_str(&format!("\n{HOW_TO_USE}\n\n"));
    out.push_str(&crate::documents::section(repo, profile, &invs, trailer)?);
    Ok(out)
}

pub async fn run(packet_ref: Option<String>, profile_override: Option<String>) -> Result<()> {
    let repo = repo_root()?;
    let http = reqwest::Client::new();
    let job = match packet_ref {
        Some(r) => {
            let id = crate::job::fetch_and_resolve(&http, &r).await?;
            Some(
                crate::gate::api(
                    &http,
                    reqwest::Method::GET,
                    &format!("/api/jobs/{id}"),
                    None,
                )
                .await?
                .context("the packet read returned no body")?,
            )
        }
        None => None,
    };
    // WHAT A HAND-RUN BRIEF RENDERS AS (c8faa7f3). `boss brief
    // <packet>` is also read by a human before dispatching, with no
    // profile implied. It renders as THE DISPATCH WOULD: the step's own
    // projection when it carries one, else the Workflow row's block for
    // that step — through `dispatch::settings_for`, which is the SAME
    // FUNCTION the dispatch resolves with rather than a second copy of
    // its order (dacee8cc) — so the human reads what the agent will
    // read. With no packet, or nothing declaring a profile anywhere,
    // it is `builder`, exactly as before profiles existed.
    // `--profile` names a lane without dispatching anything.
    let active = match job.as_ref() {
        Some(j) => active_row(&http, j).await,
        None => None,
    };
    let profile = profile_override.unwrap_or_else(|| profile_for(job.as_ref(), active.as_ref()));
    print!(
        "{}",
        render(
            &repo,
            job.as_ref(),
            &profile,
            active.as_ref(),
            crate::documents::supplied_trailer().as_deref(),
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The repo this test's own crate lives in — three levels up from
    /// `crates/orchestrators/boss-cli`.
    fn repo() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("the workspace root is above this crate")
    }

    #[test]
    fn the_gate_uid_comes_from_the_runner_manifest_not_from_a_literal() {
        let live = std::fs::read_to_string(repo().join("infra/gate-runner/gate-runner.yaml"))
            .expect("the gate-runner manifest");
        let (uid, gid) = gate_ids(&live).expect("the gate container names its uid and gid");
        assert_eq!(
            (uid, gid),
            (65534, 1500),
            "the manifest moved the gate's uid/gid; that is fine — this assertion \
             exists so the MOVE is visible, and the brief will already be printing \
             the new numbers because it reads them here"
        );
        // The anti-hardcode half: a different manifest must produce
        // different numbers. A derivation that ignores its input is a
        // literal wearing a function's clothes.
        let moved = live
            .replace("runAsUser: 65534", "runAsUser: 4242")
            .replace("runAsGroup: 1500", "runAsGroup: 4343");
        assert_eq!(gate_ids(&moved), Some((4242, 4343)));
    }

    #[test]
    fn the_postgres_sidecars_uid_is_not_mistaken_for_the_gates() {
        // The sidecar is declared FIRST in the real manifest, so a scan
        // that took the first runAsUser would answer 999 — a confident
        // wrong number, which is the failure class this car is about.
        let manifest = "\
      containers:
        - name: postgres
          securityContext:
            runAsUser: 999
            runAsGroup: 999
        - name: gate
          securityContext:
            runAsUser: 65534
            runAsGroup: 1500
";
        assert_eq!(gate_ids(manifest), Some((65534, 1500)));
    }

    #[test]
    fn a_manifest_with_no_gate_container_yields_nothing_rather_than_a_guess() {
        let manifest = "        - name: postgres\n            runAsUser: 999\n";
        assert_eq!(gate_ids(manifest), None);
    }

    #[test]
    fn the_cargo_bound_is_read_from_the_one_file_that_holds_it() {
        let live = std::fs::read_to_string(repo().join("infra/dev/pod-build.env"))
            .expect("the pod build env file");
        assert_eq!(cargo_jobs(&live).as_deref(), Some("6"));
        assert_eq!(cargo_jobs("CARGO_BUILD_JOBS=11\n").as_deref(), Some("11"));
        assert_eq!(cargo_jobs("# nothing set here\n"), None);
    }

    /// THE CGROUP A BUILDER ACTS ON IS READ OUT OF THE MANIFEST THAT
    /// DECLARES IT (backlog 28fc3a39, 2026-09-22).
    ///
    /// The cargo bound's own reason — what a bare `cargo` would drive
    /// to its ceiling — was a typed figure in both places it appeared:
    /// this invariant said "a 16 GiB cgroup" and the builder rules said
    /// "16 GiB / 8-CPU", while `infra/cluster/manifests/boss-dev.yaml`
    /// had declared `{cpu: "16", memory: 32Gi}` since the pod was
    /// resized for two builders and an operator (5ee0ff2a). Half the
    /// memory in both copies, half the CPU in one. A rule whose stated
    /// reason is false teaches the reader to discount the rule, so the
    /// figure is read from the declaration rather than corrected into
    /// a third drift.
    #[test]
    fn the_cargo_bound_cites_the_cgroup_the_dev_pod_declares() {
        let live = std::fs::read_to_string(repo().join(DEV_MANIFEST)).expect("the dev manifest");
        let limits = pod_cgroup_limits(&live).expect("the dev container declares its limits");
        assert_eq!(
            limits, "limits: {cpu: \"16\", memory: 32Gi}",
            "the dev pod was resized; that is fine — this assertion exists so the \
             MOVE is visible, and the brief will already be printing the new figure \
             because it reads it here"
        );
        // The anti-hardcode half: a different manifest must produce a
        // different figure, or the derivation is a literal wearing a
        // function's clothes.
        let moved = live.replace(&limits, "limits: {cpu: \"64\", memory: 128Gi}");
        assert_eq!(
            pod_cgroup_limits(&moved).as_deref(),
            Some("limits: {cpu: \"64\", memory: 128Gi}")
        );

        let invs = invariants(&repo()).expect("the invariants derive from this tree");
        let inv = invs
            .iter()
            .find(|i| i.name == "cargo jobs")
            .expect("the cargo bound is an invariant of this tree");
        assert!(
            inv.lines.join("\n").contains(&limits),
            "the cargo bound states a cgroup the manifest does not declare: {:?}",
            inv.lines
        );
        let Grounding::Derived(readings) = &inv.grounding else {
            panic!("the cargo bound is derived, not written");
        };
        assert!(
            readings
                .iter()
                .any(|r| r.from == DEV_MANIFEST && r.value == limits),
            "the cgroup figure rides under the bound's own authority instead of \
             naming the manifest it was read from"
        );
    }

    #[test]
    fn a_sidecars_limits_are_not_mistaken_for_the_dev_containers() {
        // Declared AFTER the sidecar here, because in the real manifest
        // it comes first — a scan that took the first `limits:` would
        // pass against the tree and answer with postgres's 4Gi the day
        // someone reorders it.
        let manifest = "\
      containers:
        - name: postgres
          resources:
            limits: {cpu: \"2\", memory: 4Gi}
        - name: dev
          resources:
            requests: {cpu: \"4\", memory: 8Gi}
            limits: {cpu: \"16\", memory: 32Gi}
        - name: reclaim
          resources:
            limits: {cpu: 500m, memory: 256Mi}
";
        assert_eq!(
            pod_cgroup_limits(manifest).as_deref(),
            Some("limits: {cpu: \"16\", memory: 32Gi}")
        );
        // No dev container is nothing, never the next container's
        // numbers: a confident wrong figure is the failure class here.
        assert_eq!(
            pod_cgroup_limits("        - name: postgres\n            limits: {cpu: \"2\"}\n"),
            None
        );
        // A dev container that declares no limits is unbounded, and
        // saying so is not this function's business either.
        assert_eq!(
            pod_cgroup_limits(
                "        - name: dev\n          image: x\n        - name: pg\n            limits: {cpu: \"2\"}\n"
            ),
            None
        );
    }

    #[test]
    fn the_database_prohibition_names_what_test_db_actually_binds() {
        let live = std::fs::read_to_string(repo().join("crates/core/boss-testing/src/test_db.rs"))
            .expect("test_db.rs");
        let url = test_db_admin_url(&live).expect("TestDb declares a default admin URL");
        assert!(
            url.contains("127.0.0.1"),
            "TestDb's default admin URL is {url:?}; the brief prints whatever this is, \
             and the prohibition is about the host a port-forward hijacks"
        );
        let invs = invariants(&repo()).expect("the invariants derive");
        let db = invs
            .iter()
            .find(|i| i.name == "the database")
            .expect("a database invariant");
        assert!(
            db.lines.iter().any(|l| l.contains(&url)),
            "the rendered invariant must carry the URL read from test_db.rs, not a \
             retyped one: {:?}",
            db.lines
        );
    }

    #[test]
    fn the_phase_list_is_the_gates_own_check_calls_and_excludes_its_self_test() {
        let live = std::fs::read_to_string(repo().join("infra/gate.sh")).expect("infra/gate.sh");
        let phases = gate_phases(&live);
        for want in ["fmt", "clippy", "test", "fixture", "svelte-check"] {
            assert!(
                phases.iter().any(|p| p == want),
                "the gate runs {want:?} but the derived phase list is {phases:?}"
            );
        }
        assert!(
            !phases.iter().any(|p| p.contains("self-test")),
            "the gate's stdin self-test installs a fixture through the real `check` and \
             removes it from the receipt; it is not a phase a car pays for: {phases:?}"
        );
        // Dedupe: `clippy` has three call sites (lint scope, full,
        // scoped) and must be named once.
        assert_eq!(phases.iter().filter(|p| *p == "clippy").count(), 1);
    }

    #[test]
    fn every_invariant_names_an_authority_that_exists() {
        let invs = invariants(&repo()).expect("the invariants derive from this tree");
        assert!(invs.len() >= 8, "expected the full set, got {}", invs.len());
        for inv in &invs {
            assert!(
                repo().join(&inv.authority).is_file(),
                "{:?} names {} and that file does not exist",
                inv.name,
                inv.authority
            );
            assert!(!inv.lines.is_empty(), "{:?} says nothing", inv.name);
        }
    }

    /// THE AUTHORITY FIELD IS CHECKED TO DECIDE THE LINES (backlog
    /// c94ddc6f, 2026-09-19). Until this, the only check on `authority`
    /// was that the named path EXISTS — nothing asked whether the file
    /// had anything to do with the text beside it. The measured cost:
    /// the base-check invariant carried a hand-typed `git diff` command
    /// under a label naming `freshness.rs`, a file that runs no such
    /// command, and the derived-looking label misled TWO readers in ONE
    /// day into reporting the defect's location as that file — the
    /// builder of 8d054cb2, and the operator afterwards in this
    /// packet's own triage evidence, marked `verified`.
    ///
    /// So an invariant now declares which of the two it is, and this
    /// test proves the claim: every string a `Derived` invariant says
    /// it read is still present BOTH in the authority file and in the
    /// lines a reader sees. Prose wearing derived clothes is the hole
    /// cc9ddc5d closed one level up (ten briefs retyped from memory,
    /// the uid wrong in all ten); an unchecked label re-opens it.
    #[test]
    fn every_derived_invariant_can_show_its_reading_in_the_file_it_names() {
        let invs = invariants(&repo()).expect("the invariants derive from this tree");
        let mut derived = 0;
        for inv in &invs {
            let Grounding::Derived(readings) = &inv.grounding else {
                continue;
            };
            derived += 1;
            assert!(
                !readings.is_empty(),
                "{:?} claims a reading of nothing",
                inv.name
            );
            let lines = inv.lines.join("\n");
            for r in readings {
                assert!(
                    !r.value.trim().is_empty(),
                    "{:?} claims an empty reading",
                    inv.name
                );
                // ITS OWN source, not the invariant's authority
                // (d334116c): one authority standing for every value
                // is how the gid came to ride under a file that does
                // not contain it.
                let source = std::fs::read_to_string(repo().join(&r.from))
                    .unwrap_or_else(|_| panic!("{} reads", r.from));
                assert!(
                    source.contains(r.value.as_str()),
                    "{:?} says it read `{}` out of {}, and that file does not contain it",
                    inv.name,
                    r.value,
                    r.from
                );
                assert!(
                    lines.contains(r.value.as_str()),
                    "{:?} read `{}` and then does not print it",
                    inv.name,
                    r.value
                );
            }
        }
        assert!(
            derived >= 5,
            "most invariants are read, not written: {derived}"
        );
    }

    /// EVERY SUBSTITUTED VALUE NAMES THE FILE IT CAME FROM, and a value
    /// that came from somewhere other than the invariant's authority
    /// says so where a builder reads it (backlog d334116c, 2026-09-20).
    ///
    /// Measured before the fix: `verify as the gate` printed `uid 65534
    /// / gid 1500` under `infra/dev/as-gate-uid.sh`, a file containing
    /// `65534` four times (all of them prose) and `1500` zero times.
    /// Both numbers are in fact the gate-runner manifest's — the script
    /// awks them out of it at runtime — so the invariant's one
    /// authority was right about the METHOD and wrong about where
    /// either number was read. The pin covered the uid alone, so the
    /// gid rode unchecked under a derived label.
    ///
    /// This is cc9ddc5d one layer in: the whole point of `boss brief`
    /// is that nobody retypes these numbers, and a reader who trusts
    /// the label instead of their memory has to be able to trust all of
    /// it.
    #[test]
    fn the_two_gate_ids_are_attributed_to_the_manifest_that_decides_them() {
        let invs = invariants(&repo()).expect("the invariants derive");
        let manifest = "infra/gate-runner/gate-runner.yaml";
        let (uid, gid) =
            gate_ids(&std::fs::read_to_string(repo().join(manifest)).expect("the manifest"))
                .expect("the gate container's ids");
        let inv = invs
            .iter()
            .find(|i| i.name == "verify as the gate")
            .expect("a verify-as-the-gate invariant");
        let Grounding::Derived(readings) = &inv.grounding else {
            panic!("verify as the gate prints two read values");
        };
        for n in [uid.to_string(), gid.to_string()] {
            let r = readings
                .iter()
                .find(|r| r.value == n)
                .unwrap_or_else(|| panic!("the invariant prints {n} and pins {readings:?}"));
            assert_eq!(
                r.from, manifest,
                "{n} is read from the manifest, not from {}",
                r.from
            );
        }
        // The METHOD is still the authority — it is the file to go read
        // — and the rendered block now names the manifest beside the
        // two numbers it actually holds.
        assert_eq!(inv.authority, "infra/dev/as-gate-uid.sh");
        let rendered = invariant_section(&invs, LANE_CAR);
        let expected = foreign_source_line(&[uid.to_string(), gid.to_string()], manifest);
        assert!(
            rendered.contains(&expected),
            "the rendered invariant does not say where the ids came from: {rendered}"
        );
        // And an invariant whose values all come from its authority
        // stays plain: the extra line has to DISCRIMINATE, or it is
        // decoration.
        let db = invs
            .iter()
            .find(|i| i.name == "the database")
            .expect("a database invariant");
        assert!(
            foreign_sources(db).is_empty(),
            "the database invariant reads only its own authority"
        );
    }

    /// The other half of the same rule: an invariant whose lines are
    /// WRITTEN says so where the authority is printed, so a reader can
    /// tell which claims carry a file behind them. `base check` is the
    /// measured one — `freshness.rs` REFUSES a stale base, which is why
    /// it is the right file to name, but it does not spell the two
    /// commands a builder runs.
    #[test]
    fn an_invariant_whose_lines_are_written_is_not_dressed_as_a_reading() {
        let invs = invariants(&repo()).expect("the invariants derive");
        let rendered = invariant_section(&invs, LANE_CAR);
        let base = invs
            .iter()
            .find(|i| i.name == "base check")
            .expect("a base-check invariant");
        assert_eq!(base.grounding, Grounding::Written);
        assert!(
            rendered.contains(&format!("[{} {WRITTEN_MARK}]", base.authority)),
            "the written invariant is printed as if it were read: {rendered}"
        );
        // And a read one carries no such mark: the label has to
        // DISCRIMINATE, or it is the unchecked label again.
        let uid = invs
            .iter()
            .find(|i| i.name == "gate uid / gid")
            .expect("a uid invariant");
        assert!(matches!(uid.grounding, Grounding::Derived(_)));
        assert!(
            rendered.contains(&format!("[{}]", uid.authority)),
            "a derived invariant must print its authority plainly: {rendered}"
        );
    }

    #[test]
    fn the_method_is_an_executable_and_agrees_with_the_manifest_about_the_uid() {
        let script = repo().join("infra/dev/as-gate-uid.sh");
        assert!(script.is_file(), "the method script must exist");
        let text = std::fs::read_to_string(&script).expect("the method script");
        // CODE only. The script's prose says the word `chown` repeatedly,
        // because explaining why it cannot chown is half the point; what
        // must not exist is a CALL. (The first version of this assertion
        // scanned the whole file and failed on its own documentation —
        // a check that reds on the explanation of the fact it checks.)
        let calls_chown = text
            .lines()
            .map(|l| l.split('#').next().unwrap_or(""))
            .any(|code| code.contains("chown"));
        assert!(
            !calls_chown,
            "the method must not chown: there is no CAP_CHOWN on this pod, which is \
             precisely the fact ten briefs got wrong"
        );
        // THE PIN THAT CANNOT BE COLLAPSED: the script reads the ids in
        // awk and this module reads them in Rust. One file decides them,
        // but two readers parse it, so the readers are made to agree by
        // RUNNING the script rather than by a comment asking them to.
        let out = std::process::Command::new("bash")
            .arg("infra/dev/as-gate-uid.sh")
            .arg("--print-ids")
            .current_dir(repo())
            .output()
            .expect("the method script runs");
        assert!(
            out.status.success(),
            "as-gate-uid.sh --print-ids exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let printed = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let live = std::fs::read_to_string(repo().join("infra/gate-runner/gate-runner.yaml"))
            .expect("the manifest");
        let (uid, gid) = gate_ids(&live).expect("the ids");
        assert_eq!(
            printed,
            format!("{uid} {gid}"),
            "the script and this module read one manifest and must answer the same ids"
        );
    }

    #[test]
    fn the_packet_half_prints_metadata_in_full_and_does_not_curate_keys() {
        // Longer than any terminal. A fitted value is the defect: it is
        // what sent a brief-writer back to memory for the rest of the
        // sentence and got the packet's class wrong.
        let long = "m".repeat(400);
        let job = json!({
            "id": "cc9ddc5d-7e43-4a74-91f9-273b9ca2ba6a",
            "title": "Ten briefs restated the invariants",
            "kind": "backlog-item",
            "status": "open",
            "priority": "urgent",
            "opened_on": "2026-09-11",
            "metadata": {
                // Not `claim`/`evidence`/`proposed`: a curated key list
                // would silently drop a packet's real content, which is
                // the summarising failure with extra steps.
                "measured_2026_09_11": long.clone(),
                "do_not": "Do not solve this by making CLAUDE.md longer.",
            },
            "steps": [{
                "spec_slug": "triage",
                "kind": "task",
                "status": "ready",
                "title": "Measure the claim, choose a route",
            }],
        });
        let out = packet_section(&job);
        assert!(
            out.contains(&long),
            "a metadata value must not be truncated"
        );
        assert!(out.contains("measured_2026_09_11"));
        assert!(out.contains("Do not solve this by making CLAUDE.md longer."));
        assert!(out.contains("2 key(s)"));
        assert!(out.contains("triage"), "the step the packet is at: {out}");
        assert!(out.contains("cc9ddc5d-7e43-4a74-91f9-273b9ca2ba6a"));
    }

    /// THE DECISION LIVES ON THE STEPS, SO THE BRIEF PRINTS THEM
    /// (backlog 3cdad35a, measured by builder run df220512 on 3bc896be,
    /// 2026-09-23). That item's brief showed an open question; its
    /// steps carried the approved design that answered it, and the
    /// triage step's evidence and disposition — exactly what a builder
    /// needs — were on a step too. The shape below is the live
    /// backlog-item's: a completed trigger, a completed triage, a
    /// completed design step, a skipped one, and the active build.
    #[test]
    fn the_packet_half_prints_every_completed_steps_metadata_in_full() {
        let long = "e".repeat(400);
        let job = json!({
            "id": "3bc896be-0000-4000-8000-000000000000",
            "kind": "backlog-item",
            "metadata": { "area": "platform" },
            "steps": [
                { "spec_slug": "entered", "kind": "trigger", "status": "completed",
                  "title": "Item enters the backlog",
                  "metadata": { "trigger_name": "item-enters-the-backlog" } },
                { "spec_slug": "triage", "kind": "task", "status": "completed",
                  "title": "Measure the claim, choose a route",
                  "completed_by": "agent-claude",
                  "completed_at": "2026-09-23T14:53:42.043974Z",
                  "metadata": {
                      "disposition": "design",
                      "evidence": format!("{long}\nsecond line of evidence"),
                  } },
                { "spec_slug": "design", "kind": "task", "status": "completed",
                  "title": "File the design",
                  "metadata": { "design_id": "5877860d-aaaa-4bbb-8ccc-000000000000" } },
                { "spec_slug": "answer", "kind": "answer-question", "status": "skipped",
                  "title": "Answer the question",
                  "metadata": { "skipped_key": "not a record of work done" } },
                { "spec_slug": "build", "kind": "task", "status": "active",
                  "title": "Build the change",
                  "metadata": { "agent_run": "run-of-the-reader" } },
            ],
        });
        let out = packet_section(&job);
        // Every key of every completed step, untruncated, lines kept.
        assert!(out.contains(&long), "{out}");
        assert!(out.contains("    second line of evidence\n"), "{out}");
        assert!(out.contains("disposition:\n    design\n"), "{out}");
        assert!(
            out.contains("5877860d-aaaa-4bbb-8ccc-000000000000"),
            "{out}"
        );
        assert!(out.contains("item-enters-the-backlog"), "{out}");
        // Each block names its step the way a surface does, and who
        // completed it when the record says.
        assert!(out.contains("Measure the claim, choose a route"), "{out}");
        assert!(out.contains("step `triage`"), "{out}");
        assert!(out.contains("agent-claude"), "{out}");
        assert!(out.contains("2026-09-23T14:53:42.043974Z"), "{out}");
        // Only COMPLETED steps: a skipped step recorded no work, and the
        // step the packet is at has its own section.
        assert!(!out.contains("skipped_key"), "{out}");
        assert!(!out.contains("run-of-the-reader"), "{out}");
        // In the packet's own step order.
        let triage = out.find("step `triage`").expect("triage");
        let design = out.find("step `design`").expect("design");
        assert!(triage < design, "{out}");
    }

    /// A CORRECTION IS PRINTED UNDER THE FIELD IT CORRECTS (design
    /// 4105b020). The job GET hands each step its own `corrections`;
    /// the brief is a reader of step metadata like any other, so the
    /// damaged sentence and its correction arrive together — the
    /// original first, never replaced — and a withdrawn correction says
    /// so.
    #[test]
    fn a_completed_steps_corrections_print_under_the_field_they_correct() {
        let damaged = "Ordering trap confirmed:  is required of every rule";
        let job = json!({
            "id": "f3e091f0-0000-4000-8000-000000000000",
            "metadata": {},
            "steps": [
                { "spec_slug": "triage", "kind": "task", "status": "completed",
                  "title": "Measure the claim, choose a route",
                  "metadata": { "disposition": "build", "evidence": damaged, "zeta": "z" },
                  "corrections": [
                      { "index": 0, "step": "s", "field": "evidence",
                        "reads": "confirmed:  is", "should_read": "confirmed: `why` is",
                        "why": "the shell ate the word", "by": "agent-claude",
                        "at": "2026-09-19T19:10:00Z" },
                      { "index": 2, "step": "s", "field": "evidence",
                        "reads": "every rule", "should_read": "every rule file",
                        "why": "", "by": "agent-claude", "at": "2026-09-20T00:00:00Z" },
                      { "index": 3, "step": "s", "field": "evidence", "withdraws": 2,
                        "why": "the original was right", "by": "emp-david",
                        "at": "2026-09-21T00:00:00Z" },
                  ] },
            ],
        });
        let out = packet_section(&job);
        let original = out.find(damaged).expect("the original is printed");
        let first = out
            .find("correction [0]")
            .expect("the correction is printed");
        let next_key = out.find("  zeta:").expect("the next key");
        assert!(
            original < first && first < next_key,
            "under its field, before the next key: {out}"
        );
        assert!(out.contains("reads:        confirmed:  is\n"), "{out}");
        assert!(out.contains("should read:  confirmed: `why` is\n"), "{out}");
        assert!(
            out.contains("why:          the shell ate the word\n"),
            "{out}"
        );
        assert!(
            out.contains("by agent-claude at 2026-09-19T19:10:00Z"),
            "{out}"
        );
        assert!(out.contains("correction [2] (withdrawn by [3])"), "{out}");
        assert!(
            out.contains("correction [3] withdraws [2], by emp-david"),
            "{out}"
        );
        // Nothing is printed under a field nobody corrected.
        let disposition = out.find("  disposition:").expect("disposition");
        let evidence = out.find("  evidence:").expect("evidence");
        assert!(
            !out[disposition..evidence].contains("correction ["),
            "{out}"
        );
    }

    #[test]
    fn a_multiline_metadata_value_keeps_its_lines() {
        let job = json!({"metadata": {"claim": "first line\nsecond line"}});
        let out = packet_section(&job);
        assert!(out.contains("    first line\n    second line\n"), "{out}");
    }

    /// THE PROBE-TIME RULE IS SAID WHERE BUILDERS READ (c0ac92b8): the
    /// line names the refusing function and the tokens it refuses,
    /// taken from the constant itself rather than retyped, so the brief
    /// cannot say a token the gate does not refuse.
    #[test]
    fn the_probe_time_invariant_names_the_refusal_and_its_tokens_from_the_constant() {
        let invs = invariants(&repo()).expect("the invariants derive");
        let inv = invs
            .iter()
            .find(|i| i.name == "probe time")
            .expect("a probe-time invariant");
        assert_eq!(inv.authority, "crates/core/boss-jobs/src/probe.rs");
        let text = inv.lines.join("\n");
        assert!(text.contains("reads_git_time_with_an_offset"), "{text}");
        for token in boss_jobs::probe::GIT_TIME_WITH_AN_OFFSET {
            assert!(text.contains(token), "{token} missing from: {text}");
        }
        assert!(text.contains("--format=%ct"), "{text}");
        assert!(text.contains("date -u -d"), "{text}");
        assert!(text.contains("midnight"), "{text}");
        // And the cutoff the brief names is the FIXED one (a92571a6):
        // a builder who dates it from HEAD writes a car that starves.
        assert!(
            text.contains(boss_jobs::probe::CAR_CONVERGED_AT_VAR),
            "{text}"
        );
    }

    /// 9843aeb9 (2026-09-19): the base-check invariant prescribed the
    /// TWO-dot `git diff --numstat origin/main HEAD`, which diffs the
    /// two TIPS. That was correct only by ADJACENCY to the
    /// `--is-ancestor` line above it — the forms agree exactly when
    /// origin/main IS an ancestor of HEAD, and the builder is told to
    /// run the guard first. But the guard is the line that FAILS when
    /// main has moved, and a builder reading that failure runs the diff
    /// next: the one case where tip-to-tip reports every commit that
    /// landed on main while it worked as the branch's own deletions
    /// (car 6e738252, one commit behind: 47 files / 5235 deletions
    /// against the three-dot form's 7 files / 16). Three dots diff from
    /// the MERGE-BASE, so the line is correct whether or not the guard
    /// ran, and the ordering stops being load-bearing. Same argument
    /// 8d054cb2 made for the rules document; that car's pin walks the
    /// document's `--stat` occurrences, so this literal — Rust source,
    /// a different flag — needs its own.
    #[test]
    fn the_base_check_invariant_diffs_from_the_merge_base_not_the_tips() {
        let invs = invariants(&repo()).expect("the invariants derive");
        let inv = invs
            .iter()
            .find(|i| i.name == "base check")
            .expect("a base-check invariant");
        assert_eq!(
            inv.authority,
            "crates/orchestrators/boss-cli/src/freshness.rs"
        );
        let text = inv.lines.join("\n");
        assert!(
            text.contains("merge-base --is-ancestor origin/main HEAD"),
            "{text}"
        );
        let check = "git diff --numstat origin/main";
        assert!(text.contains(check), "{text}");
        for (at, _) in text.match_indices(check) {
            assert!(
                text[at + check.len()..].starts_with("...HEAD"),
                "a tip-to-tip diff survives in the base-check invariant at byte {at}: {text}"
            );
        }
    }

    #[test]
    fn the_rendered_invariants_name_their_authority_beside_each_statement() {
        let invs = invariants(&repo()).expect("the invariants derive");
        let rendered = invariant_section(&invs, LANE_CAR);
        for inv in invs.iter().filter(|i| i.lanes.contains(&LANE_CAR)) {
            assert!(
                rendered.contains(&inv.authority),
                "{:?} is printed without the file that decides it",
                inv.name
            );
        }
        assert!(rendered.contains("infra/dev/as-gate-uid.sh"));
        assert!(rendered.contains("pre-flight lints"));
    }

    /// The rules half: the document for the step's own profile follows
    /// the invariants, so a builder is briefed with the rules of the
    /// tree it stands in — and a packet declaring no profile is briefed
    /// as a builder rather than with nothing.
    #[test]
    fn the_brief_ends_with_the_rules_for_the_steps_profile() {
        let declared = json!({
            "id": "39d0b528-ff69-4cb8-ba82-408b641da66c",
            "kind": "backlog-item",
            "metadata": {},
            "steps": [
                { "spec_slug": "triage", "status": "completed", "metadata": {} },
                // The WHOLE projection, as car 1 writes it — four keys
                // together. It carried two until dacee8cc, which is
                // the shape dispatch has always read as no projection
                // at all.
                { "spec_slug": "build", "status": "ready",
                  "metadata": { "agent_profile": "analyst", "agent_model": "opus-5[1m]",
                                "agent_budget_usd": 4.0, "agent_effort": "high" } },
            ],
        });
        assert_eq!(profile_for(Some(&declared), None), "analyst");
        let undeclared = json!({
            "steps": [{ "spec_slug": "build", "status": "ready", "metadata": {} }],
        });
        assert_eq!(profile_for(Some(&undeclared), None), "builder");
        assert_eq!(profile_for(None, None), "builder");

        let out = render(&repo(), Some(&undeclared), "builder", None, None).expect("renders");
        let packet = out.find("== THE PACKET").expect("the packet half");
        let invariants = out.find("== THE INVARIANTS").expect("the invariant half");
        let rules = out.find("== THE RULES").expect("the rules half");
        assert!(
            packet < invariants && invariants < rules,
            "packet, invariants, rules"
        );
        assert!(out.contains("# Builder rules"));
        assert!(out.contains(HOW_TO_USE));
        // No packet: invariants and rules alone.
        let alone = render(&repo(), None, "builder", None, None).expect("renders");
        assert!(!alone.contains("== THE PACKET"));
        assert!(alone.contains("== THE RULES"));
    }

    /// ONE READER, ONE ANSWER (backlog dacee8cc). The step-then-row
    /// fallback was spelled twice — `boss dispatch` read all four
    /// projected keys or fell through to the row, `boss brief` read
    /// `agent_profile` alone — so half a projection made the two verbs
    /// disagree about which lane a step belongs to, and the brief a
    /// human read before dispatching was not the one the dispatch
    /// would render. Measured 2026-09-22: 195 live steps across
    /// page-audit, backlog-item and user-feedback carry no `agent_`
    /// key at all (the whole page march plus nine others), so this
    /// fallback decides the lane for most agent work on the instance.
    #[test]
    fn the_brief_and_the_dispatch_read_the_block_the_same_way() {
        let row = json!({
            "kind": "page-audit",
            "steps": [
                { "title": "measure", "kind": "task",
                  "agent": { "profile": "analyst", "model": "opus-5[1m]",
                             "budget_usd": 4, "effort": "high" } },
            ],
        });
        // HALF A PROJECTION IS NONE — dispatch's own rule since car 1,
        // because the four keys are written together. A step carrying
        // only `agent_profile` is therefore the row's lane, not that
        // key's.
        let half = json!({
            "kind": "page-audit",
            "steps": [{ "spec_slug": "measure", "status": "ready",
                        "metadata": { "agent_profile": "builder" } }],
        });
        let step = now_step(&half).expect("the open step");
        assert_eq!(
            crate::dispatch::settings_for(step, Some(&row)).map(|s| s.profile),
            Some("analyst".to_string()),
        );
        assert_eq!(profile_for(Some(&half), Some(&row)), "analyst");

        // A FULL projection wins over the row: the packet is pinned to
        // the version it was admitted under, and the projection is
        // that version's declaration.
        let projected = json!({
            "kind": "page-audit",
            "steps": [{ "spec_slug": "measure", "status": "ready",
                        "metadata": { "agent_profile": "builder",
                                      "agent_model": "opus-5[1m]",
                                      "agent_budget_usd": 5.0,
                                      "agent_effort": "high" } }],
        });
        assert_eq!(profile_for(Some(&projected), Some(&row)), "builder");

        // Neither half declares anything: the default lane, as before
        // profiles existed.
        let bare = json!({
            "kind": "page-audit",
            "steps": [{ "spec_slug": "measure", "status": "ready", "metadata": {} }],
        });
        assert_eq!(profile_for(Some(&bare), Some(&row)), "analyst");
        assert_eq!(profile_for(Some(&bare), None), "builder");
    }

    /// A page-audit `measure` step, in the shape the live one has
    /// (read from the system of record 2026-09-19, packet c0d2caf0).
    fn a_step_with_a_specification() -> Value {
        json!({
            "id": "c0d2caf0-bfc5-4163-b81e-97f9273a0404",
            "kind": "page-audit",
            "metadata": { "route": "/it/auth-admin", "department": "it" },
            "steps": [
                { "spec_slug": "opened", "status": "completed", "metadata": {} },
                {
                    "spec_slug": "measure",
                    "kind": "task",
                    "status": "active",
                    "title": "Inventory the page and the department's needs",
                    "assignee_id": "claude@algedonic.dev",
                    "fields": [
                        { "name": "controls_md", "field_type": "string",
                          "required": true, "filled_by": "executor" },
                        { "name": "needs_md", "field_type": "string",
                          "required": false, "filled_by": "executor" },
                    ],
                    "metadata": {
                        "agent_profile": "analyst",
                        "agent_budget_usd": 4.0,
                        "audience": { "role": "platform-admin" },
                        "authority_role": "platform-admin",
                        "human_only": false,
                        "procedure": "Read the PAGE and the DEPARTMENT, and write the \
                                      difference as a list.\nCount what you list.",
                    },
                },
            ],
        })
    }

    /// THE BRIEF CARRIES THE THING IT SENDS THE READER TO FOLLOW
    /// (c8faa7f3, measured on analyst run d5e0f287). The brief rendered
    /// JOB metadata and a one-line `now at`; the 1915-character
    /// `procedure` that IS the step's specification lives in STEP
    /// metadata, and so did the `fields` a completion is refused for.
    /// The whole premise of the verb is that it saves the reader a
    /// fetch of the packet, and it did not.
    #[test]
    fn the_step_half_carries_the_procedure_and_the_fields_required_at_done() {
        let job = a_step_with_a_specification();
        let out = step_section(&job).expect("a step with a procedure has a section");
        assert!(out.contains("Read the PAGE and the DEPARTMENT"), "{out}");
        assert!(out.contains("Count what you list."), "{out}");
        assert!(out.contains("controls_md"), "{out}");
        assert!(out.contains("REQUIRED"), "{out}");
        assert!(
            out.contains("needs_md") && out.contains("optional"),
            "{out}"
        );
        assert!(out.contains("human_only"), "{out}");
        // The agent block is the run's, not the specification's: the
        // run section states model, budget and effort already.
        assert!(!out.contains("agent_budget_usd"), "{out}");

        // A `build` step carrying only its agent block has no
        // specification to print, so a builder's brief is unchanged.
        let build = json!({
            "steps": [{
                "spec_slug": "build", "kind": "task", "status": "active",
                "title": "Build the change", "fields": [],
                "metadata": { "agent_profile": "builder", "agent_effort": "high",
                              "authority_role": "platform-admin" },
            }],
        });
        assert_eq!(step_section(&build), None);
        let rendered = render(&repo(), Some(&build), "builder", None, None).expect("renders");
        assert!(!rendered.contains("== THE STEP"), "{rendered}");
    }

    /// THE ONE LINE IT DID RENDER WAS WRONG TWICE (c8faa7f3). It read
    /// `now at: measure (ready)`: `measure` is the `spec_slug`, a key
    /// no surface shows — the title is "Inventory the page and the
    /// department's needs" — and the status was already `active`,
    /// because the claim door had moved it. The status half is fixed
    /// at the other end (dispatch renders after the claim, from a
    /// re-read); this is the naming half.
    #[test]
    fn the_now_at_line_names_the_step_the_way_a_surface_names_it() {
        let out = packet_section(&a_step_with_a_specification());
        assert!(
            out.contains("now at: Inventory the page and the department's needs"),
            "{out}"
        );
        assert!(out.contains("step `measure`"), "{out}");
        assert!(out.contains("status active"), "{out}");
        assert!(out.contains("held by claude@algedonic.dev"), "{out}");
        assert!(
            !out.contains("now at: measure"),
            "the slug alone is the line that taught a reader to mistrust the brief: {out}"
        );
    }

    /// AN INVARIANT IS PRINTED TO A LANE THAT CAN ACT ON IT, AND TO NO
    /// OTHER (c8faa7f3). The analyst dispatch got the gate's phase
    /// list, its uid and gid, fixture paths, the base check and the
    /// probe-time rule — about forty lines for a profile that ships no
    /// car, enters no gate and pushes nothing — crowding out the ones
    /// it could use.
    #[test]
    fn a_lane_is_briefed_with_its_own_invariants_and_none_of_the_others() {
        let job = a_step_with_a_specification();
        let analyst = render(&repo(), Some(&job), "analyst", None, None).expect("renders");
        let builder = render(&repo(), Some(&job), "builder", None, None).expect("renders");

        // The car lane's invariants are absent from the step lane...
        for car_only in [
            "gate uid / gid",
            "pre-flight lints",
            "infra/dev/as-gate-uid.sh",
            "--park-probe",
            "CARGO_BUILD_JOBS",
        ] {
            assert!(
                !analyst.contains(car_only),
                "the analyst brief carries the car lane's `{car_only}`"
            );
            assert!(
                builder.contains(car_only),
                "the builder brief lost `{car_only}` — it must still see what it saw"
            );
        }
        // ...and the step lane's is absent from the car lane.
        assert!(analyst.contains("the system of record"), "{analyst}");
        assert!(analyst.contains("control read"), "{analyst}");
        assert!(!builder.contains("== THE INVARIANTS — for the `step` lane"));
        assert!(analyst.contains("# Analyst rules"), "{analyst}");
        assert!(builder.contains("# Builder rules"));
        // Both lanes get the step's own specification: it is the
        // packet's content, not a profile's preference.
        assert!(analyst.contains("== THE STEP"), "{analyst}");
        assert!(builder.contains("== THE STEP"));

        // The section says which lane it is, so a reader knows what it
        // is NOT being told.
        assert!(
            analyst.contains("== THE INVARIANTS — for the `step` lane"),
            "{analyst}"
        );
        assert!(builder.contains("== THE INVARIANTS — for the `car` lane"));
    }

    /// Every invariant is filed under at least one lane a document can
    /// declare: one filed under a lane no profile reads is printed to
    /// nobody, which is the same defect as printing it to everybody.
    #[test]
    fn every_invariant_is_filed_under_a_lane_a_document_can_declare() {
        let invs = invariants(&repo()).expect("the invariants derive");
        for inv in &invs {
            assert!(!inv.lanes.is_empty(), "{:?} names no lane", inv.name);
            for lane in &inv.lanes {
                assert!(
                    LANES.contains(lane),
                    "{:?} is filed under `{lane}`, which is not a lane: {LANES:?}",
                    inv.name
                );
            }
        }
        for lane in LANES {
            assert!(
                invs.iter().any(|i| i.lanes.contains(&lane)),
                "no invariant is filed under the `{lane}` lane, so a profile briefed \
                 in it reads a section that says nothing"
            );
        }
    }

    /// The step lane's own invariant names the address from the ONE
    /// file that spells it — the lint `the-estate-address-lives-once`
    /// refuses that literal anywhere else, so a brief could not state
    /// it even if it wanted to.
    #[test]
    fn the_system_of_record_invariant_reads_the_address_from_the_estate_file() {
        let live = std::fs::read_to_string(repo().join("infra/estate/estate.toml"))
            .expect("the estate file");
        let url = sor_url(&live).expect("the estate file spells sor_url");
        assert!(url.starts_with("http"), "{url}");
        // The cluster spelling is a different key and must not be read
        // as this one.
        assert_eq!(sor_url("sor_cluster_url = \"http://x:7900\"\n"), None);
        assert_eq!(
            sor_url("sor_url = \"http://example:7900\"\n").as_deref(),
            Some("http://example:7900")
        );
        let invs = invariants(&repo()).expect("the invariants derive");
        let inv = invs
            .iter()
            .find(|i| i.name == "the system of record")
            .expect("a system-of-record invariant");
        assert_eq!(inv.authority, "infra/estate/estate.toml");
        assert!(
            inv.lines.iter().any(|l| l.contains(&url)),
            "{:?}",
            inv.lines
        );
        assert_eq!(inv.lanes, vec![LANE_STEP]);
    }

    /// A page-audit packet pinned to v2, at its `measure` step, holding
    /// the procedure that was current when it was OPENED — the live
    /// shape read from the system of record (794e8d61, 2026-09-19).
    fn pinned_at_v2() -> Value {
        json!({
            "id": "c0d2caf0-bfc5-4163-b81e-97f9273a0404",
            "kind": "page-audit",
            "workflow_version": 2,
            "steps": [{
                "spec_slug": "measure",
                "kind": "task",
                "status": "active",
                "title": "Inventory the page and the department's needs",
                "metadata": { "procedure": "Read the PAGE and the DEPARTMENT." },
            }],
        })
    }

    /// The registry's ACTIVE row, as `GET /api/workflows/{kind}` serves
    /// it: `version` plus `steps[].metadata_defaults`, which is where a
    /// step's `procedure` is copied FROM at materialisation.
    fn active_row(version: i64, procedure: &str) -> Value {
        json!({
            "kind": "page-audit",
            "version": version,
            "steps": [{
                "title": "measure",
                "metadata_defaults": { "procedure": procedure },
            }],
        })
    }

    /// NOTHING SAID WHICH VERSION OF A PROCEDURE A PACKET WAS RUNNING
    /// (backlog 794e8d61). A step's `procedure` is copied out of the
    /// Workflow row at materialisation, so a packet carries the prose
    /// that was current when it OPENED, permanently — publishing v3
    /// changes what future packets materialise and nothing else. The
    /// brief renders that stored text as the executor's specification
    /// and said nothing about its age, so the lag could not be seen,
    /// measured, or decided about: 47 page-audit packets were open on
    /// v2 and ~94 completions were due to run against superseded
    /// instructions.
    #[test]
    fn the_protocol_section_names_the_version_the_stored_procedure_came_from() {
        let job = pinned_at_v2();

        // BEHIND, AND THE TEXT REALLY MOVED: the version pair alone
        // over-reports, so the verdict is the comparison.
        let out = protocol_section(
            &job,
            Some(&active_row(3, "Read the PAGE, then COMPLETE it.")),
        )
        .expect("a packet with a pinned version has a protocol section");
        assert!(out.contains("page-audit v2"), "{out}");
        assert!(out.contains("1 version behind"), "{out}");
        assert!(out.contains("did NOT reach"), "{out}");

        // BEHIND BY A VERSION THAT DID NOT TOUCH THIS STEP: the stored
        // text is old and current at the same time, which is the case
        // a bare version line would call stale.
        let same = protocol_section(
            &job,
            Some(&active_row(3, "Read the PAGE and the DEPARTMENT.")),
        )
        .expect("section");
        assert!(same.contains("identical"), "{same}");
        assert!(!same.contains("did NOT reach"), "{same}");

        // PINNED TO THE CURRENT VERSION: nothing is lagging.
        let current = protocol_section(
            &job,
            Some(&active_row(2, "Read the PAGE and the DEPARTMENT.")),
        )
        .expect("section");
        assert!(current.contains("is current"), "{current}");

        // THE ROW COULD NOT BE READ. The pin is still a fact and is
        // still printed; what is NOT known is said rather than assumed
        // — a brief that silently claims currency is the defect.
        let unread = protocol_section(&job, None).expect("section");
        assert!(unread.contains("page-audit v2"), "{unread}");
        assert!(unread.contains("not read"), "{unread}");
        assert!(!unread.contains("is current"), "{unread}");

        // A packet with no pinned version prints no claim about one.
        assert_eq!(protocol_section(&json!({"kind": "page-audit"}), None), None);
    }

    /// The section rides in the brief, ABOVE the specification it
    /// dates, and a rendered brief cannot carry the stored procedure
    /// without saying which version it came from.
    #[test]
    fn the_brief_dates_the_specification_it_renders() {
        let job = pinned_at_v2();
        let row = active_row(3, "Read the PAGE, then COMPLETE it.");
        let out = render(&repo(), Some(&job), "analyst", Some(&row), None).expect("renders");
        let packet = out.find("== THE PACKET").expect("the packet half");
        let protocol = out.find("== THE PROTOCOL").expect("the protocol half");
        let step = out.find("== THE STEP").expect("the step half");
        assert!(packet < protocol && protocol < step, "{out}");
    }

    /// THE PRE-FLIGHT DOOR IS READ, NOT RESTATED (backlog 5d919334,
    /// 2026-09-22). CLAUDE.md §Doors decides which mode a builder runs
    /// before a push; the builder rules carried their own copy of it,
    /// still reading `--quick` a day after §Doors moved to `--lint`.
    /// That is the same shape as c94ddc6f one level out: a document
    /// vouching for a tree it did not read.
    #[test]
    fn the_preflight_door_is_read_out_of_the_doors_list() {
        let claude = std::fs::read_to_string(repo().join("CLAUDE.md")).expect("CLAUDE.md");
        let door = preflight_door(&claude)
            .expect("CLAUDE.md §Doors no longer carries a `Before pushing` door");
        assert!(
            door.starts_with("infra/gate.sh --"),
            "the `Before pushing` door is a gate.sh mode; §Doors names `{door}`"
        );

        let invs = invariants(&repo()).expect("the invariants derive from this tree");
        let inv = invs
            .iter()
            .find(|i| i.name == "pre-flight")
            .expect("a `pre-flight` invariant, so a rules document can quote the door");
        assert_eq!(inv.authority, "CLAUDE.md", "§Doors decides this one");
        assert!(inv.lanes.contains(&LANE_CAR), "a car is what gets pushed");
        let lines = inv.lines.join("\n");
        assert!(
            lines.contains(&door),
            "the invariant prints a door §Doors does not name. §Doors: `{door}`\n{lines}"
        );
        // THE COLLAPSE, not a correction: ONE spelling of the mode in
        // the whole block. A second one is the copy this packet is
        // about, one word later.
        assert_eq!(
            lines.matches("infra/gate.sh ").count(),
            1,
            "the pre-flight invariant names a gate.sh mode more than once:\n{lines}"
        );
    }

    /// AN INVARIANT'S FIRST LINE RUNS AS ONE PLAIN COMMAND (backlog
    /// 65cea113, 2026-09-23). The first line of an invariant is the one
    /// a builder copies into a shell, and a builder here runs in a
    /// worktree-isolated agent session whose harness refuses a command
    /// it cannot show stays out of another tree's git. Measured on run
    /// dba4bb17 against this tree, both invariant lines as printed:
    /// `bash infra/gate.sh --lint > <log> 2>&1; echo $?` was refused as
    /// "a construct too complex to verify", and `set -a; . infra/dev/
    /// pod-build.env; set +a` as "a string through ., which can't be
    /// verified" — while `bash infra/gate.sh --lint > <log> 2>&1` alone
    /// ran, and the tool reported its exit code. Three builders in one
    /// night each rediscovered the split by hand. The exit code the
    /// `; echo $?` printed is the command's own, so nothing is lost by
    /// dropping it; a pipe, a list or a sourced file is what the guard
    /// refuses, so none of them may appear before a trailing comment.
    #[test]
    fn every_invariants_first_line_runs_as_one_plain_command() {
        let invs = invariants(&repo()).expect("the invariants derive from this tree");
        let refused = [";", "&&", "||", "|", "$?", "$("];
        for inv in &invs {
            let first = inv.lines.first().expect("an invariant has a first line");
            let command = first.split(" #").next().unwrap_or(first).trim();
            for op in refused {
                assert!(
                    !command.contains(op),
                    "the {:?} invariant's first line carries `{op}`, which a worktree-isolated \
                     builder's harness refuses to run: {first}",
                    inv.name
                );
            }
            assert!(
                !command.starts_with(". ") && !command.starts_with("source "),
                "the {:?} invariant's first line sources a file, which the harness refuses: \
                 {first}",
                inv.name
            );
        }
    }
}
