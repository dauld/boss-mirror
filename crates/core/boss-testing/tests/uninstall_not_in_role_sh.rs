//! `infra/gcp/uninstall-not-in-role.sh` is RUN, not read — against a
//! stubbed `systemctl`, a scratch `/etc/systemd/system` and a stubbed
//! system of record, so every verdict below is one the script actually
//! reached.
//!
//! WHY THE VERB EXISTS (design 9e3e093f, decided by David 2026-09-11;
//! backlog d5941ef3 car 4). A host derives its unit roster from the
//! roles it declares, and `install-units.sh units` installs only the
//! rows the roles name — every other row it REPORTS as NOT IN ROLE and
//! leaves alone. After the retire verb stopped the second stack on
//! 2026-09-15 21:11Z (ops-request 7912c9ae) the ten legacy-stack unit
//! pairs stayed on disk, disabled, and boss-gcp-converge has reported
//! "not in role 10" on every tick since. A unit installed and never
//! serving is worse than absent. So the uninstall is an ops verb: the
//! set it may touch is DERIVED, at call time, by the installer's own
//! `roster` mode — TIMERS rows minus what the host's live roles name —
//! never a second list; it refuses anything outside that set, refuses
//! when the set is empty, and prints every unit it removed, in order,
//! onto the packet.
//!
//! What each case pins: the derivation is the installer's own and
//! answers the live roles; the dry run plans exactly the not-in-role
//! set and changes nothing; a roles reading that is not live, or empty,
//! or that leaves nothing outside the roster, is a refusal; the real
//! run disables and removes exactly the set (timers before services),
//! reloads, verifies, and reports it; a kept unit and a foreign unit
//! file are never touched; a failure mid-way names what was done; and
//! the allowlist's own validation, exercised THROUGH `ops-runner.sh`
//! with the real verb files, as the runner on boss-gcp would.
//!
//! Nothing here touches a host. `systemctl` is a stub on every path and
//! the unit files live in a scratch directory this process owns.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

const SCRIPT: &str = "infra/gcp/uninstall-not-in-role.sh";
const INSTALLER: &str = "infra/gcp/install-units.sh";
/// The roles boss-gcp declared on 2026-09-15 (legacy-stack gone since
/// car 3 landed on #362).
const ROLES: &str = r#"["wireguard-bastion","off-cluster-observer","ml-batch-host"]"#;

/// The stems `infra/estate/roles.toml` names under one section header,
/// read the way `install-units.sh`'s `role_units` reads them: each
/// `units = [...]` is on one line.
fn role_units(section: &str) -> Vec<String> {
    let toml = std::fs::read_to_string(repo_root().join("infra/estate/roles.toml"))
        .expect("infra/estate/roles.toml");
    let mut on = false;
    let mut out = Vec::new();
    for line in toml.lines() {
        if line.trim() == format!("[{section}]") {
            on = true;
            continue;
        }
        if line.starts_with('[') {
            on = false;
        }
        if on && line.trim_start().starts_with("units") {
            out.extend(line.split('"').skip(1).step_by(2).map(str::to_string));
        }
    }
    out
}

/// The stems the three declared roles plus `always` name — what the
/// host keeps.
fn kept_stems() -> Vec<String> {
    let mut out = role_units("always");
    for role in [
        "roles.wireguard-bastion",
        "roles.off-cluster-observer",
        "roles.ml-batch-host",
    ] {
        out.extend(role_units(role));
    }
    out
}

/// The stems outside those roles. `infra/lint/boss-gcp-converges-itself.sh`
/// pins roles.toml against the TIMERS array (every stem in exactly one
/// section), so under the three declared roles this is the legacy-stack
/// section and nothing else.
fn not_in_role_stems() -> Vec<String> {
    let mut out = role_units("roles.legacy-stack");
    out.extend(role_units("roles.cluster-operator"));
    assert!(out.len() >= 8, "legacy-stack names {out:?}");
    out
}

/// One fixture: a scratch /etc/systemd/system carrying every TIMERS pair
/// (and its drop-in) plus units nothing derives, a stub `systemctl`
/// that records every call, a stub `curl` answering the estate
/// registry.
struct Case {
    root: PathBuf,
    bin: PathBuf,
    etc: PathBuf,
    log: PathBuf,
    nodes: PathBuf,
}

/// Unit files no derivation names — the door, a daemon retired from
/// the DAEMONS array, and two distro units — which the verb must never
/// touch whatever the roles say.
const FOREIGN: &[&str] = &[
    "boss-ops-runner.service",
    "boss-ops-runner.timer",
    "boss-step-effects-runner.service",
    "wg-quick@wg0.service",
    "caddy.service",
];

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("uninstall-not-in-role-{name}"));
        let bin = root.join("bin");
        let etc = root.join("etc");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&etc).unwrap();
        let log = root.join("systemctl.log");
        let nodes = root.join("nodes.json");

        // What the installer put on the host: every TIMERS pair with
        // the jobs-url drop-in `install_timer_units` writes.
        for stem in kept_stems().iter().chain(not_in_role_stems().iter()) {
            write_file(
                &etc.join(format!("{stem}.service")),
                "[Service]\nType=oneshot\n",
            );
            write_file(
                &etc.join(format!("{stem}.timer")),
                "[Timer]\nOnCalendar=daily\n",
            );
            let d = etc.join(format!("{stem}.service.d"));
            std::fs::create_dir_all(&d).unwrap();
            write_file(
                &d.join("jobs-url.conf"),
                "[Service]\nEnvironment=BOSS_JOBS_URL=http://127.0.0.1:7900\n",
            );
        }
        for u in FOREIGN {
            write_file(&etc.join(u), "[Unit]\nDescription=foreign\n");
        }

        // The `list-units --all` snapshot: the kept units loaded and
        // active; the not-in-role ones as the retire left them on
        // 2026-09-15 — two still loaded but inactive, the rest garbage-
        // collected out of memory (a disabled, inactive unit is not
        // listed, its file is); a dangling reference as not-found; and
        // the foreign daemon loaded but dead.
        let mut snapshot = String::new();
        for stem in kept_stems() {
            snapshot.push_str(&format!("{stem}.timer loaded active waiting kept\n"));
            snapshot.push_str(&format!("{stem}.service loaded inactive dead kept\n"));
        }
        let nir = not_in_role_stems();
        snapshot.push_str(&format!("{}.timer loaded inactive dead retired\n", nir[0]));
        snapshot.push_str(&format!(
            "{}.service loaded inactive dead retired\n",
            nir[0]
        ));
        snapshot.push_str("boss-pr-train-reconcile.service not-found failed failed boss-pr-train-reconcile.service\n");
        snapshot.push_str("boss-step-effects-runner.service loaded inactive dead retired daemon\n");
        snapshot.push_str("boss-ops-runner.timer loaded active running the door\n");
        write_file(&root.join("snapshot.txt"), &snapshot);

        write_exec(
            &bin.join("systemctl"),
            r#"#!/bin/sh
# stub systemctl: list-units reads the snapshot, dropping a loaded unit
# whose file has since been removed (systemd forgets it on reload);
# list-unit-files lists the scratch etc; disable --now, daemon-reload
# and reset-failed append to the log; STUB_FAIL_UNIT fails its disable.
case "$1" in
  list-units)
    while read -r unit load rest; do
      if [ "$load" = loaded ] && [ ! -e "$STUB_ETC/$unit" ] && [ "${unit%.service}" != "boss-ops-runner" ] && [ "$unit" != "boss-ops-runner.timer" ]; then
        continue
      fi
      echo "$unit $load $rest"
    done < "$STUB_SNAPSHOT"
    ;;
  list-unit-files)
    for f in "$STUB_ETC"/boss-*.service "$STUB_ETC"/boss-*.timer; do
      [ -e "$f" ] || continue
      echo "$(basename "$f") disabled enabled"
    done
    ;;
  disable)
    # systemctl disable --now [--] <unit>
    shift 2; [ "$1" = "--" ] && shift
    echo "disable --now $1" >> "$STUB_LOG"
    if [ "$1" = "${STUB_FAIL_UNIT:-}" ]; then
      echo "Failed to disable $1: Unit file $1 does not exist (stub)" >&2
      exit 1
    fi
    ;;
  daemon-reload)
    echo "daemon-reload" >> "$STUB_LOG"
    ;;
  reset-failed)
    shift; [ "$1" = "--" ] && shift
    echo "reset-failed $1" >> "$STUB_LOG"
    ;;
  *) echo "stub systemctl: unexpected $*" >&2; exit 99 ;;
esac
"#,
        );
        write_exec(
            &bin.join("curl"),
            "#!/bin/sh\n# stub curl: the estate registry's /api/estate/nodes\ncat \"$STUB_NODES\"\n",
        );
        write_file(
            &nodes,
            &format!(
                r#"{{"data":[{{"id":"boss-gcp","roles":{ROLES}}},{{"id":"forge","roles":["cluster-operator"]}}]}}"#
            ),
        );
        Self {
            root,
            bin,
            etc,
            log,
            nodes,
        }
    }

    fn env(&self, cmd: &mut Command) {
        cmd.env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_SNAPSHOT", self.root.join("snapshot.txt"))
            .env("STUB_LOG", &self.log)
            .env("STUB_ETC", &self.etc)
            .env("STUB_NODES", &self.nodes)
            .env("INSTALL_ETC", &self.etc)
            // The installer's `roster` mode reads TIMERS and roles.toml
            // off the checkout it is pointed at, and must not read a
            // host's /etc/boss/deploy.env here.
            .env("BOSS_REPO_ROOT", repo_root())
            .env("BOSS_DEPLOY_ENV", "/dev/null")
            .env(
                "BOSS_ESTATE_NODES_URL",
                "http://sor.invalid/api/estate/nodes",
            )
            // The roles reader remembers a live read beside the host's
            // state (/var/lib/boss by default); a test's read stays in
            // its own scratch.
            .env("BOSS_NODE_ROLES_CACHE", self.root.join("roles.cache"));
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT)).args(args);
        self.env(&mut cmd);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("uninstall-not-in-role.sh runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    /// Every `disable --now <unit>` the stub received, in order.
    fn disabled(&self) -> Vec<String> {
        self.log_lines()
            .into_iter()
            .filter_map(|l| l.strip_prefix("disable --now ").map(str::to_string))
            .collect()
    }

    fn log_lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Every file and directory under the scratch etc, sorted.
    fn etc_listing(&self) -> Vec<String> {
        fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let e = e.unwrap();
                let name = format!("{prefix}{}", e.file_name().to_string_lossy());
                if e.path().is_dir() {
                    out.push(format!("{name}/"));
                    walk(&e.path(), &format!("{name}/"), out);
                } else {
                    out.push(name);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.etc, "", &mut out);
        out.sort();
        out
    }

    fn present(&self, unit: &str) -> bool {
        self.etc.join(unit).exists()
    }
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(text.contains(n), "{what}: expected `{n}` in:\n{text}");
    }
}

/// The units the verb may touch, as the plan orders them: for each
/// not-in-role stem its timer then its service.
fn expected_units() -> Vec<String> {
    not_in_role_stems()
        .iter()
        .flat_map(|s| [format!("{s}.timer"), format!("{s}.service")])
        .collect()
}

fn sorted(v: &[String]) -> Vec<String> {
    let mut v = v.to_vec();
    v.sort();
    v
}

// ---------------------------------------------------------------------------
// The derivation is the installer's own.
// ---------------------------------------------------------------------------

/// `install-units.sh roster` says, for every roles.toml row, whether the
/// roles in BOSS_NODE_ROLES name it — the same `roster_for_roles` the
/// `units` mode installs by, so the uninstall set cannot drift from the
/// install set (CLAUDE.md §9a). With no roles declared every row is in
/// role, exactly as the units mode installs every row.
#[test]
fn the_installer_roster_mode_answers_the_roles() {
    let c = Case::new("roster");
    let run = |roles: &str| -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(INSTALLER)).arg("roster");
        c.env(&mut cmd);
        cmd.env("BOSS_NODE_ROLES", roles);
        let out = cmd.output().expect("install-units.sh roster runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    let (rc, text) = run("wireguard-bastion,off-cluster-observer,ml-batch-host");
    assert_eq!(rc, 0, "roster mode did not exit 0:\n{text}");
    let not_in: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("not-in-role ").map(str::to_string))
        .collect();
    let in_role: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("in-role ").map(str::to_string))
        .collect();
    assert_eq!(
        sorted(&not_in),
        sorted(&not_in_role_stems()),
        "the roster mode's not-in-role set is not TIMERS minus the roles' rows:\n{text}"
    );
    assert_eq!(
        sorted(&in_role),
        sorted(&kept_stems()),
        "the roster mode's in-role set is not always + the roles' rows:\n{text}"
    );
    let (rc, text) = run("");
    assert_eq!(rc, 0, "{text}");
    assert!(
        !text.lines().any(|l| l.starts_with("not-in-role ")),
        "with no roles declared a row was reported not-in-role — the units mode installs every row then:\n{text}"
    );
    assert_eq!(
        text.lines().filter(|l| l.starts_with("in-role ")).count(),
        kept_stems().len() + not_in_role_stems().len(),
        "{text}"
    );
}

// ---------------------------------------------------------------------------
// The script, over stubs.
// ---------------------------------------------------------------------------

/// `--dry-run` plans exactly the not-in-role set, timers before
/// services, names what it keeps, and changes nothing: no systemctl
/// call that acts, no file touched.
#[test]
fn dry_run_plans_the_not_in_role_set_and_changes_nothing() {
    let c = Case::new("dry-run");
    let before = c.etc_listing();
    let (rc, text) = c.run(&["--dry-run"]);
    assert_eq!(rc, 0, "dry run did not exit 0:\n{text}");
    assert!(
        c.log_lines().is_empty(),
        "a dry run acted through systemctl:\n{}",
        c.log_lines().join("\n")
    );
    assert_eq!(c.etc_listing(), before, "a dry run changed the unit files");
    let planned: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("would disable+remove ").map(str::to_string))
        .collect();
    assert_eq!(
        sorted(&planned),
        sorted(&expected_units()),
        "the plan is not the not-in-role set:\n{text}"
    );
    for stem in not_in_role_stems() {
        let t = planned.iter().position(|u| *u == format!("{stem}.timer"));
        let s = planned.iter().position(|u| *u == format!("{stem}.service"));
        assert!(
            t < s,
            "{stem}: the timer must be planned before the service:\n{text}"
        );
    }
    contains_all(
        &text,
        &[
            "DRY RUN",
            "roles: boss-gcp declares",
            "plan:",
            "boss-step-effects-runner.service", // in the snapshot the packet holds
        ],
        "the dry run's record",
    );
    for stem in kept_stems() {
        assert!(
            text.contains(&format!("keep {stem}")),
            "the record does not say it keeps {stem}:\n{text}"
        );
        assert!(
            !planned.iter().any(|u| u.starts_with(&format!("{stem}."))),
            "{stem} is in role and reached the plan:\n{text}"
        );
    }
    for u in FOREIGN {
        assert!(
            !planned.iter().any(|p| p == u),
            "{u} is outside every derivation and reached the plan:\n{text}"
        );
    }
}

/// The mode argument is required and is one of two literal words; the
/// script re-checks that itself rather than relying on the allowlist.
#[test]
fn refuses_a_missing_or_unknown_mode() {
    let c = Case::new("mode");
    let before = c.etc_listing();
    for args in [&[][..], &["--now"][..], &["--dry-run", "extra"][..]] {
        let (rc, text) = c.run(args);
        assert_eq!(rc, 2, "{args:?} was not refused:\n{text}");
        contains_all(&text, &["--dry-run", "--for-real"], "the usage");
    }
    assert!(c.log_lines().is_empty());
    assert_eq!(c.etc_listing(), before);
}

/// A roles reading that did not come from the live registry — dark
/// with no cache, or dark with a cached declaration — cannot evaluate
/// the bound, and a bound that cannot be evaluated is a refusal, never
/// a pass. So is a host that declares no roles at all: the installer
/// reads "no roles" as "every row", and every row is not a set this
/// verb may remove anything outside of.
#[test]
fn refuses_when_the_roles_are_not_a_live_non_empty_reading() {
    let c = Case::new("roles");
    let before = c.etc_listing();
    write_file(&c.nodes, "");
    for mode in ["--dry-run", "--for-real"] {
        let (rc, text) = c.run(&[mode]);
        assert_eq!(
            rc, 2,
            "an unreadable registry under {mode} was not a refusal:\n{text}"
        );
        contains_all(&text, &["REFUSED", "roles"], "the refusal");
    }
    std::fs::write(c.root.join("roles.cache"), "wireguard-bastion\n").unwrap();
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "a cached declaration passed the bound:\n{text}");
    contains_all(&text, &["REFUSED", "roles"], "the refusal");
    let _ = std::fs::remove_file(c.root.join("roles.cache"));

    write_file(&c.nodes, r#"{"data":[{"id":"boss-gcp","roles":[]}]}"#);
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "a host declaring no roles was not refused:\n{text}");
    contains_all(&text, &["REFUSED", "roles"], "the refusal");

    assert!(c.log_lines().is_empty(), "a refused run acted");
    assert_eq!(
        c.etc_listing(),
        before,
        "a refused run changed the unit files"
    );
}

/// When the roles name every row there is nothing outside the roster,
/// and an empty set is a refusal rather than a no-op that reads as
/// success.
#[test]
fn refuses_when_every_row_is_in_role() {
    let c = Case::new("all-in-role");
    let before = c.etc_listing();
    write_file(
        &c.nodes,
        r#"{"data":[{"id":"boss-gcp","roles":["wireguard-bastion","off-cluster-observer","ml-batch-host","cluster-operator","legacy-stack"]}]}"#,
    );
    for mode in ["--dry-run", "--for-real"] {
        let (rc, text) = c.run(&[mode]);
        assert_eq!(rc, 2, "an empty set under {mode} was not refused:\n{text}");
        contains_all(&text, &["REFUSED", "nothing"], "the refusal");
    }
    assert!(c.log_lines().is_empty());
    assert_eq!(c.etc_listing(), before);
}

/// The real run disables each planned unit, removes its file and the
/// installer's drop-in, reloads, verifies nothing planned is left, and
/// reports every unit in order — leaving every kept and foreign file
/// exactly where it was.
#[test]
fn the_real_run_disables_and_removes_exactly_the_set_in_order() {
    let c = Case::new("for-real");
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "the real run did not exit 0:\n{text}");
    assert_eq!(
        sorted(&c.disabled()),
        sorted(&expected_units()),
        "the units disabled are not the not-in-role set:\n{text}"
    );
    let log = c.log_lines();
    let last_disable = log
        .iter()
        .rposition(|l| l.starts_with("disable --now "))
        .expect("a disable");
    let reload = log
        .iter()
        .position(|l| l == "daemon-reload")
        .expect("a daemon-reload");
    assert!(
        last_disable < reload,
        "daemon-reload came before the last disable:\n{}",
        log.join("\n")
    );
    for u in expected_units() {
        assert!(!c.present(&u), "{u} is still on disk");
        assert!(
            text.contains(&format!("disabled+removed {u}")),
            "the record does not name {u}:\n{text}"
        );
    }
    for stem in not_in_role_stems() {
        assert!(
            !c.etc.join(format!("{stem}.service.d")).exists(),
            "{stem}'s drop-in directory is still on disk"
        );
    }
    for stem in kept_stems() {
        for suffix in [".timer", ".service", ".service.d/jobs-url.conf"] {
            assert!(
                c.present(&format!("{stem}{suffix}")),
                "{stem}{suffix} was removed"
            );
        }
    }
    for u in FOREIGN {
        assert!(c.present(u), "{u} was removed");
        assert!(
            !log.iter().any(|l| l.contains(u)),
            "{u} reached systemctl:\n{}",
            log.join("\n")
        );
    }
    // The record states the order: timer before service for each stem,
    // and the after-snapshot and the verdict are there.
    for stem in not_in_role_stems() {
        let t = text
            .find(&format!("disabled+removed {stem}.timer"))
            .unwrap();
        let s = text
            .find(&format!("disabled+removed {stem}.service"))
            .unwrap();
        assert!(t < s, "{stem}: the service went before the timer:\n{text}");
    }
    contains_all(&text, &["before", "after", "OK"], "the real run's record");
}

/// A not-in-role unit that is not on the host — no file, not loaded —
/// is reported by name and skipped; the rest are removed. The second
/// run of the verb is exactly this for every unit, and it is not an
/// error: a converged host is the goal, not a refusal.
#[test]
fn a_unit_absent_from_the_host_is_reported_not_refused() {
    let c = Case::new("absent");
    let gone = &not_in_role_stems()[1];
    for suffix in [".timer", ".service"] {
        std::fs::remove_file(c.etc.join(format!("{gone}{suffix}"))).unwrap();
    }
    std::fs::remove_dir_all(c.etc.join(format!("{gone}.service.d"))).unwrap();
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{text}");
    contains_all(
        &text,
        &[
            &format!("not on this host {gone}.timer"),
            &format!("not on this host {gone}.service"),
        ],
        "the record",
    );
    let expected: Vec<String> = expected_units()
        .into_iter()
        .filter(|u| !u.starts_with(&format!("{gone}.")))
        .collect();
    assert_eq!(sorted(&c.disabled()), sorted(&expected));

    // Run again: everything is gone now, nothing is planned, and that
    // is an OK with an empty plan, not a refusal.
    std::fs::remove_file(&c.log).unwrap();
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(
        rc, 0,
        "a second run on a converged host did not exit 0:\n{text}"
    );
    assert!(
        c.disabled().is_empty(),
        "a second run disabled something:\n{text}"
    );
    contains_all(&text, &["plan: 0", "OK"], "the second run's record");
}

/// A disable that fails mid-way exits 1 and the record names what was
/// already removed and where it stopped — nothing after it is touched,
/// and no reload claims a state that was not reached.
#[test]
fn a_failed_disable_exits_1_and_states_what_was_done() {
    let c = Case::new("mid-way");
    let (_, plan_text) = c.run(&["--dry-run"]);
    let planned: Vec<String> = plan_text
        .lines()
        .filter_map(|l| l.strip_prefix("would disable+remove ").map(str::to_string))
        .collect();
    let victim = planned[3].clone();
    let (rc, text) = c.run_env(&["--for-real"], &[("STUB_FAIL_UNIT", victim.clone())]);
    assert_eq!(rc, 1, "{text}");
    assert_eq!(
        c.disabled(),
        planned[..4].to_vec(),
        "the run went past the failure:\n{text}"
    );
    for u in &planned[..3] {
        assert!(
            !c.present(u),
            "{u} was reported removed but is still on disk"
        );
    }
    for u in &planned[3..] {
        assert!(c.present(u), "{u} was removed after the failure");
    }
    contains_all(
        &text,
        &[
            "FAILED",
            &victim,
            "already disabled+removed",
            &planned[0],
            &planned[2],
            "not touched",
            &planned[4],
        ],
        "the mid-way record",
    );
}

// ---------------------------------------------------------------------------
// THROUGH THE RUNNER, with the real allowlist, as boss-gcp.
// ---------------------------------------------------------------------------

/// The real `infra/ops/verbs/` copied verbatim — the allowlist under
/// test is the one that ships.
fn shipped_verbs(root: &Path) -> PathBuf {
    let dst = root.join("verbs");
    std::fs::create_dir_all(&dst).unwrap();
    for e in std::fs::read_dir(repo_root().join("infra/ops/verbs")).expect("infra/ops/verbs/") {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "json") {
            std::fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
        }
    }
    dst
}

/// One open ops-request for boss-gcp carrying the verb and args, run
/// through `ops-runner.sh` against a stubbed system of record. The
/// stub `curl` answers the jobs read with the packet, records the PUT,
/// and answers the estate-nodes read the script makes.
fn run_runner(c: &Case, verbs: &Path, args: &str) -> (String, Option<serde_json::Value>) {
    write_exec(
        &c.bin.join("curl"),
        "#!/bin/sh\n\
         for a in \"$@\"; do case \"$a\" in @*) cp \"${a#@}\" \"$STUB_PUT\"; exit 0;; esac; done\n\
         for a in \"$@\"; do case \"$a\" in */api/estate/nodes*) cat \"$STUB_NODES\"; exit 0;; esac; done\n\
         cat \"$STUB_JOBS\"\n",
    );
    write_file(
        &c.root.join("jobs.json"),
        &format!(
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"boss-gcp","verb":"uninstall-not-in-role","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}]}}"#
        ),
    );
    let put = c.root.join("put.json");
    let _ = std::fs::remove_file(&put);
    let mut cmd = Command::new("sh");
    cmd.arg(repo_root().join("infra/ops/ops-runner.sh"));
    c.env(&mut cmd);
    cmd.env("HOST_ID", "boss-gcp")
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        .env("OPS_VERBS_DIR", verbs)
        .env("STUB_JOBS", c.root.join("jobs.json"))
        .env("STUB_PUT", &put);
    let out = cmd.output().expect("ops-runner.sh runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let meta = std::fs::read_to_string(&put)
        .ok()
        .map(|s| serde_json::from_str::<serde_json::Value>(&s).expect("PUT payload is JSON"))
        .map(|v| v["metadata"].clone());
    (text, meta)
}

/// The allowlist's own validation: no mode, or a word outside the two
/// literals, never reaches the script, and the refusal names the param.
#[test]
fn the_allowlist_refuses_a_missing_or_foreign_mode() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new("runner-refuses");
    let verbs = shipped_verbs(&c.root);
    let before = c.etc_listing();
    for (args, needle) in [
        ("[]", "missing required arg mode"),
        (r#"["--now"]"#, "is not one of"),
    ] {
        let (text, meta) = run_runner(&c, &verbs, args);
        let meta = meta.expect("the runner completed the execute step");
        assert_eq!(
            meta["disposition"], "refused",
            "{args} was not refused:\n{text}"
        );
        let reason = meta["reason"].as_str().unwrap_or_default();
        assert!(reason.contains(needle), "{args}: {reason}");
    }
    assert!(c.log_lines().is_empty(), "a refused packet acted");
    assert_eq!(c.etc_listing(), before);
}

/// And `--dry-run` reaches the script through the runner ON boss-gcp,
/// with the script resolved against the runner's own checkout — the
/// verb is exercisable end to end through the audited door without
/// removing anything.
#[test]
fn the_runner_on_boss_gcp_answers_a_dry_run() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new("runner-dry-run");
    let verbs = shipped_verbs(&c.root);
    let before = c.etc_listing();
    let (text, meta) = run_runner(&c, &verbs, r#"["--dry-run"]"#);
    let meta = meta.expect("the runner completed the execute step");
    assert_eq!(
        meta["disposition"], "answered",
        "the dry run was not answered:\n{text}"
    );
    assert_eq!(
        meta["exit_code"], "0",
        "the dry run did not exit 0:\n{text}"
    );
    let output = meta["output"].as_str().unwrap_or_default();
    let first = &not_in_role_stems()[0];
    contains_all(
        output,
        &[
            "DRY RUN",
            "uninstall-not-in-role: plan:",
            &format!("would disable+remove {first}.timer"),
        ],
        "the packet's output",
    );
    assert!(c.log_lines().is_empty(), "a dry run acted:\n{text}");
    assert_eq!(c.etc_listing(), before, "a dry run changed the unit files");
}
