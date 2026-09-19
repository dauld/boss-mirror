//! `infra/forge/probe-bin/boss-gateway-read` — the one way a car's
//! recorded probe reads the instance's GATEWAY, and the one reader on
//! the forge that sends NO identity on purpose.
//!
//! WHY (backlog 240e03f3, left by the builder of b4afd7b9 on
//! 2026-09-18). The hardening claim "an undeclared /api read answers
//! 401 without a session" was unprovable by probe: `boss-sor-read`
//! reaches only the service ports on the LAN machine door, every read
//! it makes carries the runner's read-scoped actor, and prod's
//! hostname sits behind Cloudflare Access, so a curl from the forge
//! answers Access's 302 — never the gateway's 401. The address may not
//! be spelled in a probe (the-estate-address-lives-once). So: a
//! `gateway=<port>` row on the machine door (sor-ports.env, pinned to
//! boss-ports by the_machine_door_carries_every_read_surface.rs) and
//! this reader, which takes the HOST from BOSS_JOBS_URL, the PORT from
//! that row, sends nothing that identifies a caller, and prints the
//! status code alone — so a probe asserts 401 versus 200 by effect.
//!
//! WHAT THIS FILE HOLDS. The reader ships in the checkout and is
//! executable; it sends no `x-boss-user` header and no cookie even when
//! the door has exported the reader identity into its env; it reads on
//! the gateway row's port and BOSS_JOBS_URL's host; it prints only the
//! status; it refuses a URL, a method, a second argument, an absent
//! base and a table without the gateway row — and it never turns "no
//! answer" into a status.

use boss_testing::repo_root;
use std::path::PathBuf;
use std::process::Command;

const READER: &str = "boss-gateway-read";

fn reader_path() -> PathBuf {
    repo_root().join("infra/forge/probe-bin").join(READER)
}

fn scratch(case: &str) -> PathBuf {
    boss_testing::scratch_dir(&format!("boss-gateway-read-sh-{case}"))
}

/// The table the unattended door hands every probe, with the gateway
/// row the machine door carries since this car.
const PORTS_TABLE: &str = "jobs=7900 events=7150 people=7500 classes=7800 locations=7820 \
                           accounts=7550 ledger=7080 dispatcher=7950 gateway=4443";

/// What the door exports for `boss-sor-read` — present here to prove
/// this reader IGNORES it.
const DOOR_ACTOR: &str = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";

#[test]
fn the_gateway_reader_ships_in_the_checkout_and_is_executable() {
    let p = reader_path();
    assert!(
        p.is_file(),
        "the probe's PATH points at {}, which does not exist",
        p.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&p)
            .expect("reader metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "{} is not executable (mode {mode:o}) — the probe would get `command not found`",
            p.display()
        );
    }
}

/// Run the reader with a STUB `curl` first on PATH that records its
/// argv, prints `stub_out` and exits `stub_rc`. Returns (exit code,
/// stdout, stderr, argv one per line).
fn run_reader(
    case: &str,
    args: &[&str],
    env: &[(&str, &str)],
    stub_out: &str,
    stub_rc: i32,
) -> (i32, String, String, String) {
    let dir = scratch(case);
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    let log = dir.join("curl-argv");
    boss_testing::write_exec(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\n\
             printf '%s' '{stub_out}'\nexit {stub_rc}\n",
            log.display()
        ),
    );
    let mut cmd = Command::new(reader_path());
    cmd.args(args)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .current_dir(&dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("the reader runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        std::fs::read_to_string(&log).unwrap_or_default(),
    )
}

fn full_env<'a>() -> Vec<(&'a str, &'a str)> {
    vec![
        ("BOSS_JOBS_URL", "http://sor.invalid:7900"),
        ("BOSS_SOR_USER", DOOR_ACTOR),
        ("BOSS_SOR_PORTS", PORTS_TABLE),
    ]
}

/// The URL the stub curl was handed — the last argv line.
fn url_read(argv: &str) -> String {
    argv.lines().last().unwrap_or_default().to_string()
}

/// THE READ CARRIES NO IDENTITY, on purpose, and reads the gateway row's
/// port on the door's host. With the door's actor sitting in the env —
/// exactly as the unattended door exports it for `boss-sor-read` — this
/// reader must not put it on the wire, must send no cookie, must not
/// select a method, and must print the status alone: the claim it
/// exists to prove is what the gateway answers a stranger.
#[test]
fn the_gateway_reader_sends_nothing_that_identifies_a_caller() {
    let (rc, stdout, stderr, argv) =
        run_reader("no-identity", &["/api/jobs"], &full_env(), "401", 0);
    assert_eq!(rc, 0, "stderr: {stderr}");
    assert_eq!(stdout, "401\n", "the status code alone; stdout: {stdout:?}");
    assert_eq!(
        url_read(&argv),
        "http://sor.invalid:4443/api/jobs",
        "the gateway row's port on BOSS_JOBS_URL's host; argv:\n{argv}"
    );
    assert!(
        !argv.contains("x-boss-user"),
        "the reader sent the door's actor — the read is identified; argv:\n{argv}"
    );
    assert!(
        !argv.contains(DOOR_ACTOR),
        "the door's actor reached curl in some other spelling; argv:\n{argv}"
    );
    for flag in ["-b", "--cookie", "-u", "--user", "-X", "--request"] {
        assert!(
            !argv.lines().any(|l| l == flag),
            "the reader must send no cookie, no basic auth and no method (`{flag}`); argv:\n{argv}"
        );
    }
    assert!(
        argv.lines().any(|l| l.contains("http_code")),
        "the reader asks curl for the status code; argv:\n{argv}"
    );
    assert!(
        !argv
            .lines()
            .any(|l| l == "-f" || l == "--fail" || l == "-fsS"),
        "a 401 is the ANSWER this reader exists to read, not a failure to hide; argv:\n{argv}"
    );
}

/// The query string rides with the path, and a 200 is printed as
/// faithfully as a 401 — the reader reports, the probe judges.
#[test]
fn the_gateway_reader_prints_whatever_status_the_gateway_answered() {
    let (rc, stdout, _, argv) = run_reader(
        "status-200",
        &["/api/jobs/live?limit=1"],
        &full_env(),
        "200",
        0,
    );
    assert_eq!(rc, 0);
    assert_eq!(stdout, "200\n");
    assert_eq!(
        url_read(&argv),
        "http://sor.invalid:4443/api/jobs/live?limit=1"
    );
}

/// NO ANSWER IS NOT A STATUS. When curl cannot connect it prints `000`
/// for `%{http_code}` and exits 7; a probe reading `000` as "not 401"
/// would judge a door that has not converged yet as a false claim.
/// The reader exits with curl's status, says why on stderr, and prints
/// no status line at all.
#[test]
fn the_gateway_reader_never_turns_no_answer_into_a_status() {
    let (rc, stdout, stderr, _) = run_reader("no-answer", &["/api/jobs"], &full_env(), "000", 7);
    assert_eq!(rc, 7, "curl's own exit, so the probe's not-yet can see it");
    assert!(
        stdout.trim().is_empty(),
        "nothing on stdout when nothing answered; got {stdout:?}"
    );
    assert!(
        stderr.contains("did not answer"),
        "stderr must say the gateway did not answer: {stderr}"
    );
}

/// A TABLE WITHOUT THE GATEWAY ROW IS A DOOR THAT DOES NOT CARRY THE
/// GATEWAY — refused, naming the row and the table, before curl is
/// reached. There is no fallback to the jobs port: that port answers a
/// stranger 200 with a narrowed world, which is the exact defect a
/// 401-versus-200 probe exists to see.
#[test]
fn the_gateway_reader_refuses_a_table_without_the_gateway_row() {
    for (case, env) in [
        (
            "table-no-gateway",
            vec![
                ("BOSS_JOBS_URL", "http://sor.invalid:7900"),
                ("BOSS_SOR_PORTS", "jobs=7900 events=7150"),
            ],
        ),
        (
            "table-absent",
            vec![("BOSS_JOBS_URL", "http://sor.invalid:7900")],
        ),
    ] {
        let (rc, stdout, stderr, argv) = run_reader(case, &["/api/jobs"], &env, "401", 0);
        assert_eq!(
            rc, 2,
            "{case}: a refusal; stdout: {stdout} stderr: {stderr}"
        );
        assert!(
            argv.is_empty(),
            "{case}: curl was reached with a guessed port; argv:\n{argv}"
        );
        assert!(
            stderr.contains("gateway") && stderr.contains("BOSS_SOR_PORTS"),
            "{case}: the refusal must name the missing row and the table; stderr: {stderr}"
        );
    }
}

/// The same refusals as `boss-sor-read`, for the same reason: a probe
/// that could name its own host reads a different stack and gets a
/// well-formed answer about different data; a method would make this a
/// write door; and no base means no safe default.
#[test]
fn the_gateway_reader_takes_a_path_and_refuses_anything_else() {
    for (case, args, env) in [
        (
            "absolute-url",
            vec!["http://sor.invalid:4443/api/jobs"],
            full_env(),
        ),
        ("method", vec!["POST", "/api/jobs"], full_env()),
        ("no-args", vec![], full_env()),
        ("relative-path", vec!["api/jobs"], full_env()),
        (
            "no-base",
            vec!["/api/jobs"],
            vec![("BOSS_SOR_PORTS", PORTS_TABLE)],
        ),
    ] {
        let (rc, _, stderr, argv) = run_reader(case, &args, &env, "401", 0);
        assert_eq!(rc, 2, "{case} must be refused; stderr: {stderr}");
        assert!(argv.is_empty(), "{case} reached curl anyway; argv:\n{argv}");
        assert!(!stderr.is_empty(), "{case} must say why");
    }
}
