//! `merge-tenant-main.sh` — a tenant merge lands through a rendered
//! plan, never through a hand typing `git push` (backlog 01d3a701).
//!
//! WHY. On 2026-09-22 two tenant branches were built, merged,
//! conflict-resolved and verified PASS by `boss tenant check`, and the
//! only way to land the merge on tenant main was an operator typing
//! `git push` — which the pod door refused, correctly, as a merge
//! without review. The door this file pins is the second caller of
//! design 17835005's shape (David, 2026-09-21: "a rendered plan hash,
//! signed with his passkey, single-use, verified before the argv is
//! built"): `plan-a-tenant-merge` renders, `merge-tenant-main` lands
//! exactly the plan whose hash it is handed and nothing else.
//!
//! WHAT A REVIEWER APPROVES is the judgement inside the merge, so the
//! plan renders every byte the merge commit holds that the automatic
//! merge of its parents does not — on 2026-09-22 both branches appended
//! a section 6 to the tail of `seeds/rules.toml`, and the resolution
//! kept both and renumbered one. That is the fixture below.
//!
//! THE FIXTURE is a forge on disk: a bare tenant repo at
//! `<forge>/david/algedonic-llc.git` — the name `infra/cluster/
//! instances.toml` gives prod, read from the real tree — and a stand-in
//! BOSS checkout whose `forgejo` remote points at `<forge>/david/
//! boss.git`, which is how the converge derives a tenant's URL
//! (`cluster-deploy-lib.sh tenant_repo_url`). `boss tenant check` is a
//! stub named by `BOSS_CLI`, the same override the converge's
//! `tenant_check` honours: the real check is pinned by
//! `an_image_sourced_tenant_passes_its_check`; what is pinned here is
//! that the plan RUNS it on the merged tree and refuses on its verdict.

use std::path::{Path, PathBuf};
use std::process::Command;

use boss_testing::{repo_root, scratch_dir};

fn script() -> PathBuf {
    repo_root().join("infra/forge/merge-tenant-main.sh")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .envs(git_env())
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Identity and isolation for every git this file runs — the gate's
/// uid has no usable HOME, and a host's global config must not change
/// what a merge produces.
fn git_env() -> Vec<(&'static str, String)> {
    vec![
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
        ("GIT_AUTHOR_NAME", "fixture".into()),
        ("GIT_AUTHOR_EMAIL", "fixture@example.invalid".into()),
        ("GIT_COMMITTER_NAME", "fixture".into()),
        ("GIT_COMMITTER_EMAIL", "fixture@example.invalid".into()),
    ]
}

fn write(p: &Path, body: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

struct Forge {
    root: PathBuf,
    bare: PathBuf,
    work: PathBuf,
    boss: PathBuf,
    cli: PathBuf,
}

const MAIN_RULES: &str = "# rules\n[[rule]]\nsection = 5\nname = \"base\"\n";

/// main; branch `ops` and branch `marketing` each append a section 6 to
/// the tail of seeds/rules.toml; `merge/ops-marketing` merges both, the
/// second merge conflicting and resolved by keeping both and
/// renumbering marketing to 7 — the 2026-09-22 merge in miniature.
fn forge() -> Forge {
    let root = scratch_dir("merge-tenant-main");
    let bare = root.join("forge/david/algedonic-llc.git");
    let work = root.join("work");
    let boss = root.join("boss");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare", "-b", "main"]);

    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    write(
        &work.join("tenant.toml"),
        "[meta]\ntenant_id = \"algedonic\"\n",
    );
    write(&work.join("seeds/rules.toml"), MAIN_RULES);
    write(&work.join("README.md"), "tenant\n");
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "tenant main"]);

    git(&work, &["checkout", "-q", "-b", "ops"]);
    write(
        &work.join("seeds/rules.toml"),
        &format!("{MAIN_RULES}[[rule]]\nsection = 6\nname = \"ops\"\n"),
    );
    git(&work, &["commit", "-q", "-am", "ops rules"]);

    git(&work, &["checkout", "-q", "-b", "marketing", "main"]);
    write(
        &work.join("seeds/rules.toml"),
        &format!("{MAIN_RULES}[[rule]]\nsection = 6\nname = \"marketing\"\n"),
    );
    git(&work, &["commit", "-q", "-am", "marketing rules"]);

    git(
        &work,
        &["checkout", "-q", "-b", "merge/ops-marketing", "main"],
    );
    git(
        &work,
        &["merge", "-q", "--no-ff", "-m", "Merge branch 'ops'", "ops"],
    );
    // The second merge conflicts on the tail; the resolution keeps both.
    let conflicted = Command::new("git")
        .arg("-C")
        .arg(&work)
        .args([
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge branch 'marketing'",
            "marketing",
        ])
        .envs(git_env())
        .output()
        .unwrap();
    assert!(
        !conflicted.status.success(),
        "the fixture's second merge must conflict"
    );
    write(
        &work.join("seeds/rules.toml"),
        &format!(
            "{MAIN_RULES}[[rule]]\nsection = 6\nname = \"ops\"\n[[rule]]\nsection = 7\nname = \"marketing\"\n"
        ),
    );
    git(&work, &["add", "seeds/rules.toml"]);
    git(&work, &["commit", "-q", "--no-edit"]);

    let bare_s = bare.to_str().unwrap().to_string();
    git(&work, &["remote", "add", "forge", &bare_s]);
    git(
        &work,
        &[
            "push",
            "-q",
            "forge",
            "main",
            "ops",
            "marketing",
            "merge/ops-marketing",
        ],
    );

    std::fs::create_dir_all(&boss).unwrap();
    git(&boss, &["init", "-q", "-b", "main"]);
    let boss_remote = format!("file://{}/forge/david/boss.git", root.display());
    git(&boss, &["remote", "add", "forgejo", &boss_remote]);

    // The check stub: PASS unless the merged tree says BROKEN, and it
    // echoes its argument so the plan's first check line is exercised.
    let cli = root.join("boss-stub");
    write(
        &cli,
        "#!/usr/bin/env bash\n\
         [ \"$1 $2\" = \"tenant check\" ] || exit 2\n\
         echo \"boss tenant check $3\"\n\
         if grep -rq BROKEN .; then echo '  INVALID  seeds/rules.toml  broken'; echo '0 ok, 0 missing, 1 invalid, 0 unknown — FAIL'; exit 1; fi\n\
         echo '  OK       seeds/rules.toml  2 rules'\n\
         echo '2 ok, 0 missing, 0 invalid, 0 unknown — PASS'\n",
    );
    Command::new("chmod").arg("+x").arg(&cli).status().unwrap();

    Forge {
        root,
        bare,
        work,
        boss,
        cli,
    }
}

/// Run the script against the fixture; (exit code, stdout, stderr).
fn run(f: &Forge, args: &[&str]) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg(script())
        .args(args)
        .envs(git_env())
        .env("BOSS_TENANT_REMOTE_OF", &f.boss)
        .env("BOSS_CLI", &f.cli)
        .output()
        .expect("the script runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn plan(f: &Forge) -> (i32, String, String) {
    run(f, &["--plan", "prod", "merge/ops-marketing"])
}

fn sha256(bytes: &str) -> String {
    let dir = scratch_dir("merge-tenant-main-hash");
    let p = dir.join("plan");
    std::fs::write(&p, bytes).unwrap();
    let out = Command::new("sha256sum").arg(&p).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn main_sha(f: &Forge) -> String {
    git(&f.bare, &["rev-parse", "refs/heads/main"])
}

#[test]
fn the_plan_names_the_shas_the_resolution_and_the_check_verdict() {
    let f = forge();
    let (code, out, err) = plan(&f);
    assert_eq!(code, 0, "plan refused:\n{err}");
    let main = main_sha(&f);
    let merge = git(&f.bare, &["rev-parse", "refs/heads/merge/ops-marketing"]);
    let tree = git(
        &f.bare,
        &["rev-parse", "refs/heads/merge/ops-marketing^{tree}"],
    );
    for needle in [
        "tenant repo: david/algedonic-llc".to_string(),
        format!("main now: {main}"),
        format!("lands as: {merge}"),
        format!("tree: {tree}"),
        "conflicted: seeds/rules.toml".to_string(),
        // The judgement itself: the renumbered section, as a line the
        // resolution added over the automatic merge's markers.
        "+section = 7".to_string(),
        "2 ok, 0 missing, 0 invalid, 0 unknown — PASS".to_string(),
        "boss tenant check .".to_string(),
        format!("--force-with-lease=refs/heads/main:{main}"),
    ] {
        assert!(out.contains(&needle), "the plan lacks `{needle}`:\n{out}");
    }
    // The clean first merge is rendered as clean, not left out.
    assert!(out.contains("clean — the automatic merge"), "{out}");
    // The seed that reaches the registries is named apart from prose.
    let seeds = out
        .split("== what reaches the live registries")
        .nth(1)
        .expect(&out);
    assert!(seeds.contains("seeds/rules.toml"), "{out}");
    // No credential-bearing URL and no scratch path in what is signed.
    assert!(
        !out.contains("file://") && !out.contains(f.root.to_str().unwrap()),
        "{out}"
    );
    // The hash is of the plan's own bytes, and is not inside them.
    assert!(
        err.contains(&format!("plan-sha256: {}", sha256(&out))),
        "{err}"
    );
}

#[test]
fn the_same_state_renders_the_same_bytes_and_a_moved_main_does_not() {
    let f = forge();
    let (_, first, _) = plan(&f);
    let (_, second, _) = plan(&f);
    assert_eq!(
        first, second,
        "two plans of one state must be byte-identical"
    );

    // main moves (the merge branch still contains the old main only):
    // the plan must refuse, because landing it is no longer a
    // fast-forward of the main the reviewer saw.
    git(&f.work, &["checkout", "-q", "main"]);
    write(&f.work.join("README.md"), "moved\n");
    git(&f.work, &["commit", "-q", "-am", "main moved"]);
    git(&f.work, &["push", "-q", "forge", "main"]);
    let (code, out, err) = plan(&f);
    assert_eq!(
        code, 78,
        "a merge that no longer contains main is not a plan:\n{out}\n{err}"
    );
    assert!(out.is_empty(), "a refusal renders no plan:\n{out}");
}

#[test]
fn a_merged_tree_that_fails_its_check_is_a_refusal_not_a_plan() {
    let f = forge();
    git(&f.work, &["checkout", "-q", "merge/ops-marketing"]);
    write(&f.work.join("seeds/rules.toml"), "BROKEN\n");
    git(&f.work, &["commit", "-q", "-am", "break it"]);
    git(&f.work, &["push", "-q", "forge", "merge/ops-marketing"]);
    let (code, out, err) = plan(&f);
    assert_eq!(code, 78, "{out}\n{err}");
    assert!(out.is_empty(), "{out}");
    assert!(
        err.contains("FAIL"),
        "the refusal names the check's verdict:\n{err}"
    );
}

/// merge-tree judges two parents, so an octopus merge's resolution
/// cannot be rendered — and an unrendered judgement is not a plan.
#[test]
fn an_octopus_merge_is_refused_because_its_resolution_cannot_be_rendered() {
    let f = forge();
    for b in ["x", "y"] {
        git(&f.work, &["checkout", "-q", "-b", b, "main"]);
        write(&f.work.join(format!("{b}.txt")), b);
        git(&f.work, &["add", "-A"]);
        git(&f.work, &["commit", "-q", "-m", b]);
    }
    git(&f.work, &["checkout", "-q", "-b", "merge/octopus", "main"]);
    // --no-ff: main is an ancestor of both, and without it git drops
    // main from the parents and records an ordinary two-parent merge.
    git(&f.work, &["merge", "-q", "--no-ff", "--no-edit", "x", "y"]);
    git(&f.work, &["push", "-q", "forge", "merge/octopus"]);
    let (code, out, err) = run(&f, &["--plan", "prod", "merge/octopus"]);
    assert_eq!(code, 78, "{out}\n{err}");
    assert!(err.contains("octopus"), "{err}");
}

#[test]
fn an_instance_without_a_tenant_repo_is_refused() {
    let f = forge();
    // The playground is image-sourced: there is no tenant main to land on.
    let (code, _, err) = run(&f, &["--plan", "playground", "merge/ops-marketing"]);
    assert_eq!(code, 78, "{err}");
    let (code, _, err) = run(&f, &["--plan", "nowhere", "merge/ops-marketing"]);
    assert_eq!(code, 78, "{err}");
}

#[test]
fn the_write_lands_only_the_plan_whose_hash_it_is_handed() {
    let f = forge();
    let (_, out, _) = plan(&f);
    let hash = sha256(&out);
    let before = main_sha(&f);
    let merge = git(&f.bare, &["rev-parse", "refs/heads/merge/ops-marketing"]);

    // A hash that is not this plan's lands nothing.
    let wrong = "0".repeat(64);
    let (code, _, err) = run(&f, &["prod", "merge/ops-marketing", &wrong]);
    assert_eq!(code, 78, "{err}");
    assert_eq!(main_sha(&f), before, "a refused write moved main");

    // The plan's own hash lands exactly the commit it names.
    let (code, stdout, err) = run(&f, &["prod", "merge/ops-marketing", &hash]);
    assert_eq!(code, 0, "{stdout}\n{err}");
    assert_eq!(
        main_sha(&f),
        merge,
        "main is not at the merge the plan named"
    );
}

#[test]
fn a_plan_approved_before_the_branch_moved_lands_nothing() {
    let f = forge();
    let (_, out, _) = plan(&f);
    let hash = sha256(&out);
    let before = main_sha(&f);
    // The branch moves after the approval: a different tree is a
    // different plan, and the old hash must not carry it.
    git(&f.work, &["checkout", "-q", "merge/ops-marketing"]);
    write(&f.work.join("README.md"), "changed after review\n");
    git(&f.work, &["commit", "-q", "-am", "after review"]);
    git(&f.work, &["push", "-q", "forge", "merge/ops-marketing"]);
    let (code, _, err) = run(&f, &["prod", "merge/ops-marketing", &hash]);
    assert_eq!(code, 78, "{err}");
    assert_eq!(main_sha(&f), before, "a stale approval moved main");
}

/// The structural property: `--plan` exits before the only line that
/// writes. Read by line, code only — the header talks about `git push`
/// at length while describing the write.
#[test]
fn the_plan_branch_exits_before_the_push() {
    let body = std::fs::read_to_string(script()).unwrap();
    let code: Vec<(usize, &str)> = body
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !t.is_empty() && !t.starts_with('#')
        })
        .collect();
    let exit = code
        .iter()
        .find(|(_, l)| l.contains("PLAN") && l.contains("exit 0"))
        .map(|(i, _)| *i)
        .expect("the --plan branch no longer exits");
    // A push that RUNS — the plan document echoes the push it
    // authorises, and an echo writes nothing.
    let push = code
        .iter()
        .find(|(_, l)| l.contains(" push ") && !l.trim_start().starts_with("echo "))
        .map(|(i, _)| *i)
        .expect("the script no longer pushes — this pin reads the wrong file");
    assert!(
        exit < push,
        "--plan exits at line {} after the push at line {}",
        exit + 1,
        push + 1
    );
}

#[test]
fn the_plan_verb_needs_no_approval_and_the_write_verb_does() {
    let read = |n: &str| -> serde_json::Value {
        let p = repo_root().join(format!("infra/ops/verbs/{n}.json"));
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
    };
    let plan = read("plan-a-tenant-merge");
    let write = read("merge-tenant-main");
    assert_ne!(
        plan["requires_approval"], true,
        "the read-only half must run today"
    );
    assert_eq!(
        write["requires_approval"], true,
        "the write must wait for a passkey"
    );
    assert_eq!(plan["argv"][0], "infra/forge/merge-tenant-main.sh");
    assert_eq!(plan["argv"][1], "--plan");
    assert_eq!(write["argv"][0], "infra/forge/merge-tenant-main.sh");
    // The write takes the plan's hash — the thing the passkey signs.
    let names: Vec<&str> = write["params"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"plan_sha256"), "{names:?}");
    // The repo is never a parameter: it is derived from the instance.
    assert!(!names.iter().any(|n| n.contains("repo")), "{names:?}");
}

/// Review F2 of car 85b7b55f (2026-09-26). boss-ops-runner runs this verb
/// as ROOT (its unit names no User=), and the tenant URL is the
/// checkout's `forgejo` remote with the repo path replaced. Until design
/// 1c90d183 that remote carried the forge token as userinfo, so root
/// authenticated by accident of the URL; once the deposit strips it, the
/// credential is a helper in the checkout OWNER's global git config and
/// root has none. So as root the script re-runs itself as the owner
/// before any git. Measured with an `id` that answers 0 to `-u` and a
/// `runuser` that records whom it was asked to run as, then runs the
/// argv: the plan still renders, byte-identical to a plain run, and the
/// drop happened once.
#[test]
fn run_as_root_it_drops_to_the_checkouts_owner_before_any_git() {
    let f = forge();
    let (_, plain, _) = plan(&f);
    let bin = f.root.join("bin");
    let log = f.root.join("runuser.log");
    std::fs::create_dir_all(&bin).unwrap();
    boss_testing::write_exec(
        &bin.join("id"),
        "#!/usr/bin/env bash\nif [ \"$*\" = -u ]; then echo 0; exit 0; fi\nPATH=\"${PATH#*:}\" exec id \"$@\"\n",
    );
    boss_testing::write_exec(
        &bin.join("runuser"),
        &format!(
            "#!/usr/bin/env bash\n[ \"$1\" = -u ] && [ \"$3\" = -- ] || {{ echo \"runuser shape: $*\" >&2; exit 97; }}\necho \"$2\" >> '{}'\nshift 3\nexec \"$@\"\n",
            log.display()
        ),
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg(script())
        .args(["--plan", "prod", "merge/ops-marketing"])
        .envs(git_env())
        .env("PATH", path)
        .env("BOSS_TENANT_REMOTE_OF", &f.boss)
        .env("BOSS_TENANT_CHECKOUT_OWNER", "someone")
        .env("BOSS_CLI", &f.cli)
        .output()
        .expect("the script runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(&log).unwrap_or_default(),
        "someone\n",
        "run as root, it re-runs ONCE as the checkout's owner: {stderr}"
    );
    assert_eq!(stdout, plain, "the owner renders the same plan root would");
}
