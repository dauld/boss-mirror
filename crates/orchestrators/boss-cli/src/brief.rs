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
    /// The strings this invariant took OUT of its authority. Each is
    /// pinned to be present both in that file and in the lines
    /// printed, so the label cannot outlive the reading it claims.
    Derived(Vec<String>),
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
                format!("set -a; . {env_file}; set +a     # CARGO_BUILD_JOBS={jobs}"),
                "Nothing else bounds cargo here (there is no .cargo/config.toml), so the".into(),
                "default is one job per CPU — 32 on this pod, against a 16 GiB cgroup.".into(),
            ],
            lanes: vec![LANE_CAR],
            grounding: Grounding::Derived(vec![format!("CARGO_BUILD_JOBS={jobs}")]),
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
            grounding: Grounding::Derived(vec![uid.to_string()]),
        },
        Invariant {
            name: "gate uid / gid",
            authority: manifest.to_string(),
            lines: vec![format!(
                "uid {uid}, gid {gid} — read from the `gate` container, not from prose"
            )],
            lanes: vec![LANE_CAR],
            grounding: Grounding::Derived(vec![uid.to_string(), gid.to_string()]),
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
            grounding: Grounding::Derived(vec!["boss_testing::scratch".into()]),
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
            grounding: Grounding::Derived(vec![admin_url.clone()]),
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
            grounding: Grounding::Derived(phases.clone()),
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
        grounding: Grounding::Derived(vec![sor.clone()]),
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
    }
    Ok(out)
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
         after it, or marked as written ==\n"
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
        for l in &inv.lines {
            out.push_str(&format!("    {l}\n"));
        }
    }
    out
}

/// The packet half: the envelope, the step it is at, and EVERY metadata
/// key verbatim.
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

    let md: BTreeMap<String, Value> = job
        .get("metadata")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    out.push_str(&format!("\nmetadata ({} key(s)), in full:\n", md.len()));
    for (k, v) in &md {
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
    for (k, v) in &spec {
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

/// The profile a packet's current step is briefed under: the
/// `agent_profile` car 1 projects onto the step from its Workflow
/// row's `agent` block, else [`crate::documents::DEFAULT_PROFILE`].
/// A packet with no open step — or no packet at all — is briefed as
/// the default, because the rules document is what the reader came
/// for and a blank brief helps nobody.
pub(crate) fn profile_for(job: Option<&Value>) -> String {
    profile_on_step(job).unwrap_or_else(|| crate::documents::DEFAULT_PROFILE.to_string())
}

/// The `agent_profile` PROJECTED ONTO THE STEP, if there is one.
pub(crate) fn profile_on_step(job: Option<&Value>) -> Option<String> {
    job.and_then(now_step)
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get(boss_jobs::agent_spec::PROFILE_KEY))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The profile the packet's CURRENT WORKFLOW ROW declares for the step
/// it is at — the same fallback `boss dispatch` makes (`block_in_row`),
/// so a hand-run brief renders the lane the dispatch will use.
///
/// Measured 2026-09-19 on page-audit c0d2caf0: the live `measure` step
/// carries `procedure`, `audience` and `authority_role` but NO agent
/// block — the projection is not on these packets — so reading only the
/// step answered `builder` for a step the Workflow row declares
/// `analyst`, and the brief a human read before dispatching was the
/// other lane's.
///
/// Best effort: the row read is an extra call, and a brief that cannot
/// make it is still worth printing, so a failure falls through to the
/// default rather than refusing.
fn profile_in_row(row: &Value, job: &Value) -> Option<String> {
    let slug = now_step(job)?.get("spec_slug").and_then(Value::as_str)?;
    crate::dispatch::block_in_row(row, slug).map(|s| s.profile)
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
    out.push_str(&crate::documents::section(repo, profile, &invs)?);
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
    // `agent_profile` when the projection carries one, else the
    // Workflow row's block for that step — the same two places
    // `dispatch` looks, in the same order — so the human reads what the
    // agent will read. With no packet, or nothing declaring a profile
    // anywhere, it is `builder`, exactly as before profiles existed.
    // `--profile` names a lane without dispatching anything.
    let active = match job.as_ref() {
        Some(j) => active_row(&http, j).await,
        None => None,
    };
    let profile = match (profile_override, profile_on_step(job.as_ref())) {
        (Some(p), _) => p,
        (None, Some(p)) => p,
        (None, None) => match (job.as_ref(), active.as_ref()) {
            (Some(j), Some(row)) => profile_in_row(row, j)
                .unwrap_or_else(|| crate::documents::DEFAULT_PROFILE.to_string()),
            _ => crate::documents::DEFAULT_PROFILE.to_string(),
        },
    };
    print!(
        "{}",
        render(&repo, job.as_ref(), &profile, active.as_ref())?
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
    fn every_derived_invariant_can_show_its_reading_in_its_authority() {
        let invs = invariants(&repo()).expect("the invariants derive from this tree");
        let mut derived = 0;
        for inv in &invs {
            let Grounding::Derived(values) = &inv.grounding else {
                continue;
            };
            derived += 1;
            assert!(
                !values.is_empty(),
                "{:?} claims a reading of nothing",
                inv.name
            );
            let authority = std::fs::read_to_string(repo().join(&inv.authority))
                .unwrap_or_else(|_| panic!("{} reads", inv.authority));
            let lines = inv.lines.join("\n");
            for v in values {
                assert!(
                    !v.trim().is_empty(),
                    "{:?} claims an empty reading",
                    inv.name
                );
                assert!(
                    authority.contains(v.as_str()),
                    "{:?} says it read `{v}` out of {}, and that file does not contain it",
                    inv.name,
                    inv.authority
                );
                assert!(
                    lines.contains(v.as_str()),
                    "{:?} read `{v}` and then does not print it",
                    inv.name
                );
            }
        }
        assert!(
            derived >= 5,
            "most invariants are read, not written: {derived}"
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
                { "spec_slug": "build", "status": "ready",
                  "metadata": { "agent_profile": "analyst", "agent_model": "opus-5[1m]" } },
            ],
        });
        assert_eq!(profile_for(Some(&declared)), "analyst");
        let undeclared = json!({
            "steps": [{ "spec_slug": "build", "status": "ready", "metadata": {} }],
        });
        assert_eq!(profile_for(Some(&undeclared)), "builder");
        assert_eq!(profile_for(None), "builder");

        let out = render(&repo(), Some(&undeclared), "builder", None).expect("renders");
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
        let alone = render(&repo(), None, "builder", None).expect("renders");
        assert!(!alone.contains("== THE PACKET"));
        assert!(alone.contains("== THE RULES"));
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
        let rendered = render(&repo(), Some(&build), "builder", None).expect("renders");
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
        let analyst = render(&repo(), Some(&job), "analyst", None).expect("renders");
        let builder = render(&repo(), Some(&job), "builder", None).expect("renders");

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
        let out = render(&repo(), Some(&job), "analyst", Some(&row)).expect("renders");
        let packet = out.find("== THE PACKET").expect("the packet half");
        let protocol = out.find("== THE PROTOCOL").expect("the protocol half");
        let step = out.find("== THE STEP").expect("the step half");
        assert!(packet < protocol && protocol < step, "{out}");
    }
}
