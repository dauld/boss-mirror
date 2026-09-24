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
//! byte for byte, which only `"$(cat …)"` read at run time delivers.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SWITCH: &str = "/work/home/.config/boss/drain-on-boot";
const PROMPT: &str = "/work/boss/infra/dev/drain.md";

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

/// What the boot exec'd `claude` with, given whether the switch file
/// and the prompt file are present.
fn boot(switch_on: bool, prompt: Option<&str>) -> Vec<String> {
    let root = scratch_dir("drain-on-boot");
    let work = root.join("work");
    let rooted = |p: &str| -> PathBuf { root.join(p.trim_start_matches('/')) };
    let bin = work.join("home/.local/bin");
    create_dir(&bin);
    create_dir(&work.join("boss/infra/dev"));
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
    if let Some(p) = prompt {
        write_file(&rooted(PROMPT), p);
    }
    let script = root.join("dev-claude.sh");
    let body = dev_claude_sh().replace("/work/", &format!("{}/", work.display()));
    write_exec(&script, &body);
    let out = Command::new("bash")
        .arg(&script)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", script.display()));
    assert!(
        out.status.success(),
        "dev-claude.sh exited {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    read_argv(&work.join("home/argv"))
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
