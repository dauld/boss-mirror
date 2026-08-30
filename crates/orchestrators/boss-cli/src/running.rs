//! `boss running` — merged, deployed and running are three facts.
//!
//! WHY THIS IS A VERB. The second of the three questions backlog-item
//! 26b3d203 names as hand-rolled every session: "which release is
//! deployed and does it carry this change?" On 2026-08-29 alone it was
//! written fresh at least six times, and it went wrong in both
//! directions:
//!
//!   - A migration was present in the deployed commit while the live
//!     registry still answered the OLD value, because the train had
//!     DEPLOYED but not CONVERGED and convergence is what runs
//!     migrate.sh. A source-level check would have recorded a true
//!     statement about the tree beside a false one about production.
//!   - A StepType was merged AND deployed while the API still reported
//!     45 kinds, because the registry is built from a TOML embedded in
//!     the binary at startup and the pods had not rolled.
//!   - A CI image was nearly reported stale off the wrong docker
//!     daemon — the user-level one rather than the rootful one the
//!     runner actually uses.
//!
//! THE DISTINCTION IS THE POINT. Three different facts get called
//! "deployed" in conversation:
//!
//!   MERGED    the forge's main carries it            (`boss merged`)
//!   DEPLOYED  a release directory exists and `current` points at it
//!   RUNNING   the process serving requests was built from it
//!
//! They are routinely different, and the gap between the last two is
//! where a proof records something true about a tree and false about
//! production. This verb reports all three side by side so the gap is
//! visible rather than assumed.

use anyhow::Result;

/// What the three layers say. `None` everywhere means "could not read",
/// never "absent" — the same fail-closed rule the gate's concurrency
/// guard and `boss merged` both use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Generations {
    /// The forge's main, via ls-remote. The merge authority.
    pub main: Option<String>,
    /// What `/usr/local/boss/current` resolves to on the deploy host.
    pub deployed: Option<String>,
    /// The commit the live jobs API reports serving, from
    /// `capabilities.commit`. This is the only one that describes a
    /// PROCESS rather than a filesystem.
    pub running: Option<String>,
}

/// How far apart the layers are, in words a reader can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alignment {
    /// All three agree — the ordinary steady state.
    Aligned,
    /// Deployed matches main but the process predates it. The pods have
    /// not rolled; anything built into the binary (StepType registry,
    /// embedded seeds) is still the OLD value even though the file is
    /// on disk.
    NotRolled,
    /// The deploy host is behind main. A merge has not reached it.
    NotDeployed,
    /// Something could not be read. Never reported as agreement.
    Unknown(String),
}

fn short(s: &Option<String>) -> &str {
    s.as_deref().map_or("UNREADABLE", |v| {
        // Callers compare full shas; humans read twelve characters.
        if v.len() >= 12 { &v[..12] } else { v }
    })
}

/// Do two revisions name the same commit?
///
/// NOT `==`. The three layers report the same commit at different
/// lengths: a release directory is named with an abbreviated sha
/// (`releases/3139df18`) while the API reports the full forty. Comparing
/// them directly reports NOT ROLLED forever, which is the failure this
/// verb exists to detect — so it would have lied in exactly its own
/// subject matter.
///
/// Caught by running it against the live cluster, not by the unit tests,
/// which all used equal-length fixtures. That is why this one has a test
/// with mismatched lengths.
fn same_commit(a: &str, b: &str) -> bool {
    let n = a.len().min(b.len());
    // Seven is git's own floor for an unambiguous abbreviation.
    n >= 7 && a[..n].eq_ignore_ascii_case(&b[..n])
}

/// The decision, pure.
///
/// Deliberately reports NotRolled BEFORE NotDeployed: when both are
/// true the process is the binding constraint, and it is the one people
/// forget. A packet whose change is on disk but not in the running
/// process is the exact shape that made a proof refuse today.
pub fn align(g: &Generations) -> Alignment {
    let (Some(main), Some(dep)) = (&g.main, &g.deployed) else {
        return Alignment::Unknown(
            "could not read main or the deployed release — an unread layer is \
             not an aligned one"
                .into(),
        );
    };
    let Some(run) = &g.running else {
        return Alignment::Unknown(
            "could not read the running commit from the live API. The process \
             is the only layer that describes what is actually serving; \
             without it, deployed is not evidence"
                .into(),
        );
    };
    if !same_commit(dep, run) {
        return Alignment::NotRolled;
    }
    if !same_commit(dep, main) {
        return Alignment::NotDeployed;
    }
    Alignment::Aligned
}

fn sh(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(args[0])
        .args(&args[1..])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Read `capabilities.commit` out of the live jobs API health endpoint.
/// Hand-parsed rather than pulling a JSON dependency into a read that
/// must not fail for its own reasons.
fn running_commit(jobs_url: &str) -> Option<String> {
    let body = sh(&[
        "curl",
        "-s",
        "-m",
        "20",
        &format!("{jobs_url}/api/jobs/health"),
    ])?;
    let idx = body.find("\"commit\"")?;
    let rest = &body[idx + 8..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    let v = rest[start..end].to_string();
    (!v.is_empty()).then_some(v)
}

pub fn run(clone: Option<String>, remote: Option<String>, jobs_url: Option<String>) -> Result<()> {
    let clone = clone.unwrap_or_else(|| ".".to_string());
    let remote = remote.unwrap_or_else(|| "origin".to_string());
    let jobs_url = jobs_url
        .or_else(|| std::env::var("BOSS_JOBS_URL").ok())
        .unwrap_or_else(|| "http://10.20.0.34:7900".to_string());

    let g = Generations {
        main: sh(&["git", "-C", &clone, "ls-remote", &remote, "refs/heads/main"])
            .and_then(|s| s.split_whitespace().next().map(str::to_string)),
        deployed: sh(&["readlink", "/usr/local/boss/current"])
            .map(|s| s.rsplit('/').next().unwrap_or(&s).to_string()),
        running: running_commit(&jobs_url),
    };

    println!("boss running:");
    println!("  merged   main({remote})  {}", short(&g.main));
    println!("  deployed current         {}", short(&g.deployed));
    println!("  running  jobs api        {}", short(&g.running));
    match align(&g) {
        Alignment::Aligned => println!("  ALIGNED — all three agree"),
        Alignment::NotRolled => println!(
            "  NOT ROLLED — the release is on disk but the process predates it. \n\
             \x20   Anything compiled INTO the binary (the StepType registry, embedded \n\
             \x20   seeds) is still the old value. Proving against the tree here records \n\
             \x20   something true about the files and false about production."
        ),
        Alignment::NotDeployed => println!(
            "  NOT DEPLOYED — main has moved past the deploy host. The merge \n\
             \x20   landed; nothing is serving it yet."
        ),
        Alignment::Unknown(why) => println!("  UNKNOWN — {why}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixtures are FULL-LENGTH on purpose. The first version of these
    /// tests used three-character shas, which is below git's seven-char
    /// floor for an unambiguous abbreviation — and that unrealism is
    /// exactly what let a real bug through: the live layers report the
    /// same commit at different lengths, and equality called the steady
    /// state NOT ROLLED. A fixture that cannot exhibit the failure
    /// cannot catch it.
    fn sha(seed: &str) -> String {
        format!("{seed:0<40}")
    }

    fn g(main: &str, dep: &str, run: &str) -> Generations {
        Generations {
            main: Some(sha(main)),
            deployed: Some(sha(dep)),
            running: Some(sha(run)),
        }
    }

    /// THE BUG THE FIXTURES HID. The layers report the same commit at
    /// different lengths — a release directory carries an abbreviated
    /// sha, the API reports the full one. Equality would call the
    /// steady state NOT ROLLED, so the verb would have been wrong about
    /// precisely the thing it exists to detect.
    #[test]
    fn a_short_release_name_matches_a_full_sha() {
        let live = Generations {
            main: Some("3139df18f68e71306a4a08f47d74afbf5e3ad09d".into()),
            deployed: Some("3139df18".into()),
            running: Some("3139df18f68e71306a4a08f47d74afbf5e3ad09d".into()),
        };
        assert_eq!(align(&live), Alignment::Aligned);
    }

    /// ...and an abbreviation that is too short to be unambiguous is not
    /// a match. Git's own floor is seven.
    #[test]
    fn an_ambiguous_abbreviation_is_not_a_match() {
        assert!(!same_commit("313", "3139df18f68e"));
        assert!(same_commit("3139df1", "3139df18f68e"));
    }

    #[test]
    fn all_three_agreeing_is_the_only_aligned_state() {
        assert_eq!(
            align(&g("aaaaaaa", "aaaaaaa", "aaaaaaa")),
            Alignment::Aligned
        );
    }

    /// THE CASE THAT MADE A PROOF REFUSE TODAY. The migration was in the
    /// deployed commit and the live registry still answered the old
    /// value, because the pods had not rolled. Deployed is a filesystem
    /// fact; running is a process fact.
    #[test]
    fn on_disk_but_not_in_the_process_is_not_rolled() {
        assert_eq!(
            align(&g("aaaaaaa", "aaaaaaa", "bbbbbbb")),
            Alignment::NotRolled
        );
    }

    /// And when BOTH are behind, the process is still the binding
    /// constraint — reporting NotDeployed first would send a reader to
    /// the deploy when the roll is what they are waiting on.
    #[test]
    fn the_process_is_reported_before_the_deploy() {
        assert_eq!(
            align(&g("ccccccc", "ddddddd", "eeeeeee")),
            Alignment::NotRolled
        );
    }

    #[test]
    fn a_merge_that_has_not_reached_the_host_is_not_deployed() {
        assert_eq!(
            align(&g("ccccccc", "ddddddd", "ddddddd")),
            Alignment::NotDeployed
        );
    }

    /// An unread layer is never an aligned one — the same fail-closed
    /// rule as the gate's concurrency guard and `boss merged`.
    #[test]
    fn an_unread_layer_is_never_agreement() {
        for miss in [
            Generations {
                main: None,
                ..g("aaaaaaa", "aaaaaaa", "aaaaaaa")
            },
            Generations {
                deployed: None,
                ..g("aaaaaaa", "aaaaaaa", "aaaaaaa")
            },
            Generations {
                running: None,
                ..g("aaaaaaa", "aaaaaaa", "aaaaaaa")
            },
        ] {
            assert!(
                matches!(align(&miss), Alignment::Unknown(_)),
                "a missing layer must not read as aligned: {miss:?}"
            );
        }
    }
}
