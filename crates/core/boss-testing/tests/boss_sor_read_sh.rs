//! `infra/forge/probe-bin/boss-sor-read` — the one way a car's recorded
//! probe reads the system of record on the forge: GET-only, base and
//! identity supplied by the door that runs the probe, never by the
//! probe's own text.
//!
//! THE DOOR is `boss prove <car> --from-car --unattended`
//! (crates/orchestrators/boss-cli/src/prove.rs), what the forge's
//! ops-runner runs for a `run-car-probe` ops-request. Until backlog
//! 9f00a805 (consolidation H8, car 2, 2026-09-18) it was the shell twin
//! infra/forge/run-car-probe.sh, and this file was that script's pin:
//! it lifted the script's prelude and verdict blocks out and RAN them,
//! and read the reader's role out of the script's `PROBE-READER` block.
//! The twin is retired and those pins went with it — the prelude, the
//! verdict and the reader identity are Rust now, tested beside their
//! definition (`prove.rs`, "THE UNATTENDED DOOR"). What stays here is
//! the READER, which is still a shell script in the tree: it ships in
//! the checkout, it is executable, it refuses to read unidentified, it
//! takes a path and nothing else, and it routes a path to its service's
//! port by the table the door hands it (design 28d2bed9). The equality
//! pin between that table, the Service manifest and boss-ports is
//! the_machine_door_carries_every_read_surface.rs.

use boss_testing::repo_root;
use std::path::PathBuf;
use std::process::Command;

fn scratch(case: &str) -> PathBuf {
    // Per-uid and per-process, and it REFUSES by name if a
    // leftover cannot be cleared — see `boss_testing::scratch`.
    boss_testing::scratch_dir(&format!("boss-sor-read-sh-{case}"))
}

/// The role the door gives the probe's reader. DEFINED in
/// `prove.rs::READER_ROLE` and pinned against core policy's defaults
/// there; this file only needs a well-formed header to hand the reader,
/// which is why a literal is honest here.
const READER_ROLE: &str = "audit-readonly";

// =====================================================================
// A PROBE READS THE SYSTEM OF RECORD AS A NAMED READER (61085a9e).
//
// THE DEFECT, measured 2026-09-10 against one backend at one commit.
// The shell twin built the right `x-boss-user` header for its OWN three
// calls — read the car, write the verdict, patch the attempt — and
// handed the probe an env carrying only `BOSS_JOBS_URL`. So a probe
// doing `curl $BOSS_JOBS_URL/api/...` reads as `operator:unidentified`,
// and policy answers a NARROWER WORLD without saying so:
//
//   /api/jobs?kind=pr-train&status=open        operator 1  → 0
//   /api/jobs?kind=ship-a-change&status=open           21  → 0
//   /api/jobs?kind=backlog-item&status=open            30  → 0
//   /api/yard/status  trains/dock/recent/dock_depth 1/1/8/1 → all 0
//   /api/workflows                                     84  → 84 (same)
//
// Per-surface, with nothing in the answer to say which kind you hit.
// `/api/yard/status` comes back a confident, well-formed, completely
// idle yard: no trains, nothing on the dock, threshold not met.
//
// WHY THAT IS WORSE THAN A BROKEN READ. A PRESENCE assertion fails for
// the wrong reason and reads as a false negative about the car —
// recoverable, because someone investigates. An ABSENCE assertion
// PASSES FALSELY: "no open job of kind X remains" is green against an
// empty page the probe was never allowed to see. That is a proof that
// vouches for nothing, recorded as a proof, on a car that then closes.
// It caught the person who filed the item twice in one hour.
//
// THE FIX, and why it is not one line. Exporting the door's own
// write actor would hand every car's probe text a platform-admin
// credential with WRITE capability, and a probe is program text
// authored upstream and run as david on the forge. So the probe gets a
// SEPARATE, READ-SCOPED actor (`audit-readonly`: core policy grants it
// Read at Scope::All on every shipped resource and no other action at
// all) plus a GET-only reader on its PATH whose header comes from the
// door, never from the probe's text.
// =====================================================================

/// The reader the probe is given, as the door spells it on PATH.
const READER: &str = "boss-sor-read";

fn reader_path() -> PathBuf {
    repo_root().join("infra/forge/probe-bin").join(READER)
}
/// The reader the gate advises must EXIST, and be executable in the
/// checkout the forge runs probes from (BOSS_PROBE_DIR). A refusal that names a tool
/// nobody shipped is worse than no refusal.
#[test]
fn the_sanctioned_reader_ships_in_the_checkout_and_is_executable() {
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
/// argv. Returns (exit code, stdout, stderr, argv one per line).
fn run_reader(case: &str, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String, String) {
    let dir = scratch(case);
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    let log = dir.join("curl-argv");
    boss_testing::write_file(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\n\
             printf 'STUB_BODY\\n'\n",
            log.display()
        ),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("curl"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub curl");
    }
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

/// THE READER IDENTIFIES THE PROBE. The header comes from the door's
/// env, never from the probe's text, and the method is GET because
/// there is no method argument to give.
#[test]
fn the_reader_sends_the_runners_read_scoped_header_on_a_get() {
    let actor = format!("{{\"id\":\"automation:x\",\"role\":\"{}\"}}", READER_ROLE);
    let (rc, stdout, stderr, argv) = run_reader(
        "reader-get",
        &["/api/yard/status"],
        &[
            ("BOSS_JOBS_URL", "http://sor.invalid:7900"),
            ("BOSS_SOR_USER", &actor),
        ],
    );
    assert_eq!(rc, 0, "stderr: {stderr}");
    assert!(stdout.contains("STUB_BODY"), "stdout: {stdout}");
    assert!(
        argv.contains(&format!("x-boss-user: {actor}")),
        "the reader did not send the door's actor; argv:\n{argv}"
    );
    assert!(
        argv.contains("http://sor.invalid:7900/api/yard/status"),
        "the reader must read the base the door pinned; argv:\n{argv}"
    );
    assert!(
        !argv.lines().any(|l| l == "-X"),
        "the reader must be GET-only — no method is selectable; argv:\n{argv}"
    );
}

/// AND IT REFUSES RATHER THAN READING UNIDENTIFIED. With no actor in
/// the env the reader must stop — never fall back to a bare read, which
/// is the false-absence shape this whole block is about. `curl` must not
/// even be reached.
#[test]
fn the_reader_refuses_rather_than_reading_unidentified() {
    let (rc, stdout, stderr, argv) = run_reader(
        "reader-no-actor",
        &["/api/yard/status"],
        &[("BOSS_JOBS_URL", "http://sor.invalid:7900")],
    );
    assert_ne!(
        rc, 0,
        "a reader with no identity must refuse; stdout: {stdout}"
    );
    assert!(
        stderr.contains("BOSS_SOR_USER"),
        "the refusal must name what is missing; stderr: {stderr}"
    );
    assert!(
        argv.is_empty(),
        "curl was invoked anyway — the unidentified read happened; argv:\n{argv}"
    );
}

/// A METHOD IS NOT SELECTABLE, AND NEITHER IS THE HOST. Both are the
/// "a wrong target answers instead of erroring" lesson: a probe that
/// could name its own URL could read the OTHER, older stack at boss-gcp
/// and get a well-formed answer about different data.
#[test]
fn the_reader_takes_a_path_and_refuses_anything_else() {
    let actor = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";
    for (case, args) in [
        (
            "reader-absolute-url",
            vec!["http://127.0.0.1:7900/api/jobs"],
        ),
        ("reader-method", vec!["PUT", "/api/jobs"]),
        ("reader-no-args", vec![]),
    ] {
        let (rc, _, stderr, argv) = run_reader(
            case,
            &args,
            &[
                ("BOSS_JOBS_URL", "http://sor.invalid:7900"),
                ("BOSS_SOR_USER", actor),
            ],
        );
        assert_ne!(rc, 0, "{case} must be refused");
        assert!(argv.is_empty(), "{case} reached curl anyway; argv:\n{argv}");
        assert!(!stderr.is_empty(), "{case} must say why");
    }
}

// =====================================================================
// THE READER ROUTES BY PATH (design 28d2bed9, David 2026-09-17). The
// machine door carries every read surface of the instance on one IP,
// one port per service; the reader picks the port from a `name=port`
// table the door hands it as BOSS_SOR_PORTS, by the path's prefix,
// and keeps the HOST from BOSS_JOBS_URL. Without a table it is exactly
// the reader it was: one base, one port, the jobs API — so nothing
// changes until the door carries the table. The equality pin between
// the table, the manifest and boss-ports is in
// the_machine_door_carries_every_read_surface.rs.
// =====================================================================

const PORTS_TABLE: &str = "jobs=7900 events=7150 people=7500 classes=7800 locations=7820 \
                           accounts=7550 ledger=7080 dispatcher=7950";

fn reader_env<'a>(actor: &'a str, table: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    let mut env = vec![
        ("BOSS_JOBS_URL", "http://sor.invalid:7900"),
        ("BOSS_SOR_USER", actor),
    ];
    if let Some(t) = table {
        env.push(("BOSS_SOR_PORTS", t));
    }
    env
}

/// The URL the stub curl was handed — the last argv line.
fn url_read(argv: &str) -> String {
    argv.lines().last().unwrap_or_default().to_string()
}

/// A path under a routed prefix goes to that service's port, on the
/// host BOSS_JOBS_URL names — the measured case first: the audit tail,
/// which 404'd on the jobs port for car 056f7bd8.
#[test]
fn the_reader_routes_a_prefixed_path_to_its_services_port() {
    let actor = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";
    for (case, path, want) in [
        (
            "route-events",
            "/api/events/tail?kind=declared&limit=500",
            "http://sor.invalid:7150/api/events/tail?kind=declared&limit=500",
        ),
        (
            "route-people",
            "/api/people/emp-david",
            "http://sor.invalid:7500/api/people/emp-david",
        ),
        (
            "route-people-bare",
            "/api/people?limit=1",
            "http://sor.invalid:7500/api/people?limit=1",
        ),
        (
            "route-classes",
            "/api/classes?subject_kind=employee",
            "http://sor.invalid:7800/api/classes?subject_kind=employee",
        ),
        (
            "route-locations",
            "/api/locations/loc-hq",
            "http://sor.invalid:7820/api/locations/loc-hq",
        ),
        // boss-accounts mounts under /api/people/ but is its own
        // service on 7550 (backlog de0989d2): the longer prefix wins
        // over the people route.
        (
            "route-accounts",
            "/api/people/accounts?limit=1",
            "http://sor.invalid:7550/api/people/accounts?limit=1",
        ),
        (
            "route-accounts-my-day",
            "/api/people/my-day/actions",
            "http://sor.invalid:7550/api/people/my-day/actions",
        ),
        // The ledger (7080) and the dispatcher's rule registry (7950)
        // joined the door on 2026-09-17 (backlog 77fd7b5a + 4145d2c1).
        (
            "route-ledger",
            "/api/ledger/trial-balance",
            "http://sor.invalid:7080/api/ledger/trial-balance",
        ),
        (
            "route-dispatcher",
            "/api/dispatcher/rules",
            "http://sor.invalid:7950/api/dispatcher/rules",
        ),
    ] {
        let (rc, _, stderr, argv) =
            run_reader(case, &[path], &reader_env(actor, Some(PORTS_TABLE)));
        assert_eq!(rc, 0, "{case}: stderr: {stderr}");
        assert_eq!(url_read(&argv), want, "{case}: argv:\n{argv}");
        assert!(
            argv.contains(&format!("x-boss-user: {actor}")),
            "{case}: the routed read lost the door's actor; argv:\n{argv}"
        );
    }
}

/// Everything else is the jobs API, on the base exactly as the door
/// pinned it — including a prefix that merely RESEMBLES a routed one.
#[test]
fn the_reader_defaults_to_the_jobs_api() {
    let actor = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";
    for (case, path) in [
        ("default-yard", "/api/yard/status"),
        ("default-jobs", "/api/jobs?kind=pr-train&status=open"),
        ("default-agents", "/api/agents"),
        ("default-lookalike", "/api/peoples/x"),
        ("default-eventsish", "/api/eventsource"),
    ] {
        let (rc, _, stderr, argv) =
            run_reader(case, &[path], &reader_env(actor, Some(PORTS_TABLE)));
        assert_eq!(rc, 0, "{case}: stderr: {stderr}");
        assert_eq!(
            url_read(&argv),
            format!("http://sor.invalid:7900{path}"),
            "{case}: argv:\n{argv}"
        );
    }
}

/// NO TABLE, NO CHANGE. A runner that does not carry BOSS_SOR_PORTS
/// (a checkout older than this car, a hand run) gets the reader it
/// always had: every path on the base, the jobs port. The routed read
/// then 404s exactly as it did, which is a loud failure and not a
/// guessed port.
#[test]
fn the_reader_without_a_table_reads_every_path_on_the_base() {
    let actor = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";
    for (case, path) in [
        ("no-table-events", "/api/events/tail?limit=1"),
        ("no-table-jobs", "/api/jobs"),
    ] {
        let (rc, _, stderr, argv) = run_reader(case, &[path], &reader_env(actor, None));
        assert_eq!(rc, 0, "{case}: stderr: {stderr}");
        assert_eq!(
            url_read(&argv),
            format!("http://sor.invalid:7900{path}"),
            "{case}: argv:\n{argv}"
        );
    }
}

/// A table that EXISTS but lacks the service a path routes to is a
/// defect in the table, not a reason to guess: the reader refuses and
/// names the missing entry, before curl is reached.
#[test]
fn the_reader_refuses_a_table_that_lacks_the_routed_service() {
    let actor = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";
    let (rc, _, stderr, argv) = run_reader(
        "table-missing-events",
        &["/api/events/tail"],
        &reader_env(actor, Some("jobs=7900 people=7500")),
    );
    assert_eq!(rc, 2, "stderr: {stderr}");
    assert!(
        argv.is_empty(),
        "curl was reached with a guessed port; argv:\n{argv}"
    );
    assert!(
        stderr.contains("events") && stderr.contains("BOSS_SOR_PORTS"),
        "the refusal must name the missing service and the table; stderr: {stderr}"
    );
}

/// The refusals are untouched by routing: still one argument, still a
/// path, still identified — with the table present.
#[test]
fn routing_leaves_every_refusal_in_place() {
    let actor = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";
    for (case, args, env) in [
        (
            "routed-url",
            vec!["http://sor.invalid:7150/api/events/tail"],
            reader_env(actor, Some(PORTS_TABLE)),
        ),
        (
            "routed-two-args",
            vec!["/api/events/tail", "/api/jobs"],
            reader_env(actor, Some(PORTS_TABLE)),
        ),
        (
            "routed-unidentified",
            vec!["/api/events/tail"],
            vec![
                ("BOSS_JOBS_URL", "http://sor.invalid:7900"),
                ("BOSS_SOR_PORTS", PORTS_TABLE),
            ],
        ),
    ] {
        let (rc, _, stderr, argv) = run_reader(case, &args, &env);
        assert_eq!(rc, 2, "{case} must be refused; stderr: {stderr}");
        assert!(argv.is_empty(), "{case} reached curl anyway; argv:\n{argv}");
    }
}
