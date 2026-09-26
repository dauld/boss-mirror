//! `boss car unland <car> --merge-ref <sha>` — close a car whose landing
//! forge main LOST, and open the successor that rides again (backlog
//! f9256445, design d812f1b7 D2, Q1 answered 2026-09-25).
//!
//! WHY. Train 2026-09-25 20:04 (PR #687) merged car dec3136a as squash
//! commit c85941b4 at 20:10:41Z. By 20:11:14Z forge main was back at
//! 777a5888, and the 20:34 train was assembled on that real main. The
//! car read landed — "landed on main as c85941b423bc" on its review step,
//! `merged: "true"` on the job — and its `proven` step stood ready for a
//! change main does not carry. Nothing in the protocol could end it
//! truthfully; the pure half of why is `boss_jobs::car_unland`.
//!
//! THE READING IS THE VERB'S OWN. It does not trust the caller's word
//! that main lost the merge — not an operator's, not the conductor's.
//! [`read_main`] reads forge main with `git ls-remote origin
//! refs/heads/main` and asks `git merge-base --is-ancestor <merge>
//! <main>` in the clone it runs in. Exit 1 is the only NOT; exit 0 is a
//! refusal (the landing stands), and any other answer — no forge, an
//! object the clone cannot read, a crash — is a refusal to judge, never
//! a verdict. A wrong target answers instead of erroring, so it is the
//! answer, not the absence of one, that is read.
//!
//! WHAT IT WRITES, IN THIS ORDER, EACH SKIPPED ON A RE-RUN THAT FINDS IT
//! DONE: the SUCCESSOR car for the same branch (`supersedes` naming this
//! one, scope and build carried, first open work `gate`); a CORRECTION
//! beside the review step's landing note through the corrections door
//! (the step stays as it was — both facts stay in the log); then the
//! three writes that close this car through `unlanded`, read back. The
//! successor comes first because the terminal requires it: a terminal
//! naming a successor that does not exist would be a claim.
//!
//! ONE WRITER, TWO CALLERS. The conductor's merge-lost arm calls
//! [`unland_car`] for every car of a train that ends `merge-lost`,
//! through its own client ([`crate::car_retire::Door`]); an operator
//! calls it through this verb. Neither has a second copy of the writes.

use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow, bail};
use boss_jobs::car;
use boss_jobs::car_unland::{
    self as core, UNLANDED_SLUG, Unlanding, review_correction, successor_body, successor_of,
    successor_writes, unland_writes,
};
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde_json::{Value, json};

use crate::car_retire::{Door, Http, apply_via, read_via};
use crate::git_auth::ForgeAuth;

/// What git read about forge main and one merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MainReading {
    /// Forge main at the read, as `ls-remote` answered it.
    pub main: String,
    /// The merge, resolved to its full sha in the clone.
    pub merge: String,
    /// RFC3339, when the read was made.
    pub read_at: String,
    /// Does main carry the merge (`merge-base --is-ancestor` exit 0)?
    pub carries: bool,
}

impl MainReading {
    /// The reading as the record states it: both reads, the command and
    /// its answer.
    pub(crate) fn evidence(&self) -> String {
        format!(
            "forge main was {} (git ls-remote origin refs/heads/main) at {}; git merge-base \
             --is-ancestor {} {} exited {}",
            self.main,
            self.read_at,
            self.merge,
            self.main,
            if self.carries { 0 } else { 1 }
        )
    }
}

/// The git every read here runs: the forge credential on it the way
/// `sh_in` puts it on the conductor's clone, fetch and push. Bare, the
/// conductor pod's `ls-remote origin` asked for a username and the
/// merge-lost arm never judged (backlog cb3d8952, 2026-09-26). With no
/// token file — `boss car unland` on the dev pod — it is a plain git and
/// the clone's own credential helper answers, as before.
fn git_command(repo: &Path, auth: Option<&ForgeAuth>) -> Command {
    let mut cmd = crate::git_auth::command_with(auth);
    cmd.arg("-C").arg(repo);
    cmd
}

fn git(repo: &Path, args: &[&str]) -> Result<Output, String> {
    git_command(repo, crate::git_auth::forge_auth().as_ref())
        .args(args)
        .output()
        .map_err(|e| format!("git {} could not run: {e}", args.join(" ")))
}

/// Run git and require exit 0; the stdout, trimmed.
fn git_ok(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(repo, args)?;
    if !out.status.success() {
        return Err(format!(
            "git {} exited {}: {}",
            args.join(" "),
            out.status
                .code()
                .map_or_else(|| "on a signal".to_string(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The full sha `rev` names in this clone, if it names a commit here.
fn commit_here(repo: &Path, rev: &str) -> Option<String> {
    git_ok(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .ok()
    .filter(|s| !s.is_empty())
}

fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Read whether forge main carries `merge`, from the clone at `repo`
/// whose `origin` is the forge. `Err` is a refusal to judge — every git
/// failure lands there, and none of them is a NOT.
///
/// A merge main lost may never have been fetched into this clone: main
/// moved back within 32 seconds on 2026-09-25, inside one conductor
/// pass. So an unknown merge named by its FULL sha is fetched by sha
/// (the forge still holds the object, measured by triage d081da27); an
/// abbreviated one the clone cannot resolve is refused, naming the fix.
pub(crate) fn read_main(
    repo: &Path,
    merge: &str,
    now: DateTime<Utc>,
) -> Result<MainReading, String> {
    let merge = merge.trim();
    let line = git_ok(repo, &["ls-remote", "origin", "refs/heads/main"])?;
    let main = line
        .split_whitespace()
        .next()
        .filter(|s| is_full_sha(s))
        .ok_or_else(|| format!("the forge answered no main (ls-remote said {line:?})"))?
        .to_string();
    if commit_here(repo, &main).is_none() {
        git_ok(repo, &["fetch", "-q", "origin", "refs/heads/main"])?;
    }
    if commit_here(repo, &main).is_none() {
        return Err(format!(
            "forge main {main} is not readable in this clone even after a fetch"
        ));
    }
    let merge_full = match commit_here(repo, merge) {
        Some(sha) => sha,
        None if is_full_sha(merge) => {
            git_ok(repo, &["fetch", "-q", "origin", merge])?;
            commit_here(repo, merge).ok_or_else(|| {
                format!("the merge {merge} is not readable in this clone even after a fetch")
            })?
        }
        None => {
            return Err(format!(
                "the merge {merge} is not a commit this clone holds, and an abbreviated sha \
                 cannot be fetched — name it by its full sha (the forge's merge_commit_sha \
                 for the train's PR)"
            ));
        }
    };
    let out = git(repo, &["merge-base", "--is-ancestor", &merge_full, &main])?;
    let carries = match out.status.code() {
        Some(0) => true,
        Some(1) => false,
        other => {
            return Err(format!(
                "git merge-base --is-ancestor {merge_full} {main} gave no answer (exit {}): {}",
                other.map_or_else(|| "on a signal".to_string(), |c| c.to_string()),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    };
    Ok(MainReading {
        main,
        merge: merge_full,
        read_at: crate::gate::stamp(now),
        carries,
    })
}

/// What an unlanding left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unlanded {
    /// The car closed through `unlanded`.
    pub car: String,
    /// Its successor's full id.
    pub successor: String,
    /// The reading it closed on (None when a pass before this one had
    /// already closed it).
    pub evidence: Option<String>,
}

fn id_of(v: &Value) -> Result<String> {
    v.get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("a packet without an id")
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

/// Open the successor, or find the one a pass before this one opened,
/// and bring it to `gate`.
async fn successor(door: &dyn Door, pred: &Value, lost: &str, at: &str) -> Result<String> {
    let pid = id_of(pred)?;
    let filter = json!({ core::SUPERSEDES: pid }).to_string();
    let listed = door
        .send(
            Method::GET,
            &format!(
                "/api/jobs?kind=ship-a-change&limit=20&metadata={}",
                percent_encoding::utf8_percent_encode(&filter, crate::job::QUERY_VALUE)
            ),
            None,
        )
        .await?;
    let cars = crate::train::rows(listed).context("listing the car's successors")?;
    let sid = match successor_of(&cars, &pid) {
        Some(found) => id_of(found)?,
        None => {
            let body = successor_body(pred).map_err(|e| anyhow!(e))?;
            let made = door
                .send(Method::POST, "/api/jobs", Some(body))
                .await?
                .context("opening the successor answered no body")?;
            id_of(&made)?
        }
    };
    let fresh = read_via(door, &sid).await?;
    for w in successor_writes(&fresh, pred, lost, at).map_err(|e| anyhow!(e))? {
        door.send(Method::PATCH, &w.merge_path(&sid), Some(w.metadata.clone()))
            .await
            .with_context(|| format!("carrying `{}` onto the successor", w.title))?;
        door.send(
            Method::PUT,
            &w.status_path(&sid),
            Some(w.status_body.clone()),
        )
        .await
        .with_context(|| format!("completing `{}` on the successor", w.title))?;
    }
    Ok(sid)
}

/// THE WRITER: unland `car_id`, landed as `merge`, reading forge main
/// from the clone at `repo`. Idempotent — a car already closed through
/// `unlanded` answers with the successor it names, and every write below
/// is skipped when a pass before this one made it.
pub(crate) async fn unland_car(
    door: &dyn Door,
    repo: &Path,
    car_id: &str,
    merge: &str,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<Unlanded> {
    let car_json = read_via(door, car_id).await?;
    let id = id_of(&car_json)?;
    if car_json
        .pointer("/metadata/outcome")
        .and_then(Value::as_str)
        == Some(UNLANDED_SLUG)
    {
        let by = car_json
            .pointer("/metadata/superseded_by")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        return Ok(Unlanded {
            car: id,
            successor: by,
            evidence: None,
        });
    }
    core::unlandable(&car_json, merge).map_err(|e| anyhow!("REFUSED — {e}"))?;
    // The terminal must exist on the car's pinned version BEFORE anything
    // is written: a successor opened for a car that then cannot close
    // would be a twin riding beside a car that still reads landed.
    let probe = Unlanding {
        merge_ref: merge.to_string(),
        evidence: "-".into(),
        superseded_by: "-".into(),
        completed_at: String::new(),
    };
    unland_writes(&car_json, &probe).map_err(|e| anyhow!("REFUSED — {e}"))?;

    let reading = read_main(repo, merge, now)
        .map_err(|e| anyhow!("REFUSED — could not read whether main carries {merge}: {e}"))?;
    if reading.carries {
        bail!(
            "REFUSED — forge main {} carries {} (git merge-base --is-ancestor exited 0): the \
             landing stands, and there is nothing to unland",
            short(&reading.main),
            short(&reading.merge)
        );
    }
    let recorded = car_json
        .pointer("/metadata/merge_ref")
        .and_then(Value::as_str)
        .unwrap_or(merge);
    let lost = format!(
        "merged as {recorded}, then lost: forge main {} at {} does not carry it — not on main",
        short(&reading.main),
        reading.read_at
    );
    if dry_run {
        println!(
            "  DRY — main does not carry {}: would open the successor for {}, correct the \
             landing note and close car {} through `{UNLANDED_SLUG}`",
            short(&reading.merge),
            car_json
                .pointer("/metadata/branch")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            short(&id)
        );
        return Ok(Unlanded {
            car: id,
            successor: String::new(),
            evidence: Some(reading.evidence()),
        });
    }

    let sid = successor(door, &car_json, recorded, &reading.read_at).await?;

    if let Some((reads, should_read)) =
        review_correction(&car_json, &format!("{lost}; rides again as car {sid}"))
        && let Some(review) = car::find_step(&car_json, car::REVIEW_SLUG, car::REVIEW)
            .and_then(|s| s.get("id"))
            .and_then(Value::as_str)
    {
        door.send(
            Method::POST,
            &format!("/api/jobs/{id}/steps/{review}/corrections"),
            Some(crate::correct::correction_body(
                "note",
                &reads,
                &should_read,
                Some("forge main lost the train's merge (boss car unland, backlog f9256445)"),
            )),
        )
        .await
        .context("correcting the landing note")?;
    }

    let current = read_via(door, &id).await?;
    let writes = unland_writes(
        &current,
        &Unlanding {
            merge_ref: recorded.to_string(),
            evidence: reading.evidence(),
            superseded_by: sid.clone(),
            completed_at: reading.read_at.clone(),
        },
    )
    .map_err(|e| anyhow!("REFUSED — {e}"))?;
    apply_via(
        door,
        &id,
        &current,
        &writes,
        UNLANDED_SLUG,
        "boss car unland",
    )
    .await?;
    Ok(Unlanded {
        car: id,
        successor: sid,
        evidence: Some(reading.evidence()),
    })
}

/// `boss car unland`.
pub(crate) async fn unland(
    given: &str,
    merge_ref: &str,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<()> {
    let http = reqwest::Client::new();
    // The actor FIRST: a write nobody names is refused, and the refusal
    // costs a line rather than a half-unlanded car (5083d6f5).
    if !dry_run {
        crate::identity::sign(&Method::PATCH, "/api/jobs")?;
    }
    let (found, branch) = crate::rerail::find_car(&http, given).await?;
    let id = id_of(&found)?;
    println!("boss car unland: car {} ({branch})", short(&id));
    // A car pinned to a version without the terminal is MOVED first —
    // through the conversion door, which previews, refuses an unsafe
    // move by step, and records the re-pin on the packet (7cf202a9).
    if car::find_step(&found, UNLANDED_SLUG, "").is_none() {
        println!(
            "  car {} is pinned to v{} with no `{UNLANDED_SLUG}` terminal — converting it",
            short(&id),
            found["workflow_version"]
        );
        crate::job::convert(&id, None, dry_run).await?;
        if dry_run {
            println!("boss car unland: DRY — nothing written past the conversion preview");
            return Ok(());
        }
    }
    let done = unland_car(&Http(&http), Path::new("."), &id, merge_ref, dry_run, now)
        .await
        .map_err(|e| anyhow!("boss car unland: {e}"))?;
    if let Some(evidence) = &done.evidence {
        println!("  evidence: {evidence}");
    }
    if !dry_run {
        println!(
            "  successor: car {} for {branch} — its first open work is `gate`: rebase or \
             merge it onto current main and gate it with the park flags (boss gate {branch} \
             --park-file …), and the auto-park handler adopts the green onto it",
            short(&done.successor)
        );
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn git_in(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The forge (a bare repo whose main is at `base`) and a clone of it
    /// that has fetched a merge on top — then main is rewound to `base`
    /// with a force push, the way 2026-09-25's main lost c85941b4.
    /// Returns (clone, base, merge).
    pub(crate) fn forge_that_lost_a_merge(tag: &str) -> (std::path::PathBuf, String, String) {
        let root = boss_testing::scratch::scratch_dir(tag);
        let forge = root.join("forge.git");
        let clone = root.join("clone");
        let work = root.join("work");
        std::fs::create_dir_all(&forge).unwrap();
        git_in(&forge, &["init", "-q", "--bare", "-b", "main"]);
        std::fs::create_dir_all(&work).unwrap();
        git_in(&work, &["init", "-q", "-b", "main"]);
        boss_testing::scratch::write_file(&work.join("a.txt"), "base\n");
        git_in(&work, &["add", "."]);
        git_in(&work, &["commit", "-qm", "the 19:26 train"]);
        let base = git_in(&work, &["rev-parse", "HEAD"]);
        git_in(&work, &["remote", "add", "origin", forge.to_str().unwrap()]);
        git_in(&work, &["push", "-q", "origin", "main"]);
        std::fs::create_dir_all(&clone).unwrap();
        git_in(&clone, &["clone", "-q", forge.to_str().unwrap(), "."]);
        boss_testing::scratch::write_file(&work.join("a.txt"), "base\nthe car\n");
        git_in(&work, &["commit", "-qam", "train 20:04 (#687)"]);
        let merge = git_in(&work, &["rev-parse", "HEAD"]);
        git_in(&work, &["push", "-q", "origin", "main"]);
        (clone, base, merge)
    }

    pub(crate) fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-25T21:19:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// The git the merge-lost arm reads forge main with carries the
    /// conductor's forge credential. Backlog cb3d8952, measured
    /// 2026-09-26 01:10Z: `fn git` was a bare `git`, so inside the
    /// conductor pod — which has only the token file git_auth reads —
    /// every `ls-remote origin` exited 128 "could not read Username",
    /// the arm never judged, and train c94d5d39 could not close. The
    /// same pass's `fetch origin` succeeded through `sh_in`, which builds
    /// on git_auth: the credential is host-scoped, so the remote's name
    /// was never the fault. The tests above use a local bare forge that
    /// asks no credential, which is why they could not see it.
    #[test]
    fn the_merge_lost_arm_reads_forge_main_with_the_forge_credential() {
        let root = boss_testing::scratch::scratch_dir("unland-auth");
        git_in(&root, &["init", "-q"]);
        let auth = crate::git_auth::ForgeAuth {
            key: "http.http://forge.test/.extraHeader".to_string(),
            value: "Authorization: token t".to_string(),
        };
        let out = git_command(&root, Some(&auth))
            .args(["config", "--get", &auth.key])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the credential is not on the arm's git: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "Authorization: token t"
        );
        // And `-C <repo>` still lands it in the clone it was given.
        let top = git_command(&root, None)
            .args(["rev-parse", "--absolute-git-dir"])
            .output()
            .unwrap();
        assert_eq!(
            std::path::PathBuf::from(String::from_utf8_lossy(&top.stdout).trim()),
            root.canonicalize().unwrap().join(".git")
        );
    }

    /// The pin for the half the test above cannot reach (CLAUDE.md 9a):
    /// every git this module runs outside its tests is built by
    /// `git_command`, so no second, bare spawner can come back.
    #[test]
    fn no_git_in_this_module_is_spawned_bare() {
        let src = include_str!("car_unland.rs");
        let live = src.split("#[cfg(test)]").next().unwrap();
        let bare = format!("Command::new({:?})", "git");
        assert!(
            !live.contains(&bare),
            "car_unland.rs spawns a bare git outside its tests — build it with git_command, \
             which carries the forge credential (backlog cb3d8952)"
        );
        assert!(live.contains("crate::git_auth::command_with("));
    }

    /// While main carries the merge the reading says so; after the
    /// rewind it reads NOT — and it reads it even though the clone never
    /// fetched the merge, because the full sha is fetched by sha.
    #[test]
    fn a_merge_main_lost_reads_not_carried_and_a_carried_one_reads_carried() {
        let (clone, base, merge) = forge_that_lost_a_merge("unland-read");
        let carried = read_main(&clone, &merge, now()).unwrap();
        assert!(carried.carries, "{carried:?}");
        assert_eq!(carried.main, merge);

        // The rewind: main written back to `base`, a force push.
        let work = clone.parent().unwrap().join("work");
        git_in(
            &work,
            &[
                "push",
                "-q",
                "-f",
                "origin",
                &format!("{base}:refs/heads/main"),
            ],
        );
        let fresh = clone.parent().unwrap().join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        git_in(
            &fresh,
            &[
                "clone",
                "-q",
                "--no-local",
                clone.parent().unwrap().join("forge.git").to_str().unwrap(),
                ".",
            ],
        );
        // `fresh` never saw the merge: the full sha is fetched by sha.
        let lost = read_main(&fresh, &merge, now()).unwrap();
        assert!(!lost.carries, "{lost:?}");
        assert_eq!(lost.main, base);
        assert_eq!(lost.merge, merge);
        assert!(
            lost.evidence().contains(&format!(
                "git merge-base --is-ancestor {merge} {base} exited 1"
            )),
            "{}",
            lost.evidence()
        );
        assert!(lost.evidence().contains("at 2026-09-25T21:19:00Z"));
    }

    /// A git failure is a refusal to judge, never a NOT: an abbreviated
    /// merge the clone cannot resolve, and a clone with no forge.
    #[test]
    fn a_reading_git_cannot_make_is_refused_not_read_as_lost() {
        let (clone, base, merge) = forge_that_lost_a_merge("unland-refuse");
        let work = clone.parent().unwrap().join("work");
        git_in(
            &work,
            &[
                "push",
                "-q",
                "-f",
                "origin",
                &format!("{base}:refs/heads/main"),
            ],
        );
        // A clone made after the rewind never saw the merge, and its
        // 12-character spelling cannot be fetched. `--no-local`: a local
        // clone copies the object directory whole, unreachable merge and
        // all, and would answer the question this test asks.
        let fresh = clone.parent().unwrap().join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        git_in(
            &fresh,
            &[
                "clone",
                "-q",
                "--no-local",
                clone.parent().unwrap().join("forge.git").to_str().unwrap(),
                ".",
            ],
        );
        let e = read_main(&fresh, &merge[..12], now()).unwrap_err();
        assert!(e.contains("name it by its full sha"), "{e}");

        let orphan = clone.parent().unwrap().join("orphan");
        std::fs::create_dir_all(&orphan).unwrap();
        git_in(&orphan, &["init", "-q", "-b", "main"]);
        let e = read_main(&orphan, &merge, now()).unwrap_err();
        assert!(e.contains("ls-remote"), "{e}");
    }

    // -- the writer, end to end ---------------------------------------
    //
    // The real jobs router with the real platform bundle on a local
    // socket, and a real forge that lost a merge: what `unland_car`
    // writes is read back off the packets, not off a stub's log.

    struct AdminRoster;

    #[async_trait::async_trait]
    impl boss_jobs::owner_resolution::RosterLookup for AdminRoster {
        async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
            Ok(match role {
                "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
                _ => Vec::new(),
            })
        }
        async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
            Ok(id == "emp-bootstrap-admin")
        }
    }

    pub(crate) async fn serve() -> String {
        use boss_jobs::WorkflowRegistry;
        use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
        use std::sync::Arc;
        let kinds = Arc::new(boss_jobs::InMemoryWorkflows::new());
        for spec in boss_jobs::registry::seedable_platform_workflows() {
            kinds.seed(spec).expect("seed platform kind");
        }
        let jobs = Arc::new(boss_jobs::InMemoryJobs::new());
        let mut policy = FakePolicyClient::builder();
        for (action, resource) in [
            (Action::Create, Resource::job()),
            (Action::Read, Resource::job()),
            (Action::Update, Resource::job()),
            (Action::Update, Resource::step()),
        ] {
            policy = policy.allow("platform-admin", action, resource, Scope::All);
        }
        let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
        let state = boss_jobs::http::JobsApiState {
            kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
            roster: Some(Arc::new(AdminRoster)),
            ..boss_jobs::http::JobsApiState::minimal(
                jobs,
                bus,
                boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs"),
                policy,
                Arc::new(boss_clock_client::WallClockClient),
            )
        };
        let app = boss_jobs::http::router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// A car the conductor landed as `merge`: scope, build, gate and
    /// review completed (review with the conductor's landing note), and
    /// the `merged` marker on the job.
    pub(crate) async fn landed(door: &dyn Door, branch: &str, merge: &str) -> String {
        let made = door
            .send(
                Method::POST,
                "/api/jobs",
                Some(json!({
                    "kind": "ship-a-change",
                    "subject": {"subject_kind": "custom", "id": branch},
                    "title": "A stamp cannot land on a moved shape",
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "tags": [],
                    "metadata": {"branch": branch, "summary": "A stamp cannot land."},
                })),
            )
            .await
            .unwrap()
            .unwrap();
        let id = made["id"].as_str().unwrap().to_string();
        for (slug, md) in [
            (
                "scope",
                json!({"summary": "A stamp cannot land.", "excludes": "the UI"}),
            ),
            ("build", json!({"test": "12 tests"})),
            ("gate", json!({"gates": "auto", "verified": "green"})),
            (
                "review",
                json!({"pr_url": "x/pulls/687",
                       "note": format!("landed on main as {}", &merge[..12])}),
            ),
        ] {
            let car = read_via(door, &id).await.unwrap();
            let sid = car::find_step(&car, slug, "").unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string();
            door.send(
                Method::PATCH,
                &format!("/api/jobs/{id}/steps/{sid}/metadata"),
                Some(md),
            )
            .await
            .unwrap();
            door.send(
                Method::PUT,
                &format!("/api/jobs/{id}/steps/{sid}"),
                Some(json!({"status": "completed"})),
            )
            .await
            .unwrap();
        }
        door.send(
            Method::PATCH,
            &format!("/api/jobs/{id}/metadata"),
            Some(json!({"merged": "true", "merge_ref": &merge[..12]})),
        )
        .await
        .unwrap();
        id
    }

    /// dec3136a's repair, end to end: the verb reads main NOT carrying
    /// the merge, opens the successor at `gate`, corrects the landing
    /// note, and closes the car through `unlanded` — and a second run
    /// writes nothing new and names the same successor.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_lost_landing_is_unlanded_once_with_a_successor_at_gate() {
        let (clone, base, merge) = forge_that_lost_a_merge("unland-e2e");
        let work = clone.parent().unwrap().join("work");
        git_in(
            &work,
            &[
                "push",
                "-q",
                "-f",
                "origin",
                &format!("{base}:refs/heads/main"),
            ],
        );
        let wire = crate::steps::Wire::at(serve().await, admin_caller());
        let branch = "fix/a-stamp-cannot-land-on-a-moved-shape";
        let id = landed(&wire, branch, &merge).await;

        // By its FULL sha: this clone was made before the merge and never
        // fetched it — the conductor clone's own shape on 2026-09-25,
        // when main lost the merge inside one pass — so it is fetched by
        // sha. The car's 12-character `merge_ref` still names it.
        let done = unland_car(&wire, &clone, &id, &merge, false, now())
            .await
            .expect("unlanded");
        assert_eq!(done.car, id);
        let car_after = read_via(&wire, &id).await.unwrap();
        assert_eq!(car_after["status"], "closed", "{car_after:#}");
        assert_eq!(car_after["metadata"]["outcome"], "unlanded");
        assert_eq!(car_after["metadata"]["merged"], "true", "both facts stay");
        assert_eq!(
            car_after["metadata"]["superseded_by"],
            done.successor.as_str()
        );
        let step = car::find_step(&car_after, "unlanded", "").unwrap();
        assert!(
            step["metadata"]["evidence"]
                .as_str()
                .unwrap()
                .contains("exited 1"),
            "{step:#}"
        );
        let review = car::find_step(&car_after, "review", "").unwrap();
        assert_eq!(
            review["corrections"][0]["field"], "note",
            "the landing note is corrected beside itself: {review:#}"
        );

        let next = read_via(&wire, &done.successor).await.unwrap();
        assert_eq!(next["metadata"]["supersedes"], id.as_str());
        assert_eq!(next["metadata"]["branch"], branch);
        assert_eq!(
            car::find_step(&next, "gate", "").unwrap()["status"],
            "ready"
        );

        let again = unland_car(&wire, &clone, &id, &merge, false, now())
            .await
            .expect("a re-run answers");
        assert_eq!(again.successor, done.successor, "no second successor");
        assert_eq!(again.evidence, None, "and no second reading");
    }

    /// The verb does not take the caller's word: while main carries the
    /// merge it refuses, and nothing is written.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_landing_main_still_carries_is_refused_and_nothing_is_written() {
        let (clone, _base, merge) = forge_that_lost_a_merge("unland-carried");
        let wire = crate::steps::Wire::at(serve().await, admin_caller());
        let id = landed(&wire, "fix/still-on-main", &merge).await;
        let e = unland_car(&wire, &clone, &id, &merge[..12], false, now())
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("the landing stands"), "{e}");
        let untouched = read_via(&wire, &id).await.unwrap();
        assert_eq!(untouched["status"], "open");
        assert!(untouched["metadata"].get("superseded_by").is_none());
    }

    /// The signer the roster and the policy know.
    fn admin_caller() -> Option<crate::identity::Caller> {
        Some(crate::identity::Caller {
            id: "emp-bootstrap-admin".into(),
            source: crate::identity::Source::Env,
        })
    }
}
