//! `infra/forge/forge-log.sh` — the READ-ONLY ops verb that reads the
//! Forgejo container's own server log over a bounded window, plus the
//! bare repository's reflog of `refs/heads/main` over the same window
//! (backlog 620bb69e, car 2 of design d812f1b7, D4; David 2026-09-25:
//! "I actually want you to be able to do this operationally").
//!
//! On 2026-09-25 between 20:10:41Z and 20:11:14Z something wrote the
//! forge's `refs/heads/main` back from c85941b4 to 777a5888, and the one
//! record that names who (a receive-pack line: user, source address,
//! time) is that container's log — which no door read. pod-logs reads
//! cluster pods, journal-tail reads systemd units.
//!
//! Pinned here, against a stub docker that records its argv:
//!   * the SYSTEM daemon, always: `--host unix:///var/run/docker.sock`
//!     rides the argv (sudo's env_reset would drop an exported
//!     DOCKER_HOST), and an inherited rootless DOCKER_HOST changes
//!     nothing — the wrong daemon answers "no such container" about a
//!     container that is running (forge-backup.sh's lesson)
//!   * the read is `docker logs --timestamps --since <A> --until <B>
//!     forgejo`, the container checked first with `inspect --type
//!     container`, and the header (container, window, counts) comes
//!     BEFORE any log text
//!   * query-string values and URL credentials are redacted, keys kept
//!   * the line cap is loud: the first N of M, and says so
//!   * the reflog: entries whose time falls in the window, read through
//!     the container (`exec -u git forgejo`), before the server log; no
//!     reflog kept is SAID, a missing repository is exit 4
//!   * refusals, before any docker call: a malformed or impossible time,
//!     until <= since, a window over 15 minutes, lines outside 1..2000
//!   * a missing container, a refused sudo, a failing docker logs: exit
//!     4 with the tool's own words and NOTHING on stdout — never "0
//!     lines", which is what a wrong target looks like
//!   * the verb file: read-only, forge, three bounded params, timeout 60

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

const SINCE: &str = "2026-09-25T20:10:00Z";
const UNTIL: &str = "2026-09-25T20:12:30Z";
const SYSTEM_SOCKET: &str = "unix:///var/run/docker.sock";
const REPO: &str = "/data/git/repositories/david/boss.git";

fn script() -> PathBuf {
    repo_root().join("infra/forge/forge-log.sh")
}

/// The stub docker. Appends its argv (one word per line, `=== call ===`
/// between calls) to `$STUB_LOG`, skips `--host <h>`, and answers by
/// subcommand:
///   inspect  — `$STUB_DIR/inspect.txt`, or docker's words for a missing
///              container when `$STUB_DIR/absent` exists
///   logs     — `$STUB_DIR/logs.out` on stdout and `logs.err` on stderr
///              (a container writes both), or fails with
///              `$STUB_DIR/logs.fail`'s words
///   exec     — `exec -u git <c> <cmd> <path>` runs the REAL `<cmd>` on
///              `$STUB_DIR/fs<path>`, so cat and test answer the way the
///              container's would, from a fixture tree
fn write_docker_stub(dir: &Path) -> PathBuf {
    let stub = dir.join("docker");
    boss_testing::write_exec(
        &stub,
        r#"#!/usr/bin/env bash
set -u
{ printf '%s\n' "$@"; echo '=== call ==='; } >> "$STUB_LOG"
[ "${1:-}" = "--host" ] && shift 2
sub="${1:-}"; shift
case "$sub" in
    inspect)
        if [ -e "$STUB_DIR/absent" ]; then
            echo 'Error: No such container: forgejo' >&2; exit 1
        fi
        cat "$STUB_DIR/inspect.txt" ;;
    logs)
        if [ -e "$STUB_DIR/logs.fail" ]; then
            cat "$STUB_DIR/logs.fail" >&2; exit 1
        fi
        cat "$STUB_DIR/logs.out"
        [ -e "$STUB_DIR/logs.err" ] && cat "$STUB_DIR/logs.err" >&2
        exit 0 ;;
    exec)
        [ "$1" = "-u" ] && shift 2
        shift # the container
        cmd="$1"; shift
        args=()
        for a in "$@"; do
            case "$a" in /*) args+=("$STUB_DIR/fs$a") ;; *) args+=("$a") ;; esac
        done
        exec "$cmd" "${args[@]}" ;;
    *) echo "stub docker: unexpected argv" >&2; exit 1 ;;
esac
"#,
    );
    stub
}

/// A stub `sudo` that refuses the way `sudo -n` does without a rule.
fn write_refusing_sudo(dir: &Path) -> PathBuf {
    let stub = dir.join("sudo");
    boss_testing::write_exec(
        &stub,
        "#!/usr/bin/env bash\n{ printf '%s\\n' \"$@\"; echo '=== call ==='; } >> \"$STUB_LOG\"\necho 'sudo: a password is required' >&2\nexit 1\n",
    );
    stub
}

/// running|created|started|log driver|log config — the format the script
/// asks `docker inspect` for.
const INSPECT: &str =
    "true|2026-09-20T10:00:00.123456789Z|2026-09-20T10:00:01.5Z|json-file|{\"max-size\":\"50m\"}\n";

const LOGS_OUT: &str = "\
2026-09-25T20:10:40.100000000Z 2026/09/25 20:10:40 ...eb/routing/logger.go:102:func1() [I] router: completed GET /david/boss.git/info/refs?service=git-receive-pack for 10.20.0.99:51234, 401 Unauthorized in 1.0ms
2026-09-25T20:10:41.200000000Z 2026/09/25 20:10:41 ...eb/routing/logger.go:102:func1() [I] router: completed POST /david/boss.git/git-receive-pack for 10.20.0.99:51236, 200 OK in 830.1ms @ repo/githttp.go:532(repo.ServiceReceivePack)
2026-09-25T20:10:42.000000000Z 2026/09/25 20:10:42 ...eb/routing/logger.go:102:func1() [I] router: completed GET /api/v1/repos/david/boss/pulls?state=open&token=s3cr3t-t0ken&limit=5 for 10.20.0.34:40000, 200 OK in 4.1ms
2026-09-25T20:10:43.000000000Z 2026/09/25 20:10:43 ...mirror/mirror_push.go:150:runPushSync() [I] pushing to https://dauld:ghp_mirrorpassword@github.com/dauld/boss-mirror.git
";

const LOGS_ERR: &str = "2026-09-25T20:10:44.000000000Z 2026/09/25 20:10:44 cmd/serv.go:120:fail() [E] a line the container wrote on stderr\n";

/// The reflog: one entry before the window, two inside it (20:10:41Z =
/// 1790367041, 20:11:14Z = 1790367074), one after.
fn reflog() -> String {
    let e = |s: &str| {
        let out = Command::new("date")
            .args(["-u", "-d", s, "+%s"])
            .output()
            .expect("GNU date");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    format!(
        "1111111111111111111111111111111111111111 777a5888aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa David Auld <david@example.test> {} +0000\tpush\n\
         777a5888aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa c85941b4bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb train-conductor <conductor@example.test> {} +0000\tpush\n\
         c85941b4bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb 777a5888aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa Some Pusher <pusher@example.test> {} +0000\tpush\n\
         777a5888aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 2222222222222222222222222222222222222222 train-conductor <conductor@example.test> {} +0000\tpush\n",
        e("2026-09-25T19:26:00Z"),
        e("2026-09-25T20:10:41Z"),
        e("2026-09-25T20:11:14Z"),
        e("2026-09-25T21:13:00Z"),
    )
}

#[derive(Default)]
struct Fixture {
    absent: bool,
    logs_fail: Option<&'static str>,
    logs_out: Option<String>,
    no_repo: bool,
    no_reflog: bool,
    inspect: Option<&'static str>,
    refusing_sudo: bool,
    docker_host: Option<&'static str>,
}

struct Run {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    log: String,
}

fn run(case: &str, args: &[&str], f: Fixture) -> Run {
    let dir = boss_testing::scratch_dir(&format!("forge-log-{case}"));
    let docker = write_docker_stub(&dir);
    let fixtures = dir.join("fixtures");
    boss_testing::create_dir(&fixtures);
    boss_testing::write_file(&fixtures.join("inspect.txt"), f.inspect.unwrap_or(INSPECT));
    boss_testing::write_file(
        &fixtures.join("logs.out"),
        f.logs_out.as_deref().unwrap_or(LOGS_OUT),
    );
    boss_testing::write_file(&fixtures.join("logs.err"), LOGS_ERR);
    if f.absent {
        boss_testing::write_file(&fixtures.join("absent"), "");
    }
    if let Some(words) = f.logs_fail {
        boss_testing::write_file(&fixtures.join("logs.fail"), words);
    }
    if !f.no_repo {
        let repo = fixtures.join(format!("fs{REPO}"));
        boss_testing::create_dir(&repo.join("logs/refs/heads"));
        boss_testing::write_file(&repo.join("HEAD"), "ref: refs/heads/main\n");
        if !f.no_reflog {
            boss_testing::write_file(&repo.join("logs/refs/heads/main"), &reflog());
        }
    }
    let docker_cmd = if f.refusing_sudo {
        let sudo = write_refusing_sudo(&dir);
        format!("{} -n docker", sudo.display())
    } else {
        docker.display().to_string()
    };
    let log = dir.join("argv.log");
    // The ops runner hands a verb no HOME; the script must not need one.
    let mut cmd = Command::new("bash");
    cmd.arg(script())
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_FORGE_LOG_DOCKER", docker_cmd)
        .env("STUB_LOG", &log)
        .env("STUB_DIR", &fixtures);
    if let Some(h) = f.docker_host {
        cmd.env("DOCKER_HOST", h);
    }
    let out = cmd.output().expect("bash runs");
    Run {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        log: std::fs::read_to_string(&log).unwrap_or_default(),
    }
}

/// The stub's calls, each as one argv joined by spaces.
fn calls(log: &str) -> Vec<String> {
    log.split("=== call ===\n")
        .map(|c| c.lines().collect::<Vec<_>>().join(" "))
        .filter(|c| !c.is_empty())
        .collect()
}

#[test]
fn it_reads_the_window_from_the_system_daemon_header_first() {
    let r = run("window", &[SINCE, UNTIL], Fixture::default());
    assert_eq!(r.status.code(), Some(0), "{}{}", r.stdout, r.stderr);
    let calls = calls(&r.log);
    for c in &calls {
        assert!(
            c.starts_with(&format!("--host {SYSTEM_SOCKET} ")),
            "every docker call names the SYSTEM daemon: {c}"
        );
    }
    assert!(
        calls[0].contains("inspect --type container") && calls[0].ends_with(" forgejo"),
        "the container is checked before it is read: {calls:?}"
    );
    let logs =
        format!("--host {SYSTEM_SOCKET} logs --timestamps --since {SINCE} --until {UNTIL} forgejo");
    assert!(calls.contains(&logs), "{logs} in {calls:?}");
    // The header comes first: which container, which daemon, the window.
    let first = r.stdout.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("forge-log: container forgejo on the system daemon")
            && first.contains(SYSTEM_SOCKET)
            && first.contains("running=true")
            && first.contains("log driver json-file"),
        "{first}"
    );
    assert!(
        r.stdout
            .contains(&format!("window {SINCE} .. {UNTIL} (150 s)")),
        "{}",
        r.stdout
    );
    // Both the container's streams arrive, with docker's timestamps.
    assert!(r.stdout.contains("git-receive-pack for 10.20.0.99:51236"));
    assert!(r.stdout.contains("a line the container wrote on stderr"));
    assert!(
        r.stdout
            .contains("server log: 5 line(s) in the window, all printed"),
        "{}",
        r.stdout
    );
}

#[test]
fn an_inherited_rootless_docker_host_does_not_move_the_read() {
    let r = run(
        "rootless",
        &[SINCE, UNTIL],
        Fixture {
            docker_host: Some("unix:///run/user/1000/docker.sock"),
            ..Fixture::default()
        },
    );
    assert_eq!(r.status.code(), Some(0), "{}{}", r.stdout, r.stderr);
    assert!(!r.log.contains("/run/user/1000"), "{}", r.log);
    for c in calls(&r.log) {
        assert!(c.starts_with(&format!("--host {SYSTEM_SOCKET} ")), "{c}");
    }
}

#[test]
fn query_string_values_and_url_credentials_are_redacted() {
    let r = run("redact", &[SINCE, UNTIL], Fixture::default());
    assert_eq!(r.status.code(), Some(0), "{}{}", r.stdout, r.stderr);
    for secret in ["s3cr3t-t0ken", "ghp_mirrorpassword", "dauld:"] {
        assert!(!r.stdout.contains(secret), "{secret} leaked: {}", r.stdout);
    }
    assert!(
        r.stdout
            .contains("/pulls?state=[redacted]&token=[redacted]&limit=[redacted] for 10.20.0.34"),
        "keys are kept, values go: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("info/refs?service=[redacted] for"),
        "{}",
        r.stdout
    );
    assert!(
        r.stdout
            .contains("https://[redacted]@github.com/dauld/boss-mirror.git"),
        "{}",
        r.stdout
    );
}

#[test]
fn the_reflog_entries_in_the_window_come_before_the_server_log() {
    let r = run("reflog", &[SINCE, UNTIL], Fixture::default());
    assert_eq!(r.status.code(), Some(0), "{}{}", r.stdout, r.stderr);
    let reflog_at = r
        .stdout
        .find("--- reflog of refs/heads/main")
        .expect("a reflog section");
    let log_at = r.stdout.find("--- server log").expect("a log section");
    assert!(
        reflog_at < log_at,
        "the small section rides first: {}",
        r.stdout
    );
    assert!(r.stdout[reflog_at..].contains(REPO), "{}", r.stdout);
    let section = &r.stdout[reflog_at..log_at];
    assert!(
        section.contains("2 of 4 entries in the window"),
        "{section}"
    );
    assert!(
        section.contains(
            "2026-09-25T20:11:14Z c85941b4bbbb..777a5888aaaa Some Pusher <pusher@example.test> push"
        ),
        "{section}"
    );
    assert!(
        section.contains("2026-09-25T20:10:41Z 777a5888aaaa..c85941b4bbbb train-conductor"),
        "{section}"
    );
    assert!(!section.contains("19:26:00Z") && !section.contains("21:13:00Z"));
    assert!(
        calls(&r.log).iter().any(|c| c.contains(&format!(
            "exec -u git forgejo cat {REPO}/logs/refs/heads/main"
        ))),
        "the reflog is read inside the container, as its owner: {}",
        r.log
    );
}

#[test]
fn no_reflog_kept_is_said_not_left_silent() {
    let r = run(
        "noreflog",
        &[SINCE, UNTIL],
        Fixture {
            no_reflog: true,
            ..Fixture::default()
        },
    );
    assert_eq!(r.status.code(), Some(0), "{}{}", r.stdout, r.stderr);
    assert!(
        r.stdout
            .contains(&format!("git kept no reflog for refs/heads/main in {REPO}")),
        "{}",
        r.stdout
    );
    assert!(r.stdout.contains("server log: 5 line(s)"), "{}", r.stdout);
}

#[test]
fn a_repository_not_where_the_verb_looks_is_exit_4_not_no_reflog() {
    let r = run(
        "norepo",
        &[SINCE, UNTIL],
        Fixture {
            no_repo: true,
            ..Fixture::default()
        },
    );
    assert_eq!(r.status.code(), Some(4), "{}{}", r.stdout, r.stderr);
    assert!(!r.stdout.contains("git kept no reflog"), "{}", r.stdout);
    assert!(
        r.stdout.contains("CANNOT READ") && r.stdout.contains("No such file"),
        "the reflog section carries cat's own words: {}",
        r.stdout
    );
    // The log was answered and is still printed: one missing section
    // must not hide the other.
    assert!(r.stdout.contains("git-receive-pack"), "{}", r.stdout);
}

#[test]
fn a_missing_container_is_exit_4_with_dockers_words_never_zero_lines() {
    let r = run(
        "absent",
        &[SINCE, UNTIL],
        Fixture {
            absent: true,
            ..Fixture::default()
        },
    );
    assert_eq!(r.status.code(), Some(4), "{}{}", r.stdout, r.stderr);
    assert!(r.stdout.is_empty(), "nothing on stdout: {}", r.stdout);
    assert!(
        r.stderr.contains("Error: No such container: forgejo"),
        "{}",
        r.stderr
    );
    assert!(
        !calls(&r.log).iter().any(|c| c.contains(" logs ")),
        "no read of a container that is not there: {}",
        r.log
    );
}

#[test]
fn a_failing_docker_logs_is_exit_4_with_dockers_words() {
    let r = run(
        "logsfail",
        &[SINCE, UNTIL],
        Fixture {
            logs_fail: Some(
                "Error response from daemon: configured logging driver does not support reading\n",
            ),
            ..Fixture::default()
        },
    );
    assert_eq!(r.status.code(), Some(4), "{}{}", r.stdout, r.stderr);
    assert!(r.stdout.is_empty(), "{}", r.stdout);
    assert!(
        r.stderr
            .contains("configured logging driver does not support reading"),
        "{}",
        r.stderr
    );
}

#[test]
fn a_refused_sudo_is_exit_4_and_says_it_was_sudo() {
    let r = run(
        "sudo",
        &[SINCE, UNTIL],
        Fixture {
            refusing_sudo: true,
            ..Fixture::default()
        },
    );
    assert_eq!(r.status.code(), Some(4), "{}{}", r.stdout, r.stderr);
    assert!(r.stdout.is_empty(), "{}", r.stdout);
    assert!(
        r.stderr.contains("sudo: a password is required"),
        "{}",
        r.stderr
    );
    assert!(r.stderr.contains("sudo -n refused"), "{}", r.stderr);
}

#[test]
fn the_default_reaches_the_system_daemon_through_sudo_n() {
    let src = std::fs::read_to_string(script()).unwrap();
    assert!(
        src.contains("BOSS_FORGE_LOG_DOCKER:-sudo -n docker"),
        "the default docker is `sudo -n docker`, disk-report's way in"
    );
    assert!(src.contains(&format!("\"{SYSTEM_SOCKET}\"")));
}

#[test]
fn the_line_cap_is_loud() {
    let many: String = (0..30)
        .map(|i| format!("2026-09-25T20:11:{i:02}.000000000Z line {i}\n"))
        .collect();
    let r = run(
        "cap",
        &[SINCE, UNTIL, "10"],
        Fixture {
            logs_out: Some(many),
            ..Fixture::default()
        },
    );
    assert_eq!(r.status.code(), Some(0), "{}{}", r.stdout, r.stderr);
    assert!(
        r.stdout.contains(
            "server log: 31 line(s) in the window, the FIRST 10 printed — narrow the window to read the rest"
        ),
        "{}",
        r.stdout
    );
    assert!(r.stdout.contains(" line 9\n") && !r.stdout.contains(" line 10\n"));
}

#[test]
fn an_empty_window_after_a_recreation_says_the_log_went_with_the_container() {
    let r = run(
        "recreated",
        &[SINCE, UNTIL],
        Fixture {
            logs_out: Some(String::new()),
            inspect: Some("true|2026-09-25T22:00:00.1Z|2026-09-25T22:00:01Z|json-file|{}\n"),
            ..Fixture::default()
        },
    );
    assert_eq!(r.status.code(), Some(0), "{}{}", r.stdout, r.stderr);
    // logs.err still carries one line; the point is the header.
    assert!(
        r.stdout
            .contains("created 2026-09-25T22:00:00.1Z, AFTER this window began"),
        "{}",
        r.stdout
    );
}

#[test]
fn refusals_come_before_any_docker_call() {
    for (case, args, why) in [
        ("short", vec![SINCE], "usage"),
        (
            "fmt",
            vec!["2026-09-25 20:10:00", UNTIL],
            "not an RFC 3339 UTC time",
        ),
        (
            "offset",
            vec!["2026-09-25T20:10:00+01:00", UNTIL],
            "not an RFC 3339 UTC time",
        ),
        (
            "impossible",
            vec!["2026-02-30T20:10:00Z", "2026-02-30T20:11:00Z"],
            "not a real time",
        ),
        ("order", vec![UNTIL, SINCE], "until must be after since"),
        ("equal", vec![SINCE, SINCE], "until must be after since"),
        (
            "wide",
            vec![SINCE, "2026-09-25T20:25:01Z"],
            "at most 15 minutes",
        ),
        ("zero", vec![SINCE, UNTIL, "0"], "lines must be 1..2000"),
        ("big", vec![SINCE, UNTIL, "2001"], "lines must be 1..2000"),
        ("word", vec![SINCE, UNTIL, "ten"], "lines must be a number"),
    ] {
        let r = run(case, &args, Fixture::default());
        assert_eq!(r.status.code(), Some(2), "{case}: {}{}", r.stdout, r.stderr);
        assert!(r.stderr.contains(why), "{case}: {}", r.stderr);
        assert!(r.stdout.is_empty(), "{case}: {}", r.stdout);
        assert!(r.log.is_empty(), "{case}: docker was called: {}", r.log);
    }
    // Exactly 15 minutes is inside the bound.
    let r = run("edge", &[SINCE, "2026-09-25T20:25:00Z"], Fixture::default());
    assert_eq!(r.status.code(), Some(0), "{}{}", r.stdout, r.stderr);
}

/// Whether `s` matches the verb file's `pattern` — an ERE, the way the
/// runner's jq `test()` reads it; bash's `=~` is the engine every box
/// running this suite has.
fn matches(pattern: &str, s: &str) -> bool {
    Command::new("bash")
        .args(["-c", r#"[[ "$2" =~ $1 ]]"#, "matches", pattern, s])
        .status()
        .expect("bash runs")
        .success()
}

#[test]
fn the_verb_file_is_a_read_only_forge_verb_with_bounded_params() {
    use std::os::unix::fs::PermissionsExt;
    let path = repo_root().join("infra/ops/verbs/forge-log.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["hosts"], serde_json::json!(["forge"]));
    assert_eq!(
        v["argv"],
        serde_json::json!(["infra/forge/forge-log.sh", "{1}", "{2}", "{3}"])
    );
    let about = v["about"].as_str().unwrap();
    assert!(about.starts_with("READ-ONLY"), "{about}");
    assert!(!about.contains("MUTATING"));
    for cite in ["620bb69e", "d812f1b7", "15 minutes", "2000", "redacted"] {
        assert!(about.contains(cite), "about names {cite}: {about}");
    }
    assert_eq!(v["timeout"], serde_json::json!(60));
    let params = v["params"].as_array().unwrap();
    let names: Vec<&str> = params.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["since", "until", "lines"]);
    let time = params[0]["pattern"].as_str().unwrap();
    assert_eq!(params[0]["pattern"], params[1]["pattern"]);
    assert!(matches(time, SINCE) && matches(time, UNTIL));
    for bad in [
        "2026-09-25 20:10:00",
        "2026-09-25T20:10:00+00:00",
        "2026-09-25T20:10:00.5Z",
        "-2026-09-25T20:10:00Z",
        "20m",
        "",
    ] {
        assert!(!matches(time, bad), "{bad:?} must be refused");
    }
    for p in &params[..2] {
        assert!(
            p.get("optional").is_none() && p.get("default").is_none(),
            "a window is always named: {p}"
        );
    }
    let lines = params[2]["pattern"].as_str().unwrap();
    assert!(matches(lines, "1") && matches(lines, "2000"));
    assert!(!matches(lines, "-1") && !matches(lines, "20000") && !matches(lines, "ten"));
    assert_eq!(params[2]["max"], serde_json::json!(2000));
    assert_eq!(params[2]["default"], serde_json::json!("2000"));
    let script = script();
    assert!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o111 != 0);
}
