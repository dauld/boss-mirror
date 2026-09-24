//! A dev pod boot resumes the overnight drain when its switch says so
//! (backlog ea83db08, 2026-09-24).
//!
//! The boot started `claude --remote-control boss-dev` with no task, so
//! every roll ended the drain until a human typed: the pod rolled at
//! ~10:00Z, 18:30Z, 18:52Z and 01:27Z on 2026-09-23/24, and each roll
//! killed the session, its session cron and its builders. David,
//! 2026-09-24: "We still haven't quite stayed productive all night yet."
//!
//! The standing prompt is versioned at `infra/dev/drain.md`, and the
//! boot passes it as the session's first prompt ONLY when the switch
//! file `/work/home/.config/boss/drain-on-boot` exists — it is on the
//! PVC, so David turns the drain on or off without a deploy. Without
//! the switch the boot is exactly what it was.
//!
//! Pinned by RUNNING the script the manifest writes, not by grepping
//! it: the heredoc is lifted out of `boss-dev.yaml`, its `/work/` paths
//! are moved under a scratch root, and a stub `claude` records the argv
//! it was exec'd with. So the quoting is under test too — the prompt
//! carries backticks and quotes, and it must arrive as ONE argument,
//! byte for byte, which only a prompt read at run time delivers.
//!
//! FROM ORIGIN/MAIN, not from the checkout (backlog 7a581397,
//! 2026-09-24). The roll that SHIPPED this feature (train #606) booted
//! idle, silently: the switch was on, but the main checkout was still
//! at #605 — the reclaim sidecar fast-forwards hourly — and drain.md
//! was new in #606. So the boot fetches origin and reads
//! `origin/main:infra/dev/drain.md`, falls back to the checkout's copy,
//! and when the switch is on but NO prompt can be read it says so in
//! postStart.log rather than starting idle without a word. The origin
//! cases below run real git: a checkout cloned BEFORE the commit that
//! carries the prompt, exactly the shape measured at 04:02Z.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SWITCH: &str = "/work/home/.config/boss/drain-on-boot";
const PROMPT: &str = "/work/boss/infra/dev/drain.md";
const BOOT_LOG: &str = "/work/ssh/postStart.log";

fn manifest() -> String {
    std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss-dev.yaml"))
        .expect("infra/cluster/manifests/boss-dev.yaml")
}

fn drain_prompt() -> String {
    std::fs::read_to_string(repo_root().join("infra/dev/drain.md")).expect("infra/dev/drain.md")
}

/// The body of the `cat > /work/dev-claude.sh <<'DC'` heredoc in the
/// postStart, with the YAML block's indentation taken off — what the
/// pod actually writes to /work/dev-claude.sh.
fn dev_claude_sh() -> String {
    let m = manifest();
    let lines: Vec<&str> = m.lines().collect();
    let opener = lines
        .iter()
        .position(|l| l.trim() == "cat > /work/dev-claude.sh <<'DC'")
        .expect("the postStart writes /work/dev-claude.sh through a quoted DC heredoc");
    let indent = lines[opener].len() - lines[opener].trim_start().len();
    let body: Vec<&str> = lines[opener + 1..]
        .iter()
        .take_while(|l| l.trim() != "DC")
        .map(|l| l.get(indent..).unwrap_or(""))
        .collect();
    assert!(
        lines[opener + 1 + body.len()..]
            .first()
            .is_some_and(|l| l.trim() == "DC"),
        "the dev-claude.sh heredoc has no DC terminator"
    );
    body.join("\n") + "\n"
}

/// What one boot did: the argv `claude` was exec'd with, and what the
/// boot appended to postStart.log.
struct Booted {
    argv: Vec<String>,
    log: String,
}

/// What the boot exec'd `claude` with, given whether the switch file
/// and the checkout's prompt file are present, with no git anywhere.
fn boot(switch_on: bool, prompt: Option<&str>) -> Vec<String> {
    boot_with(switch_on, prompt, None).argv
}

/// Run `git` hermetically: no system or user config, and no discovery
/// above the scratch root, so a scratch dir inside some other checkout
/// can never answer for the one under test.
fn git(root: &Path, dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=boot-test",
            "-c",
            "user.email=boot-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", root)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The boot, with the checkout's copy of the prompt as `checkout` and,
/// when `origin_ahead_with` is given, /work/boss a real clone whose
/// origin's `main` gained `infra/dev/drain.md` with that text AFTER the
/// clone — the checkout has not pulled it and its `origin/main` does
/// not know it yet, so only a fetch finds it.
fn boot_with(switch_on: bool, checkout: Option<&str>, origin_ahead_with: Option<&str>) -> Booted {
    let root = scratch_dir("drain-on-boot");
    let work = root.join("work");
    let rooted = |p: &str| -> PathBuf { root.join(p.trim_start_matches('/')) };
    let bin = work.join("home/.local/bin");
    create_dir(&bin);
    create_dir(&work.join("ssh"));
    write_exec(
        &bin.join("claude"),
        "#!/usr/bin/env bash\nprintf '%s\\0' \"$@\" > \"$HOME/argv\"\n",
    );
    if switch_on {
        let switch = rooted(SWITCH);
        create_dir(switch.parent().expect("switch has a parent"));
        // An EMPTY file: the switch is `touch`, never a value.
        write_file(&switch, "");
    }
    match origin_ahead_with {
        None => {
            create_dir(&work.join("boss/infra/dev"));
            if let Some(p) = checkout {
                write_file(&rooted(PROMPT), p);
            }
        }
        Some(ahead) => {
            let origin = root.join("origin");
            create_dir(&origin.join("infra/dev"));
            git(&root, &origin, &["init", "-q"]);
            write_file(&origin.join("README"), "base\n");
            if let Some(p) = checkout {
                write_file(&origin.join("infra/dev/drain.md"), p);
            }
            git(&root, &origin, &["add", "-A"]);
            git(&root, &origin, &["commit", "-q", "-m", "base"]);
            let to = work.join("boss");
            git(
                &root,
                &root,
                &[
                    "clone",
                    "-q",
                    &origin.display().to_string(),
                    &to.display().to_string(),
                ],
            );
            write_file(&origin.join("infra/dev/drain.md"), ahead);
            git(&root, &origin, &["add", "-A"]);
            git(
                &root,
                &origin,
                &["commit", "-q", "-m", "the train that ships the prompt"],
            );
        }
    }
    let script = root.join("dev-claude.sh");
    let body = dev_claude_sh().replace("/work/", &format!("{}/", work.display()));
    write_exec(&script, &body);
    let out = Command::new("bash")
        .arg(&script)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", &root)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", script.display()));
    assert!(
        out.status.success(),
        "dev-claude.sh exited {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    Booted {
        argv: read_argv(&work.join("home/argv")),
        log: std::fs::read_to_string(rooted(BOOT_LOG)).unwrap_or_default(),
    }
}

fn read_argv(path: &Path) -> Vec<String> {
    let raw = std::fs::read(path)
        .unwrap_or_else(|e| panic!("the stub claude never ran ({}): {e}", path.display()));
    raw.split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

#[test]
fn without_the_switch_the_boot_is_unchanged() {
    assert_eq!(
        boot(false, Some(&drain_prompt())),
        ["--remote-control", "boss-dev"],
        "no switch file means no prompt: the session starts idle, as it always did"
    );
}

#[test]
fn with_the_switch_the_drain_prompt_is_the_first_prompt_as_one_argument() {
    let prompt = drain_prompt();
    // `$(…)` drops trailing newlines and nothing else; every backtick,
    // quote and `$` in the prompt must arrive untouched.
    let expected = prompt.trim_end_matches('\n');
    assert_eq!(
        boot(true, Some(&prompt)),
        ["--remote-control", "boss-dev", expected],
        "the switch passes infra/dev/drain.md verbatim as claude's positional prompt"
    );
}

#[test]
fn the_prompt_survives_shell_metacharacters() {
    let hostile = "Run `date -u` and \"$HOME\" and $(echo no) and 'quotes'.\n";
    assert_eq!(
        boot(true, Some(hostile)),
        [
            "--remote-control",
            "boss-dev",
            hostile.trim_end_matches('\n')
        ],
        "the prompt is read at run time with \"$(cat …)\", never expanded"
    );
}

#[test]
fn a_switch_with_no_prompt_file_starts_the_session_idle_rather_than_empty() {
    assert_eq!(
        boot(true, None),
        ["--remote-control", "boss-dev"],
        "a missing drain.md must not become an empty first prompt"
    );
}

/// The measured failure (7a581397): the checkout is a train behind and
/// drain.md is new in the train that rolled the pod.
#[test]
fn the_prompt_is_read_from_origin_main_when_the_checkout_has_not_pulled_it() {
    let prompt = drain_prompt();
    let booted = boot_with(true, None, Some(&prompt));
    assert_eq!(
        booted.argv,
        [
            "--remote-control",
            "boss-dev",
            prompt.trim_end_matches('\n')
        ],
        "the boot fetches origin and reads origin/main:infra/dev/drain.md\nlog:\n{}",
        booted.log
    );
    assert!(
        booted.log.contains("dev session: drain-on-boot is ON")
            && booted.log.contains("from origin/main"),
        "postStart.log names where the prompt came from:\n{}",
        booted.log
    );
}

/// A train that EDITS drain.md must not boot the pod on the previous
/// prompt.
#[test]
fn a_newer_prompt_on_origin_main_wins_over_the_checkouts_copy() {
    let booted = boot_with(
        true,
        Some("the previous prompt\n"),
        Some("the new prompt\n"),
    );
    assert_eq!(
        booted.argv,
        ["--remote-control", "boss-dev", "the new prompt"],
        "origin/main's drain.md wins over the checkout's\nlog:\n{}",
        booted.log
    );
}

/// With no origin to read — a fetch that fails, a checkout that is not
/// a clone — the checkout's copy is still the prompt, and the log says
/// the fetch failed and which copy it used.
#[test]
fn without_origin_the_checkouts_copy_is_the_prompt_and_the_log_says_so() {
    let booted = boot_with(true, Some("the checkout's prompt\n"), None);
    assert_eq!(
        booted.argv,
        ["--remote-control", "boss-dev", "the checkout's prompt"]
    );
    assert!(
        booted.log.contains("git fetch origin failed") && booted.log.contains("from the checkout"),
        "postStart.log names the failed fetch and the fallback:\n{}",
        booted.log
    );
}

/// The switch is on and there is nothing to read: the session still
/// starts (a session is worth having), but the boot log says the drain
/// did NOT resume. Silence here is what cost the #606 roll its drain.
#[test]
fn a_switch_with_no_readable_prompt_says_so_loudly_in_the_boot_log() {
    let booted = boot_with(true, None, None);
    assert_eq!(booted.argv, ["--remote-control", "boss-dev"]);
    assert!(
        booted
            .log
            .contains("dev session: DRAIN-ON-BOOT IS ON BUT NO DRAIN PROMPT COULD BE READ"),
        "an ON switch with no prompt is loud in postStart.log:\n{}",
        booted.log
    );
}

/// Without the switch the boot neither fetches nor writes a word.
#[test]
fn without_the_switch_the_boot_does_not_touch_origin_or_the_log() {
    let booted = boot_with(false, None, Some("the new prompt\n"));
    assert_eq!(booted.argv, ["--remote-control", "boss-dev"]);
    assert_eq!(booted.log, "", "no switch, no fetch, no log line");
}

#[test]
fn the_post_start_log_says_whether_the_drain_is_on() {
    let m = manifest();
    assert!(
        m.contains("dev session: drain-on-boot is ON"),
        "postStart.log records that the boot passed the drain prompt"
    );
    assert!(
        m.contains("dev session: drain-on-boot is off"),
        "postStart.log records that the switch was absent"
    );
}

/// dev-claude.sh appends its verdict to postStart.log from inside tmux,
/// possibly while the postStart block still holds the file open. Both
/// sides must write O_APPEND, or the block's next write lands at its
/// own offset over the verdict.
#[test]
fn the_post_start_block_appends_to_its_log_so_the_boot_verdict_survives() {
    let m = manifest();
    assert!(
        m.contains("} >> /work/ssh/postStart.log 2>&1 || true"),
        "the postStart block writes its log O_APPEND"
    );
    assert!(
        !m.contains("} > /work/ssh/postStart.log"),
        "a truncating redirect on the block would overwrite dev-claude.sh's verdict"
    );
    // Appending alone would grow the log across every boot, so each
    // boot's log must still start empty — truncated once, before the
    // block opens it.
    assert!(
        m.contains("mkdir -p /work/ssh && : > /work/ssh/postStart.log"),
        "the postStart truncates its log once before the block appends to it"
    );
}

/// The prompt is read by a model that starts cold after a roll, so it
/// names the verbs it must run. A leading `-` would be parsed by the
/// CLI as an option rather than as the prompt.
#[test]
fn the_drain_prompt_names_its_verbs_and_cannot_be_read_as_a_flag() {
    let p = drain_prompt();
    assert!(!p.trim_start().is_empty(), "infra/dev/drain.md is empty");
    assert!(
        !p.starts_with('-'),
        "drain.md must not begin with '-': claude would read it as a flag"
    );
    for needle in [
        "date -u",
        "boss orient",
        "boss dispatch",
        "--report",
        "boss triage",
        "boss gate",
        "boss rerail",
        "/loop",
        "drain-on-boot",
    ] {
        assert!(p.contains(needle), "drain.md no longer names `{needle}`");
    }
}
