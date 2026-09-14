//! `infra/dev/boss-api` — the jobs-API door CLAUDE.md §Doors names:
//! `boss-api METHOD /api/path [body.json]`, pinned to the system of
//! record, signed as the session's actor, invoked bare so it stays
//! inside a permission allowlist. Body to stdout, `HTTP:<code>` to
//! stderr, non-2xx exits 1 so a driver fails loudly.
//!
//! Until 2026-09-14 this script was pod-local text under
//! /work/tools/bin (backlog 0d8d7a90): a fresh pod, a second builder
//! host, or a builder pod would not have had it, and the base URL and
//! actor-header logic were unversioned. The pod file is the spec —
//! these tests pin its behaviour against a stub `curl` on PATH so
//! nothing here reaches a network:
//!
//!   * a GET prints the body and the `HTTP:` line, and asks curl for
//!     `$BOSS_JOBS_URL$path` with the method;
//!   * a write method sends `--data-binary @<body file>`, and a non-2xx
//!     answer exits 1 with the code still on stderr;
//!   * the `X-Boss-User` header carries `BOSS_ACTOR`, else the one line
//!     in `$HOME/.config/boss/actor`; with neither, a READ goes out
//!     marked `operator:unidentified` and a WRITE is refused before
//!     curl runs, naming both fixes — the CLI's rule
//!     (crates/orchestrators/boss-cli/src/identity.rs, backlog
//!     5083d6f5). Until 2026-09-14 the script signed an unnamed write
//!     as the pod copy's fixed id instead (backlog 416d503c);
//!   * the default base URL is the one line in `infra/dev/sor-url`,
//!     read from beside the script, so the system-of-record address is
//!     spelled once in infra/dev (CLAUDE.md §9a);
//!   * the machine token rides as `X-Boss-Machine-Token` only when its
//!     file exists (the stub records the header NAME, never a value);
//!   * a bad method or a missing path is refused with exit 2 before
//!     curl runs.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/dev/boss-api";

/// The fixed actor id the pod copy used to sign an unnamed call with.
/// A refusal names it, so a reader can tell "nobody is named" apart
/// from a policy denial — the same reason the CLI's refusal names the
/// conductor.
const FORMER_DEFAULT_ACTOR: &str = "claude@algedonic.dev";

/// The id an unnamed READ carries — `identity::UNIDENTIFIED` in the
/// CLI. Not an `automation:` slug: an unidentified operator is not a
/// process, and the server's automation branch must not read it as one.
const UNIDENTIFIED: &str = "operator:unidentified";

/// The one-line file beside the script that spells the system of record.
const SOR_URL_FILE: &str = "infra/dev/sor-url";

struct Fixture {
    root: PathBuf,
    bin: PathBuf,
    /// A HOME the test owns, so the actor-file leg never reads the
    /// real `~/.config/boss/actor`.
    home: PathBuf,
    /// Written by the `curl` stub: one argv element per line, with the
    /// machine-token header's VALUE replaced by `<present>`.
    argv: PathBuf,
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("boss-api-{name}"));
        let bin = root.join("bin");
        create_dir(&bin);
        let home = root.join("home");
        create_dir(&home);
        let argv = root.join("curl-argv.txt");
        // curl: record what the script asked for, then answer the way
        // the real one does under `-w '\n%{http_code}'` — the body,
        // a newline, the code. STUB_BODY / STUB_CODE choose the answer.
        // A header whose name is the machine token is recorded by name
        // only: the test asserts the header is PRESENT, never its value.
        write_exec(
            &bin.join("curl"),
            "#!/usr/bin/env bash\n\
             : > \"$STUB_ARGV\"\n\
             for a in \"$@\"; do\n\
                 case \"$a\" in\n\
                     X-Boss-Machine-Token:*) echo 'X-Boss-Machine-Token: <present>' ;;\n\
                     *) printf '%s\\n' \"$a\" ;;\n\
                 esac >> \"$STUB_ARGV\"\n\
             done\n\
             printf '%s\\n%s' \"${STUB_BODY:-}\" \"${STUB_CODE:-200}\"\n",
        );
        Self {
            root,
            bin,
            home,
            argv,
        }
    }

    /// The tree's script, with `BOSS_JOBS_URL` pinned to a test address.
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let mut cmd = self.command(&repo_root().join(SCRIPT));
        cmd.env("BOSS_JOBS_URL", "http://sor.test:7900");
        Self::finish(cmd, args, env)
    }

    /// A `Command` for `script` with the fixture's isolation — the stub
    /// PATH, an owned HOME, no token file, no actor from the caller's
    /// shell — and NO `BOSS_JOBS_URL`, so a test can watch the default.
    fn command(&self, script: &Path) -> Command {
        let mut cmd = Command::new(script);
        cmd.current_dir(&self.root)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("HOME", &self.home)
            .env("STUB_ARGV", &self.argv)
            .env_remove("BOSS_JOBS_URL")
            // A token file that does not exist, so the pod's real
            // /etc/boss/machine-token is never read by a test.
            .env(
                "BOSS_MACHINE_TOKEN_FILE",
                self.root.join("no-such-token-file"),
            )
            .env_remove("BOSS_ACTOR")
            .env_remove("BOSS_ACTOR_FILE")
            .env_remove("STUB_BODY")
            .env_remove("STUB_CODE");
        cmd
    }

    fn finish(mut cmd: Command, args: &[&str], env: &[(&str, &str)]) -> Run {
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run boss-api");
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Forget the previous run's argv, so "curl must not run" after a
    /// run that DID reach curl asserts on this run, not the last one.
    fn clear_argv(&self) {
        let _ = std::fs::remove_file(&self.argv);
    }

    fn curl_argv(&self) -> Vec<String> {
        std::fs::read_to_string(&self.argv)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The value of a `-H` header the stub recorded, by name.
    fn header(&self, name: &str) -> Option<String> {
        let prefix = format!("{name}: ");
        self.curl_argv()
            .into_iter()
            .find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
    }
}

#[test]
fn the_script_is_in_the_tree_and_executable() {
    use std::os::unix::fs::PermissionsExt;
    let path = repo_root().join(SCRIPT);
    let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "{SCRIPT} must be executable: the pod's /work/tools/bin/boss-api is a symlink to it"
    );
    let text = std::fs::read_to_string(&path).expect("read boss-api");
    assert!(
        !text.contains("svc.cluster.local:7900") && !text.contains("10.20.0.34:7900"),
        "the SoR address is read from {SOR_URL_FILE}, not spelled in the script (CLAUDE.md 9a)"
    );
    assert!(
        !text.contains("cat /etc/boss/machine-token"),
        "the token path is a default behind BOSS_MACHINE_TOKEN_FILE, not a literal read"
    );
}

/// The read every session makes: body to stdout, code to stderr,
/// exit 0, and curl asked for the pinned base URL plus the path.
#[test]
fn a_get_prints_the_body_and_the_http_line() {
    let f = Fixture::new("get");
    let r = f.run(
        &["GET", "/api/jobs?kind=pr-train"],
        &[
            ("STUB_BODY", r#"{"total":3}"#),
            ("BOSS_ACTOR", "emp-reader"),
        ],
    );
    assert_eq!(r.code, 0, "a 2xx exits 0: {}", r.stderr);
    // The newline is the `-w '\n%{http_code}'` separator, which the pod
    // copy leaves on the body; `boss-api GET … > file` has always
    // written it, so it stays.
    assert_eq!(
        r.stdout, "{\"total\":3}\n",
        "the body (and the separator newline) goes to stdout, nothing else"
    );
    assert_eq!(
        r.stderr, "HTTP:200\n",
        "the code goes to stderr as one HTTP: line"
    );

    let argv = f.curl_argv();
    let method = argv
        .iter()
        .position(|a| a == "-X")
        .map(|i| argv[i + 1].as_str());
    assert_eq!(method, Some("GET"), "curl -X carries the method: {argv:?}");
    assert_eq!(
        argv.last().map(String::as_str),
        Some("http://sor.test:7900/api/jobs?kind=pr-train"),
        "the URL is BOSS_JOBS_URL plus the path: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--data-binary"),
        "a GET sends no body: {argv:?}"
    );
    assert_eq!(
        f.header("Content-Type").as_deref(),
        Some("application/json"),
        "{argv:?}"
    );
    assert_eq!(
        f.header("X-Boss-Machine-Token"),
        None,
        "no token file, no token header: {argv:?}"
    );
}

/// A write: the body file rides as `--data-binary @file`, and a
/// non-2xx answer keeps the body and the code but exits 1 — the
/// property drivers rely on to fail loudly.
#[test]
fn a_write_sends_the_body_file_and_a_non_2xx_exits_1() {
    let f = Fixture::new("write");
    let body = f.root.join("body.json");
    write_file(&body, r#"{"note":"hello"}"#);
    let body_arg = format!("@{}", body.display());

    let r = f.run(
        &[
            "PATCH",
            "/api/jobs/abc/metadata",
            body.to_str().expect("utf8"),
        ],
        &[
            ("STUB_BODY", r#"{"ok":true}"#),
            ("BOSS_ACTOR", "emp-writer"),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.stdout, "{\"ok\":true}\n");
    let argv = f.curl_argv();
    let data = argv
        .iter()
        .position(|a| a == "--data-binary")
        .map(|i| argv[i + 1].as_str());
    assert_eq!(data, Some(body_arg.as_str()), "{argv:?}");
    assert!(argv.iter().any(|a| a == "PATCH"), "{argv:?}");

    let r = f.run(
        &["PUT", "/api/jobs/abc", body.to_str().expect("utf8")],
        &[
            ("STUB_BODY", r#"{"error":"conflict"}"#),
            ("STUB_CODE", "409"),
            ("BOSS_ACTOR", "emp-writer"),
        ],
    );
    assert_eq!(r.code, 1, "a non-2xx exits 1: {}", r.stderr);
    assert_eq!(
        r.stdout, "{\"error\":\"conflict\"}\n",
        "the error body still reaches stdout"
    );
    assert_eq!(r.stderr, "HTTP:409\n");
}

/// Who the call signs as, in the order the `boss` CLI uses
/// (crates/orchestrators/boss-cli/src/identity.rs): `BOSS_ACTOR`,
/// else the one line in `$HOME/.config/boss/actor`, else — for a READ,
/// which attributes nothing — the unidentified marker, said once on
/// stderr. Never the pod copy's fixed id: that signed a fresh session's
/// history as the operator's agent (backlog 416d503c).
#[test]
fn the_actor_header_comes_from_env_then_the_actor_file_and_an_unnamed_read_is_marked() {
    let f = Fixture::new("actor");

    let r = f.run(&["GET", "/api/jobs"], &[]);
    assert_eq!(r.code, 0, "an unnamed read still goes out: {}", r.stderr);
    let user = f.header("X-Boss-User").expect("X-Boss-User header");
    assert!(
        user.contains(&format!(r#""id":"{UNIDENTIFIED}""#)),
        "unnamed, a read is marked unidentified: {user}"
    );
    assert!(
        !user.contains(FORMER_DEFAULT_ACTOR),
        "the pod copy's fixed id must never ride unasked: {user}"
    );
    assert!(
        user.contains(r#""role":"platform-admin""#) && user.contains(r#""access_tier":"operator""#),
        "the role and tier the pod copy sent are kept: {user}"
    );
    assert!(
        r.stderr.contains(UNIDENTIFIED) && r.stderr.contains("BOSS_ACTOR"),
        "the read says on stderr that nobody is named and how to fix it: {}",
        r.stderr
    );
    assert!(
        r.stderr.ends_with("HTTP:200\n"),
        "the HTTP line is still the last thing on stderr: {}",
        r.stderr
    );

    let cfg = f.home.join(".config/boss");
    create_dir(&cfg);
    write_file(&cfg.join("actor"), "emp-from-file\n");
    let r = f.run(&["GET", "/api/jobs"], &[]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let user = f.header("X-Boss-User").expect("X-Boss-User header");
    assert!(
        user.contains(r#""id":"emp-from-file""#),
        "the actor file's one line names the actor, trailing newline dropped: {user}"
    );
    assert_eq!(r.stderr, "HTTP:200\n", "a named read says nothing extra");

    let r = f.run(&["GET", "/api/jobs"], &[("BOSS_ACTOR", "emp-from-env")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let user = f.header("X-Boss-User").expect("X-Boss-User header");
    assert!(
        user.contains(r#""id":"emp-from-env""#),
        "BOSS_ACTOR wins over the file: {user}"
    );

    // A blank env falls THROUGH to the file rather than shadowing it —
    // `export BOSS_ACTOR=` is a misconfiguration, not the empty actor.
    let r = f.run(&["GET", "/api/jobs"], &[("BOSS_ACTOR", "  ")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let user = f.header("X-Boss-User").expect("X-Boss-User header");
    assert!(
        user.contains(r#""id":"emp-from-file""#),
        "blank is not an answer; the file still names the actor: {user}"
    );
}

/// The claim of backlog 416d503c: an unnamed WRITE is refused before
/// curl runs — exit 2, the CLI's sentence naming both fixes and the
/// identity it refused to use — for every method that records an actor.
#[test]
fn an_unnamed_write_is_refused_before_curl_runs_and_names_both_fixes() {
    let f = Fixture::new("unnamed-write");
    let body = f.root.join("body.json");
    write_file(&body, r#"{"status":"completed"}"#);

    let r = f.run(
        &[
            "PUT",
            "/api/jobs/abc/steps/s1",
            body.to_str().expect("utf8"),
        ],
        &[],
    );
    assert_eq!(
        r.code, 2,
        "an unnamed write is a refusal, not a request: {}",
        r.stderr
    );
    assert!(r.stdout.is_empty(), "nothing on stdout: {}", r.stdout);
    assert!(f.curl_argv().is_empty(), "curl must not run");
    assert!(
        r.stderr
            .contains("refusing to sign PUT /api/jobs/abc/steps/s1"),
        "{}",
        r.stderr
    );
    assert!(
        r.stderr
            .contains("nothing names the actor running this command"),
        "the CLI's sentence, mirrored: {}",
        r.stderr
    );
    assert!(
        r.stderr.contains("export BOSS_ACTOR=")
            && r.stderr
                .contains(&f.home.join(".config/boss/actor").display().to_string()),
        "both fixes are named, with the file's location on THIS machine: {}",
        r.stderr
    );
    assert!(
        r.stderr.contains(FORMER_DEFAULT_ACTOR),
        "it names the identity it REFUSED to use, so this reads apart from a policy denial: {}",
        r.stderr
    );
    assert!(
        !r.stderr.contains("HTTP:"),
        "no HTTP line: no request was made: {}",
        r.stderr
    );

    for method in ["POST", "PATCH", "DELETE"] {
        let r = f.run(&[method, "/api/jobs/abc"], &[]);
        assert_eq!(r.code, 2, "{method} records an actor: {}", r.stderr);
        assert!(f.curl_argv().is_empty(), "{method}: curl must not run");
    }

    // A whitespace-only BOSS_ACTOR is not a name either.
    let r = f.run(&["POST", "/api/jobs"], &[("BOSS_ACTOR", " ")]);
    assert_eq!(r.code, 2, "blank is not an answer: {}", r.stderr);
    assert!(f.curl_argv().is_empty(), "curl must not run");

    // Named, the same write goes out.
    let r = f.run(
        &[
            "PUT",
            "/api/jobs/abc/steps/s1",
            body.to_str().expect("utf8"),
        ],
        &[("BOSS_ACTOR", "emp-writer")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        f.header("X-Boss-User")
            .is_some_and(|u| u.contains(r#""id":"emp-writer""#)),
        "{:?}",
        f.curl_argv()
    );
}

/// The system-of-record address is spelled ONCE in infra/dev: the one
/// line in `infra/dev/sor-url`, which the script reads from beside
/// itself when `BOSS_JOBS_URL` is unset. Until 2026-09-14 this script
/// and the boss shim each carried their own spelling of the same
/// deployment (backlog 416d503c, CLAUDE.md 9a).
#[test]
fn the_default_url_is_the_one_line_in_sor_url() {
    let sor_url_path = repo_root().join(SOR_URL_FILE);
    let text = std::fs::read_to_string(&sor_url_path)
        .unwrap_or_else(|e| panic!("{}: {e}", sor_url_path.display()));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{SOR_URL_FILE} is one line: {text:?}");
    let url = lines[0].trim();
    assert!(
        url.starts_with("http://") || url.starts_with("https://"),
        "{SOR_URL_FILE} holds a URL: {url:?}"
    );
    assert!(
        !url.ends_with('/'),
        "no trailing slash — the script appends /api/...: {url:?}"
    );

    let f = Fixture::new("sor-url");
    let r = Fixture::finish(
        f.command(&repo_root().join(SCRIPT)),
        &["GET", "/api/yard/status"],
        &[("BOSS_ACTOR", "emp-reader")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let argv = f.curl_argv();
    assert_eq!(
        argv.last().map(String::as_str),
        Some(format!("{url}/api/yard/status").as_str()),
        "with BOSS_JOBS_URL unset the file's line is the base: {argv:?}"
    );

    // BOSS_JOBS_URL still wins — the conductor's unit and every test
    // set it explicitly.
    let r = f.run(
        &["GET", "/api/yard/status"],
        &[("BOSS_ACTOR", "emp-reader")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some("http://sor.test:7900/api/yard/status")
    );

    // A copy of the script with no sor-url beside it names no system
    // of record: refuse (exit 2) and say which file is missing, rather
    // than asking curl for a relative path. A wrong target answers
    // instead of erroring (CLAUDE.md §Doors); a missing one must not.
    let orphan = f.root.join("orphan");
    create_dir(&orphan);
    let script_text = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("read boss-api");
    write_exec(&orphan.join("boss-api"), &script_text);
    f.clear_argv();
    let r = Fixture::finish(
        f.command(&orphan.join("boss-api")),
        &["GET", "/api/yard/status"],
        &[("BOSS_ACTOR", "emp-reader")],
    );
    assert_eq!(r.code, 2, "no file, no default, no request: {}", r.stderr);
    assert!(f.curl_argv().is_empty(), "curl must not run");
    assert!(
        r.stderr.contains("sor-url") && r.stderr.contains("BOSS_JOBS_URL"),
        "the refusal names the file and the override: {}",
        r.stderr
    );
}

/// The machine token rides only when its file exists, and the file's
/// location is a default behind `BOSS_MACHINE_TOKEN_FILE`. The stub
/// records the header's presence, not its value.
#[test]
fn the_machine_token_header_rides_only_when_its_file_exists() {
    let f = Fixture::new("token");
    let token_file = f.root.join("machine-token");
    write_file(&token_file, "stub-token-for-the-test\n");

    let r = f.run(
        &["GET", "/api/jobs"],
        &[
            (
                "BOSS_MACHINE_TOKEN_FILE",
                token_file.to_str().expect("utf8"),
            ),
            ("BOSS_ACTOR", "emp-reader"),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.header("X-Boss-Machine-Token").as_deref(),
        Some("<present>"),
        "the token header must ride when the file exists: {:?}",
        f.curl_argv()
    );
}

/// Refusals happen before curl: a method outside the five, or a
/// missing path, is usage (exit 2), and nothing is sent.
#[test]
fn a_bad_method_or_a_missing_path_is_refused_before_curl_runs() {
    let f = Fixture::new("usage");

    let r = f.run(&["FETCH", "/api/jobs"], &[]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    assert!(r.stderr.contains("usage:"), "{}", r.stderr);
    assert!(f.curl_argv().is_empty(), "curl must not run");

    let r = f.run(&["GET"], &[]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    assert!(r.stderr.contains("usage:"), "{}", r.stderr);
    assert!(f.curl_argv().is_empty(), "curl must not run");
}
