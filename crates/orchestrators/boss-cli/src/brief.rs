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
//! Read-only, and it writes nothing. The packet read goes through
//! `gate::api`, so it inherits the no-default `BOSS_JOBS_URL` rule and
//! is signed as the caller.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One invariant a brief can reference instead of restating, and the
/// file in the tree that DECIDES it.
///
/// `authority` is repo-relative and is checked to exist: an invariant
/// whose authority is missing is not a weaker invariant, it is a
/// sentence, and this verb exists to stop printing those.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Invariant {
    pub(crate) name: &'static str,
    pub(crate) authority: String,
    pub(crate) lines: Vec<String>,
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
        },
        Invariant {
            name: "gate uid / gid",
            authority: manifest.to_string(),
            lines: vec![format!(
                "uid {uid}, gid {gid} — read from the `gate` container, not from prose"
            )],
        },
        Invariant {
            name: "fixture paths",
            authority: fixture_lint.to_string(),
            lines: vec![
                format!("boss_testing::scratch (see {scratch}) — pid AND uid in the path"),
                "A fixed /tmp path is shared with every account on this long-lived pod;".into(),
                "the lint named above is in the pre-flight roster and refuses one.".into(),
            ],
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
        },
        Invariant {
            name: "order",
            authority: gate.to_string(),
            lines: vec![
                "failing test -> watch it fail -> fix -> cargo fmt -> COMMIT AND PUSH".into(),
                "-> then any optional local check. The gate is the compile authority;".into(),
                "a cold local build that is never pushed parks a car with nothing on it.".into(),
            ],
        },
        Invariant {
            name: "base check",
            authority: freshness.to_string(),
            lines: vec![
                "git merge-base --is-ancestor origin/main HEAD     # must exit 0".into(),
                "git diff --numstat origin/main HEAD               # only your files".into(),
                "A branch on an old base merges clean and reverts landed work.".into(),
            ],
        },
        Invariant {
            name: "gate phases",
            authority: gate.to_string(),
            lines: vec![
                format!("{} pre-flight lints, then: {}", lints, phases.join(", ")),
                "Derived from the gate's own `check` call sites and `--roster`, so this".into(),
                "list cannot fall behind the gate that judges the car.".into(),
            ],
        },
    ];
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

/// The invariant half, rendered.
pub(crate) fn invariant_section(invs: &[Invariant]) -> String {
    let mut out = String::from(
        "== THE INVARIANTS — derived from the file named after each, not restated ==\n",
    );
    for inv in invs {
        out.push_str(&format!("\n{}   [{}]\n", inv.name, inv.authority));
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
    let at = crate::envelope::steps(job)
        .into_iter()
        .map(crate::envelope::step_line)
        .find(|l| l.now)
        .map(|l| format!("{} ({})", l.slug, l.status));
    out.push_str(&format!(
        "now at: {}\n",
        at.as_deref().unwrap_or("no ready or active step")
    ));

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
fn repo_root() -> Result<PathBuf> {
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

pub async fn run(packet_ref: Option<String>) -> Result<()> {
    let repo = repo_root()?;
    let invs = invariants(&repo)?;

    if let Some(r) = packet_ref {
        let http = reqwest::Client::new();
        let id = crate::job::fetch_and_resolve(&http, &r).await?;
        let job = crate::gate::api(
            &http,
            reqwest::Method::GET,
            &format!("/api/jobs/{id}"),
            None,
        )
        .await?
        .context("the packet read returned no body")?;
        print!("{}", packet_section(&job));
        println!();
    }
    print!("{}", invariant_section(&invs));
    println!("\n{HOW_TO_USE}");
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

    #[test]
    fn the_rendered_invariants_name_their_authority_beside_each_statement() {
        let invs = invariants(&repo()).expect("the invariants derive");
        let rendered = invariant_section(&invs);
        for inv in &invs {
            assert!(
                rendered.contains(&inv.authority),
                "{:?} is printed without the file that decides it",
                inv.name
            );
        }
        assert!(rendered.contains("infra/dev/as-gate-uid.sh"));
        assert!(rendered.contains("pre-flight lints"));
    }
}
