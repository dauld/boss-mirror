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
//!     curl runs;
//!   * the PORT follows the path (backlog de0989d2, 2026-09-17): the
//!     system of record is several services on one LAN IP, and until
//!     this the door sent every path to the jobs port, so
//!     `POST /api/people/accounts` — the first real step of the first
//!     real sponsorship loop — could not be done through any door. The
//!     rules are the ONE route function both doors source
//!     (`infra/forge/probe-bin/sor-routes.sh`); the `name=port` table is
//!     `BOSS_SOR_PORTS`, else `infra/forge/sor-ports.env` read from
//!     beside the script; absent both, nothing is routed. A table that
//!     lacks the routed service is refused before curl;
//!   * `BOSS_SOR_SERVICE` names the service when no path can route to
//!     it (backlog bf1f5ad2, 2026-09-19) — the gateway fronts every
//!     path, so it is reached by name, the way the forge's
//!     `boss-gateway-read` reaches it. Without this a builder
//!     rehearsing a gateway probe hand-set the host and the port table
//!     and was not using the door at all;
//!   * a rollout is waited out (backlog 034002b3, 2026-09-23): a curl
//!     that never got its request out (exit 6 or 7) is re-sent for up
//!     to `BOSS_SOR_WAIT_SECONDS` (120) with a visible line per wait,
//!     then fails naming the elapsed time; every other exit and every
//!     HTTP status is surfaced on the first attempt.

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
            // The port table comes from the tree's file unless a test
            // sets it — never from the caller's shell. Nor does the
            // named-service override leak in from the shell that ran
            // the suite.
            .env_remove("BOSS_SOR_PORTS")
            .env_remove("BOSS_SOR_SERVICE")
            // Nor the roll wait's window (backlog 034002b3).
            .env_remove("BOSS_SOR_WAIT_SECONDS")
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
    // Backlog d843abf2 (measured 2026-09-18): nobody-in-particular
    // reads under the platform's own READ role, not the operator's —
    // the same rule as the CLI's identity.rs. The role's one home is
    // boss_core::roles; a shell script cannot import it, so this pins
    // the copy (CLAUDE.md §9a).
    let reader_role = boss_core::roles::AUDIT_READONLY_ROLE;
    assert!(
        user.contains(&format!(r#""role":"{reader_role}""#))
            && user.contains(r#""access_tier":"auditor""#),
        "an unnamed read carries {reader_role} at the auditor tier: {user}"
    );
    assert!(
        !user.contains("platform-admin"),
        "an unnamed read must not carry the operator's role: {user}"
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
    assert!(
        user.contains(r#""role":"platform-admin""#) && user.contains(r#""access_tier":"operator""#),
        "a NAMED caller keeps the operator's role and tier: {user}"
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

// =====================================================================
// THE PORT FOLLOWS THE PATH (backlog de0989d2, measured 2026-09-17
// 14:50Z). The reconcile step of the first real sponsorship needs an
// account created through POST /api/people/accounts, which
// boss-accounts serves on 7550; this door sent it to the jobs port,
// where it is a 404 about a surface that exists. The rules are the one
// route function the forge's probe reader also sources; the port table
// is BOSS_SOR_PORTS, else infra/forge/sor-ports.env beside the tree.
// =====================================================================

/// The measured case: a write to an accounts path leaves on the
/// accounts port, on the same host, with the body and the actor intact.
#[test]
fn a_post_to_an_accounts_path_routes_to_the_accounts_port() {
    let f = Fixture::new("route-accounts");
    let body = f.root.join("account.json");
    write_file(&body, r#"{"name":"sponsor"}"#);
    let r = f.run(
        &["POST", "/api/people/accounts", body.to_str().expect("utf8")],
        &[
            ("STUB_BODY", r#"{"id":"acct-1"}"#),
            ("BOSS_ACTOR", "agent-x"),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let argv = f.curl_argv();
    assert_eq!(
        argv.last().map(String::as_str),
        Some(
            format!(
                "http://sor.test:{}/api/people/accounts",
                boss_ports::prod("accounts")
            )
            .as_str()
        ),
        "the host is kept and the port is boss-accounts': {argv:?}"
    );
    assert!(
        argv.iter().any(|a| a == "--data-binary"),
        "the body still rides: {argv:?}"
    );
    assert!(
        f.header("X-Boss-User")
            .is_some_and(|u| u.contains(r#""id":"agent-x""#)),
        "the routed write is still signed: {argv:?}"
    );

    // A read of the same surface, with a query string, goes the same way.
    let r = f.run(
        &["GET", "/api/people/accounts?limit=1"],
        &[("BOSS_ACTOR", "agent-x")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some(
            format!(
                "http://sor.test:{}/api/people/accounts?limit=1",
                boss_ports::prod("accounts")
            )
            .as_str()
        )
    );
}

/// The ledger and the dispatcher joined the door on 2026-09-17
/// (backlog 77fd7b5a + 4145d2c1): a journal entry posts to boss-ledger
/// on its port, and the rule registry reads from the dispatcher on
/// its own — neither is the jobs port any longer.
#[test]
fn a_ledger_path_and_a_dispatcher_path_route_to_their_own_ports() {
    let f = Fixture::new("route-ledger-dispatcher");
    for (path, service) in [
        ("/api/ledger/journal-entries", "ledger"),
        ("/api/dispatcher/rules", "dispatcher"),
    ] {
        let r = f.run(&["GET", path], &[("BOSS_ACTOR", "agent-x")]);
        assert_eq!(r.code, 0, "{path}: {}", r.stderr);
        assert_eq!(
            f.curl_argv().last().map(String::as_str),
            Some(format!("http://sor.test:{}{path}", boss_ports::prod(service)).as_str()),
            "{path} leaves on {service}'s port, host kept"
        );
    }
}

/// The jobs API stays where it was, and so does anything the table
/// does not name — including a prefix that merely resembles a routed
/// one. Routing adds ports; it never moves the base.
#[test]
fn the_jobs_api_and_an_unknown_prefix_stay_on_the_base() {
    let f = Fixture::new("route-default");
    for path in [
        "/api/jobs?kind=pr-train",
        "/api/yard/status",
        "/api/peoples/x",
    ] {
        let r = f.run(&["GET", path], &[("BOSS_ACTOR", "agent-x")]);
        assert_eq!(r.code, 0, "{path}: {}", r.stderr);
        assert_eq!(
            f.curl_argv().last().map(String::as_str),
            Some(format!("http://sor.test:7900{path}").as_str()),
            "{path} is the jobs API, on the base as given"
        );
    }
}

/// A table that exists but lacks the routed service is a defect in the
/// table, not a reason to send the request to the jobs port: refuse
/// (exit 2), name the service and the table, and never reach curl.
/// `BOSS_SOR_PORTS` wins over the file when set, which is also how a
/// test hands the door a table of its choosing.
#[test]
fn a_table_that_lacks_the_service_is_refused_before_curl_and_the_env_table_wins() {
    let f = Fixture::new("route-missing");
    let body = f.root.join("account.json");
    write_file(&body, r#"{"name":"sponsor"}"#);
    let r = f.run(
        &["POST", "/api/people/accounts", body.to_str().expect("utf8")],
        &[
            ("BOSS_ACTOR", "agent-x"),
            ("BOSS_SOR_PORTS", "jobs=7900 people=7500"),
        ],
    );
    assert_eq!(
        r.code, 2,
        "a table without the service is a refusal: {}",
        r.stderr
    );
    assert!(f.curl_argv().is_empty(), "curl must not run");
    assert!(r.stdout.is_empty(), "nothing on stdout: {}", r.stdout);
    assert!(
        r.stderr.contains("accounts") && r.stderr.contains("jobs=7900 people=7500"),
        "the refusal names the missing service and the table it read: {}",
        r.stderr
    );
    assert!(
        !r.stderr.contains("HTTP:"),
        "no request was made: {}",
        r.stderr
    );

    // The env table, when set, is the table — the file beside the tree
    // is not consulted.
    let r = f.run(
        &["GET", "/api/people/accounts"],
        &[
            ("BOSS_ACTOR", "agent-x"),
            ("BOSS_SOR_PORTS", "jobs=7900 accounts=9999"),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some("http://sor.test:9999/api/people/accounts")
    );
}

// =====================================================================
// THE ONE SERVICE NO PATH ROUTES TO (backlog bf1f5ad2, 2026-09-19).
// The gateway fronts every path there is, so no prefix can route to it
// — the machine door carries it as a row a reader reaches BY NAME
// (the_machine_door_carries_every_read_surface.rs, NOT_PATH_ROUTED).
// The forge has such a reader, `boss-gateway-read`; the pod had none,
// so a builder rehearsing a gateway probe set the host AND the port
// table by hand and the door was not the door. `BOSS_SOR_SERVICE` is
// that name on this door: the path decides the service unless a name
// is given, and then the name decides it.
// =====================================================================

/// Named, the read leaves on the gateway's port with the host kept —
/// and the SAME path with no name is the jobs API, which is what makes
/// the name (not the path) the thing that routed it.
#[test]
fn a_named_service_sends_the_read_to_that_services_port() {
    let f = Fixture::new("route-named-service");

    let r = f.run(
        &["GET", "/health"],
        &[("BOSS_ACTOR", "agent-x"), ("BOSS_SOR_SERVICE", "gateway")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some(format!("http://sor.test:{}/health", boss_ports::prod("gateway")).as_str()),
        "the named service decides the port; the host is still the system of record's"
    );

    let r = f.run(&["GET", "/health"], &[("BOSS_ACTOR", "agent-x")]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some("http://sor.test:7900/health"),
        "unnamed, /health is not a routed prefix and stays on the base"
    );

    // Blank is not a name: it falls through to the path, exactly as a
    // blank BOSS_ACTOR falls through to the file.
    let r = f.run(
        &["GET", "/api/people/accounts"],
        &[("BOSS_ACTOR", "agent-x"), ("BOSS_SOR_SERVICE", "  ")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some(
            format!(
                "http://sor.test:{}/api/people/accounts",
                boss_ports::prod("accounts")
            )
            .as_str()
        ),
        "a blank name is not an answer; the path still routes"
    );
}

/// A name the table cannot place, or a name with no table at all, is
/// refused before curl — never sent to the jobs port. Reading the jobs
/// port under a gateway question answers 200 with a narrowed world
/// (the defect boss-gateway-read refuses for the same reason), and a
/// wrong target answers instead of erroring.
#[test]
fn a_named_service_the_table_cannot_place_is_refused_before_curl() {
    let f = Fixture::new("route-named-missing");

    let r = f.run(
        &["GET", "/health"],
        &[
            ("BOSS_ACTOR", "agent-x"),
            ("BOSS_SOR_SERVICE", "gateway"),
            ("BOSS_SOR_PORTS", "jobs=7900 people=7500"),
        ],
    );
    assert_eq!(
        r.code, 2,
        "a table without the service refuses: {}",
        r.stderr
    );
    assert!(f.curl_argv().is_empty(), "curl must not run");
    assert!(
        r.stderr.contains("gateway") && r.stderr.contains("jobs=7900 people=7500"),
        "the refusal names the service and the table it read: {}",
        r.stderr
    );
    assert!(!r.stderr.contains("HTTP:"), "no request: {}", r.stderr);

    // No table at all is the same refusal, not the silent fall-through
    // an unnamed path gets: a name that cannot be placed must never
    // leave on the base.
    let lone = f.root.join("lone-named");
    create_dir(&lone);
    let script_text = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("read boss-api");
    write_exec(&lone.join("boss-api"), &script_text);
    write_file(&lone.join("sor-url"), "http://lone.test:7900\n");
    f.clear_argv();
    let r = Fixture::finish(
        f.command(&lone.join("boss-api")),
        &["GET", "/health"],
        &[("BOSS_ACTOR", "agent-x"), ("BOSS_SOR_SERVICE", "gateway")],
    );
    assert_eq!(r.code, 2, "no table, no placing the name: {}", r.stderr);
    assert!(f.curl_argv().is_empty(), "curl must not run");
    assert!(
        r.stderr.contains("gateway") && r.stderr.contains("BOSS_SOR_PORTS"),
        "the refusal names the service and where a table comes from: {}",
        r.stderr
    );
}

/// NO TABLE, NO ROUTING. A copy of the script with sor-url beside it
/// but no forge/sor-ports.env two directories up, and no
/// BOSS_SOR_PORTS, is the door as it was: every path on the base.
#[test]
fn without_a_table_every_path_goes_to_the_base_as_before() {
    let f = Fixture::new("route-no-table");
    let lone = f.root.join("lone");
    create_dir(&lone);
    let script_text = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("read boss-api");
    write_exec(&lone.join("boss-api"), &script_text);
    write_file(&lone.join("sor-url"), "http://lone.test:7900\n");
    let r = Fixture::finish(
        f.command(&lone.join("boss-api")),
        &["GET", "/api/people/accounts"],
        &[("BOSS_ACTOR", "agent-x")],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        f.curl_argv().last().map(String::as_str),
        Some("http://lone.test:7900/api/people/accounts"),
        "absent both tables, the path is not routed"
    );
}

// -- a rollout is waited out ----------------------------------------------
//
// Backlog 034002b3, twice on 2026-09-23: the stack rolls with strategy
// Recreate, so every converging train takes the jobs API dark for about
// a minute, and every write through this door in that minute exited 7
// (`No route to host`) at once and had to be relaunched by hand. curl
// exits 6 (could not resolve host) and 7 (could not connect: refused,
// no route, network unreachable) only when the request never left —
// those, and only those, are waited out. Every other exit may be a
// request that reached the server (28 is one code for a connect timeout
// AND a transfer that timed out after the body went; 52 and 56 are a
// reply lost after sending), so it is surfaced as before, and an HTTP
// status is an answer.

/// Replace the fixture's curl with one that refuses its first
/// `STUB_REFUSALS` calls the way real curl does under `-sS -w
/// '\n%{http_code}'` (its message on stderr, `000` on stdout, exit
/// `STUB_RC`, default 7), then answers as the plain stub does. Every
/// call is counted in `curl-calls.txt`. And a `sleep` that records the
/// wait it was asked for in `sleeps.txt` and returns at once, so a test
/// reads the backoff without spending it.
fn install_rolling_stubs(f: &Fixture) -> (PathBuf, PathBuf) {
    let calls = f.root.join("curl-calls.txt");
    let sleeps = f.root.join("sleeps.txt");
    write_exec(
        &f.bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\n\
             echo call >> '{calls}'\n\
             n=$(wc -l < '{calls}')\n\
             if [ \"$n\" -le \"${{STUB_REFUSALS:-0}}\" ]; then\n\
                 if [ -n \"${{STUB_CROSS_A_SECOND:-}}\" ]; then\n\
                     t=$(printf '%(%s)T' -1)\n\
                     while [ \"$(printf '%(%s)T' -1)\" = \"$t\" ]; do :; done\n\
                 fi\n\
                 echo 'curl: (7) Failed to connect to sor.test port 7900: No route to host' >&2\n\
                 printf '\\n000'\n\
                 exit \"${{STUB_RC:-7}}\"\n\
             fi\n\
             : > \"$STUB_ARGV\"\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\" >> \"$STUB_ARGV\"; done\n\
             printf '%s\\n%s' \"${{STUB_BODY:-}}\" \"${{STUB_CODE:-200}}\"\n",
            calls = calls.display()
        ),
    );
    write_exec(
        &f.bin.join("sleep"),
        &format!(
            "#!/usr/bin/env bash\necho \"$1\" >> '{}'\n",
            sleeps.display()
        ),
    );
    (calls, sleeps)
}

fn count_lines(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn a_refused_connect_is_waited_out_and_the_write_goes_out_once_the_api_is_back() {
    let f = Fixture::new("roll-waited");
    let (calls, sleeps) = install_rolling_stubs(&f);
    let body = f.root.join("body.json");
    write_file(&body, r#"{"kind":"backlog-item"}"#);
    let r = f.run(
        &["POST", "/api/jobs", body.to_str().expect("utf8")],
        &[
            ("STUB_REFUSALS", "2"),
            ("STUB_BODY", r#"{"id":"j1"}"#),
            ("BOSS_ACTOR", "agent-x"),
        ],
    );
    assert_eq!(
        r.code, 0,
        "the write lands once the API is back: {}",
        r.stderr
    );
    assert_eq!(
        r.stdout, "{\"id\":\"j1\"}\n",
        "only the answer reaches stdout"
    );
    assert_eq!(count_lines(&calls), 3, "two refusals, then the write");
    let said = r
        .stderr
        .lines()
        .filter(|l| {
            l.contains("the jobs API is not answering (a rollout?) — retrying POST /api/jobs")
        })
        .count();
    assert_eq!(said, 2, "one visible line per wait:\n{}", r.stderr);
    assert_eq!(
        std::fs::read_to_string(&sleeps).unwrap_or_default(),
        "2\n4\n",
        "the waits back off"
    );
    assert!(r.stderr.ends_with("HTTP:200\n"), "{}", r.stderr);
}

#[test]
fn a_dns_failure_is_waited_out_too() {
    // exit 6: the name did not resolve, so nothing was sent.
    let f = Fixture::new("roll-dns");
    let (calls, _) = install_rolling_stubs(&f);
    let r = f.run(
        &["GET", "/api/jobs"],
        &[
            ("STUB_REFUSALS", "1"),
            ("STUB_RC", "6"),
            ("BOSS_ACTOR", "agent-x"),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(count_lines(&calls), 2);
}

#[test]
fn only_a_request_that_never_left_is_retried() {
    for rc in ["28", "52", "56"] {
        let f = Fixture::new(&format!("roll-not-{rc}"));
        let (calls, sleeps) = install_rolling_stubs(&f);
        let body = f.root.join("body.json");
        write_file(&body, "{}");
        let r = f.run(
            &["POST", "/api/jobs", body.to_str().expect("utf8")],
            &[
                ("STUB_REFUSALS", "1"),
                ("STUB_RC", rc),
                ("BOSS_ACTOR", "agent-x"),
            ],
        );
        assert_eq!(
            r.code.to_string(),
            rc,
            "curl {rc} may have reached the server — surfaced with curl's own code: {}",
            r.stderr
        );
        assert_eq!(count_lines(&calls), 1, "curl {rc} is asked once");
        assert_eq!(count_lines(&sleeps), 0, "curl {rc} is not waited out");
        assert!(!r.stderr.contains("retrying"), "{}", r.stderr);
    }
    // And an HTTP status is an answer, even a 503.
    let f = Fixture::new("roll-not-503");
    let (calls, _) = install_rolling_stubs(&f);
    let r = f.run(
        &["GET", "/api/jobs"],
        &[("STUB_CODE", "503"), ("BOSS_ACTOR", "agent-x")],
    );
    assert_eq!(r.code, 1, "{}", r.stderr);
    assert_eq!(count_lines(&calls), 1, "a 503 is asked once");
}

#[test]
fn a_roll_that_outlasts_the_window_fails_naming_the_elapsed_time() {
    // BOSS_SOR_WAIT_SECONDS is the window; 0 means the first refusal is
    // already past it, so the failure is read without spending one.
    let f = Fixture::new("roll-outlasted");
    let (calls, sleeps) = install_rolling_stubs(&f);
    let r = f.run(
        &["GET", "/api/jobs"],
        &[
            ("STUB_REFUSALS", "99"),
            ("STUB_CROSS_A_SECOND", "1"),
            ("BOSS_SOR_WAIT_SECONDS", "0"),
            ("BOSS_ACTOR", "agent-x"),
        ],
    );
    assert_eq!(r.code, 7, "curl's own code, as before: {}", r.stderr);
    assert_eq!(count_lines(&calls), 1);
    assert_eq!(count_lines(&sleeps), 0);
    // The elapsed time is bash's whole-second $SECONDS, so it reads the
    // wall clock's boundaries, not the call's length: a sub-second
    // refusal names 0s or 1s. The stub spins across a boundary so the
    // answer is always at least 1 — the reading is the real elapsed
    // time, never a constant (the gate flaked on an exact "0s").
    const NAMED: &str = "boss-api: GET /api/jobs: the jobs API refused every connection for ";
    let elapsed = r.stderr.split_once(NAMED).and_then(|(_, rest)| {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        rest[digits.len()..]
            .starts_with("s (1 attempts")
            .then(|| digits.parse::<u64>().ok())
            .flatten()
    });
    assert!(
        elapsed.is_some_and(|s| s >= 1)
            && r.stderr.contains("waited out for up to 0s")
            && r.stderr.contains("nothing was sent"),
        "the failure names the call, the elapsed time, and that relaunching is safe:\n{}",
        r.stderr
    );
    assert!(
        r.stdout.is_empty(),
        "no body on a refused connect: {:?}",
        r.stdout
    );

    // A window that is not a whole number of seconds is refused before
    // curl runs, rather than read as zero or as forever.
    let r = f.run(
        &["GET", "/api/jobs"],
        &[("BOSS_SOR_WAIT_SECONDS", "2m"), ("BOSS_ACTOR", "agent-x")],
    );
    assert_eq!(r.code, 2, "{}", r.stderr);
    assert!(r.stderr.contains("BOSS_SOR_WAIT_SECONDS"), "{}", r.stderr);
    assert_eq!(count_lines(&calls), 1, "curl did not run");
}

#[test]
fn the_default_window_is_two_minutes() {
    let text = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("read boss-api");
    assert!(
        text.contains("WAIT_WINDOW=${BOSS_SOR_WAIT_SECONDS:-120}"),
        "the door waits out a roll for 120s, the same window as the CLI's ROLL_WAIT"
    );
}
