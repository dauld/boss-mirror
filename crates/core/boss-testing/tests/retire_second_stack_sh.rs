//! `infra/gcp/retire-second-stack.sh` is RUN, not read — against a
//! stubbed `systemctl`, a stubbed `pg_dump` and a stubbed system of
//! record, so every verdict below is one the script actually reached.
//!
//! WHY THE VERB EXISTS (design 9e3e093f, decided by David 2026-09-11;
//! backlog d5941ef3 car 2). boss-gcp carries a second, older, complete
//! BOSS stack — 32 running services, one of them serving a crate deleted
//! from the tree — plus the ten legacy-stack chores, six of them failed
//! for three weeks. Stopping it by hand is 54 `systemctl` invocations on
//! a console with nothing recording which, or in what order, or what
//! the database held the moment before. So the retirement is an ops
//! verb: bounded to a list the TREE carries, refusing anything outside
//! it, capturing the database before it stops anything, and printing
//! every unit it touched in the order it touched them, onto the packet.
//!
//! What each case pins: the dry run stops nothing and plans exactly the
//! tree's list; a unit outside the list's shape, or in the KEEP set, is
//! refused by name; the real run refuses when the capture fails; the
//! real run disables exactly the listed units, in order, and reports
//! it; the keep set never appears in the stop list; the tree's list
//! agrees with `infra/estate/roles.toml`; and the allowlist's own
//! validation, exercised THROUGH `ops-runner.sh` with the real verb
//! files, as the runner on boss-gcp would.
//!
//! Nothing here touches a host. `systemctl` and `pg_dump` are stubs on
//! every path, and the units a `disable --now` receives are appended to
//! a file.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

const SCRIPT: &str = "infra/gcp/retire-second-stack.sh";
const LIST: &str = "infra/gcp/second-stack-units.txt";
const PASSWORD: &str = "s3cretpw";

/// The units the tree's list names, in the tree's order, comments and
/// blanks dropped — the same reading the script does.
fn tree_list() -> Vec<String> {
    std::fs::read_to_string(repo_root().join(LIST))
        .expect("the tree carries infra/gcp/second-stack-units.txt")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// The stems `infra/estate/roles.toml` names under one section header,
/// read the way `deploy-services.sh`'s `role_units` reads them: each
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

/// One fixture: a list-units snapshot, a stub `systemctl` that records
/// every `disable --now`, a stub `pg_dump`, a stub `curl` answering the
/// estate registry, and a backup directory.
struct Case {
    root: PathBuf,
    bin: PathBuf,
    units_file: PathBuf,
    stopped: PathBuf,
    dumps: PathBuf,
    backups: PathBuf,
    nodes: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("retire-second-stack-{name}"));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let units_file = root.join("units.txt");
        let stopped = root.join("stopped.log");
        let dumps = root.join("pg_dump.log");
        let backups = root.join("backups");
        let nodes = root.join("nodes.json");

        // The snapshot systemd would print: every unit the tree lists,
        // loaded and active, plus the host's own loops (which the list
        // must never name) and one unit nobody listed. A `disable --now`
        // the stub has recorded turns that unit inactive on the next
        // listing, so the after-snapshot is honest.
        let mut snapshot = String::new();
        for u in tree_list() {
            let sub = if u.ends_with(".timer") {
                "waiting"
            } else {
                "running"
            };
            snapshot.push_str(&format!("{u} loaded active {sub} second stack\n"));
        }
        for u in [
            "boss-ops-runner.timer",
            "boss-ops-runner.service",
            "boss-gcp-converge.timer",
            "boss-gcp-converge.service",
            "boss-estate-observe-units.timer",
            "boss-estate-observe-host.timer",
            "boss-codebase-metrics.timer",
            "boss-ml-inference-batch.timer",
            "boss-nobody-listed-me.service",
        ] {
            snapshot.push_str(&format!("{u} loaded active running host loop\n"));
        }
        write_file(&root.join("snapshot.txt"), &snapshot);

        write_exec(
            &bin.join("systemctl"),
            r#"#!/bin/sh
# stub systemctl: list-units reads the snapshot (units already stopped
# show inactive); show prints the dispatcher's Environment; disable --now
# and reset-failed append to the log; STUB_FAIL_UNIT fails its disable.
case "$1" in
  list-units)
    while read -r unit rest; do
      if [ -f "$STUB_STOPPED" ] && grep -qxF "disable --now $unit" "$STUB_STOPPED"; then
        echo "$unit loaded inactive dead ${rest#loaded active * }"
      else
        echo "$unit $rest"
      fi
    done < "$STUB_SNAPSHOT"
    ;;
  show)
    unit="$5"
    case "$unit" in
      boss-dispatcher.service) echo "RUST_LOG=info BOSS_POSTGRES_URL=$STUB_PG_URL" ;;
      *) echo "RUST_LOG=info" ;;
    esac
    ;;
  disable)
    # systemctl disable --now [--] <unit>
    shift 2; [ "$1" = "--" ] && shift
    echo "disable --now $1" >> "$STUB_STOPPED"
    if [ "$1" = "${STUB_FAIL_UNIT:-}" ]; then
      echo "Failed to stop $1: Job for $1 failed (stub)" >&2
      exit 1
    fi
    ;;
  reset-failed)
    shift; [ "$1" = "--" ] && shift
    echo "reset-failed $1" >> "$STUB_STOPPED"
    ;;
  *) echo "stub systemctl: unexpected $*" >&2; exit 99 ;;
esac
"#,
        );
        write_exec(
            &bin.join("pg_dump"),
            r#"#!/bin/sh
# stub pg_dump: records its argv, writes a dump to --file unless
# STUB_PG_FAIL is set.
echo "$*" >> "$STUB_DUMPS"
[ -n "${STUB_PG_FAIL:-}" ] && { echo "pg_dump: error: connection to server failed (stub)" >&2; exit 1; }
out=""
for a in "$@"; do case "$a" in --file=*) out="${a#--file=}";; esac; done
[ -n "$out" ] && printf -- '-- PostgreSQL database dump (stub)\nCREATE TABLE t ();\n' > "$out"
exit 0
"#,
        );
        write_exec(
            &bin.join("curl"),
            "#!/bin/sh\n# stub curl: the estate registry's /api/estate/nodes\ncat \"$STUB_NODES\"\n",
        );
        write_file(
            &nodes,
            r#"{"data":[{"id":"boss-gcp","roles":["wireguard-bastion","off-cluster-observer","ml-batch-host"]},{"id":"forge","roles":["cluster-operator"]}]}"#,
        );
        std::fs::copy(repo_root().join(LIST), &units_file).unwrap();
        Self {
            root,
            bin,
            units_file,
            stopped,
            dumps,
            backups,
            nodes,
        }
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .args(args)
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_SNAPSHOT", self.root.join("snapshot.txt"))
            .env("STUB_STOPPED", &self.stopped)
            .env("STUB_DUMPS", &self.dumps)
            .env("STUB_NODES", &self.nodes)
            .env(
                "STUB_PG_URL",
                format!("postgres://boss:{PASSWORD}@127.0.0.1/boss"),
            )
            .env("BOSS_RETIRE_UNITS_FILE", &self.units_file)
            .env("BOSS_RETIRE_BACKUP_DIR", &self.backups)
            .env(
                "BOSS_ESTATE_NODES_URL",
                "http://sor.invalid/api/estate/nodes",
            )
            // The roles reader remembers a live read beside the host's
            // state (/var/lib/boss by default); a test's read stays in
            // its own scratch.
            .env("BOSS_NODE_ROLES_CACHE", self.root.join("roles.cache"));
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("retire-second-stack.sh runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    /// Every `disable --now <unit>` the stub received, in order.
    fn stopped(&self) -> Vec<String> {
        std::fs::read_to_string(&self.stopped)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix("disable --now "))
            .map(str::to_string)
            .collect()
    }

    fn dump_calls(&self) -> String {
        std::fs::read_to_string(&self.dumps).unwrap_or_default()
    }
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(text.contains(n), "{what}: expected `{n}` in:\n{text}");
    }
}

// ---------------------------------------------------------------------------
// The list the tree carries.
// ---------------------------------------------------------------------------

/// The reviewed list is well-formed: boss-* units only, no duplicates,
/// timers before services (a service disabled while its timer still
/// waits is started again at the next elapse), and it names the units
/// the inventory of 2026-09-12 22Z found running — including the two
/// the brief singles out.
#[test]
fn the_tree_list_is_well_formed_and_names_the_inventory() {
    let list = tree_list();
    assert!(list.len() >= 50, "{} units listed", list.len());
    let mut seen = std::collections::BTreeSet::new();
    let mut last_timer = 0;
    let mut first_service = usize::MAX;
    for (i, u) in list.iter().enumerate() {
        assert!(
            u.starts_with("boss-") && (u.ends_with(".service") || u.ends_with(".timer")),
            "{u} is not a boss-*.service or boss-*.timer"
        );
        assert!(
            u.trim_end_matches(".service")
                .trim_end_matches(".timer")
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{u} carries a character outside [a-z0-9-]"
        );
        assert!(seen.insert(u.clone()), "{u} is listed twice");
        if u.ends_with(".timer") {
            last_timer = i;
        } else {
            first_service = first_service.min(i);
        }
    }
    assert!(
        last_timer < first_service,
        "a timer is listed after a service — timers first, or the timer restarts what was stopped"
    );
    contains_all(
        &list.join("\n"),
        &[
            "boss-docs-api.service",
            "boss-pr-train-reconcile.service",
            "boss-pr-train-reconcile.timer",
            "boss-jobs-api.service",
            "boss-gateway.service",
            "boss-backup.timer",
            "boss-backup.service",
        ],
        "the list",
    );
}

/// The list and `infra/estate/roles.toml` are two statements about the
/// same units, so they get an equality test (CLAUDE.md §9a): every
/// legacy-stack stem is retired as both its timer and its service, and
/// no stem any OTHER section names — `always`, or a role boss-gcp
/// declares — is in the list at all.
#[test]
fn the_list_agrees_with_the_roles_registry() {
    let list = tree_list();
    let legacy = role_units("roles.legacy-stack");
    assert!(legacy.len() >= 8, "legacy-stack names {legacy:?}");
    for stem in &legacy {
        for suffix in [".timer", ".service"] {
            let unit = format!("{stem}{suffix}");
            assert!(
                list.contains(&unit),
                "{unit} is a legacy-stack unit (infra/estate/roles.toml) but {LIST} does not retire it"
            );
        }
    }
    let toml = std::fs::read_to_string(repo_root().join("infra/estate/roles.toml")).unwrap();
    let sections: Vec<&str> = toml
        .lines()
        .filter_map(|l| l.trim().strip_prefix('[').and_then(|s| s.strip_suffix(']')))
        .filter(|s| *s != "roles.legacy-stack")
        .collect();
    assert!(sections.contains(&"always"), "{sections:?}");
    for section in sections {
        for stem in role_units(section) {
            for suffix in [".timer", ".service"] {
                let unit = format!("{stem}{suffix}");
                assert!(
                    !list.contains(&unit),
                    "{unit} is named by [{section}] in infra/estate/roles.toml — a unit this host \
                     keeps — and {LIST} would retire it"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The script, over stubs.
// ---------------------------------------------------------------------------

/// `--dry-run` plans exactly the tree's list, in the tree's order,
/// rehearses the capture without writing a dump, and stops nothing.
#[test]
fn dry_run_plans_the_tree_list_and_stops_nothing() {
    let c = Case::new("dry-run");
    let (rc, text) = c.run(&["--dry-run"]);
    assert_eq!(rc, 0, "dry run did not exit 0:\n{text}");
    assert!(
        c.stopped().is_empty(),
        "a dry run stopped something:\n{text}"
    );
    assert!(
        !c.backups.exists() || std::fs::read_dir(&c.backups).unwrap().next().is_none(),
        "a dry run wrote a dump"
    );
    let planned: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("would stop+disable "))
        .collect();
    assert_eq!(
        planned,
        tree_list().iter().map(String::as_str).collect::<Vec<_>>(),
        "the plan is not the tree's list in the tree's order:\n{text}"
    );
    contains_all(
        &text,
        &[
            "DRY RUN",
            "boss-nobody-listed-me.service", // in the snapshot the packet holds
            "postgres://boss:***@127.0.0.1/boss", // the database, redacted
            "--schema-only",                 // the rehearsal
        ],
        "the dry run's record",
    );
    assert!(
        !planned.contains(&"boss-nobody-listed-me.service"),
        "an unlisted unit reached the plan"
    );
    assert!(
        !text.contains(PASSWORD),
        "the database credential was printed:\n{text}"
    );
    for keep in [
        "boss-ops-runner.timer",
        "boss-ops-runner.service",
        "boss-gcp-converge.timer",
        "boss-gcp-converge.service",
        "boss-estate-observe-units.timer",
        "boss-estate-observe-host.timer",
        "boss-codebase-metrics.timer",
        "boss-ml-inference-batch.timer",
    ] {
        assert!(
            !planned.contains(&keep),
            "{keep} is a kept unit and reached the plan"
        );
        assert!(
            text.contains(&format!("keep {keep}")),
            "the record does not say it keeps {keep}:\n{text}"
        );
    }
}

/// A list that names a unit outside the shape this verb touches
/// (`boss-*.service` / `boss-*.timer`) is refused by that name, before
/// anything is read from the host.
#[test]
fn refuses_a_list_naming_a_unit_outside_its_shape() {
    let c = Case::new("outside-shape");
    write_file(
        &c.units_file,
        "boss-docs-api.service\nwg-quick@wg0.service\nboss-gateway.service\n",
    );
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "not refused:\n{text}");
    contains_all(&text, &["REFUSED", "wg-quick@wg0.service"], "the refusal");
    assert!(
        c.stopped().is_empty(),
        "a refused run stopped something:\n{text}"
    );
    assert_eq!(c.dump_calls(), "", "a refused run dumped the database");
}

/// A list that names one of the host's own loops — the KEEP set — is
/// refused by that name, on the dry run and the real run alike.
#[test]
fn refuses_a_list_naming_a_kept_unit() {
    let c = Case::new("keep-set");
    for keep in [
        "boss-ops-runner.service",
        "boss-gcp-converge.timer",
        "boss-ml-inference-batch.service",
    ] {
        write_file(&c.units_file, &format!("boss-docs-api.service\n{keep}\n"));
        for mode in ["--dry-run", "--for-real"] {
            let (rc, text) = c.run(&[mode]);
            assert_eq!(rc, 2, "{keep} under {mode} was not refused:\n{text}");
            contains_all(&text, &["REFUSED", keep, "KEEP"], "the refusal");
        }
    }
    assert!(c.stopped().is_empty(), "a refused run stopped something");
}

/// The mode argument is required and is one of two literal words; the
/// script re-checks that itself rather than relying on the allowlist.
#[test]
fn refuses_a_missing_or_unknown_mode() {
    let c = Case::new("mode");
    for args in [&[][..], &["--now"][..], &["--dry-run", "extra"][..]] {
        let (rc, text) = c.run(args);
        assert_eq!(rc, 2, "{args:?} was not refused:\n{text}");
        contains_all(&text, &["--dry-run", "--for-real"], "the usage");
    }
    assert!(c.stopped().is_empty());
}

/// The real run refuses when the capture fails: a stack whose database
/// could not be dumped is not stopped, and the record says so.
#[test]
fn the_real_run_refuses_when_the_capture_fails() {
    let c = Case::new("capture-fails");
    let (rc, text) = c.run_env(&["--for-real"], &[("STUB_PG_FAIL", "1".into())]);
    assert_eq!(rc, 2, "not refused:\n{text}");
    contains_all(
        &text,
        &["REFUSED", "capture", "nothing was stopped"],
        "the refusal",
    );
    assert!(
        c.stopped().is_empty(),
        "a failed capture was followed by a stop:\n{text}"
    );
    assert!(
        !text.contains(PASSWORD),
        "the credential was printed:\n{text}"
    );
}

/// The real run refuses while the host still declares `legacy-stack`:
/// the converge would re-enable every chore timer at its next tick, so a
/// stop now would be reverted and the record would lie.
#[test]
fn the_real_run_refuses_while_the_host_declares_legacy_stack() {
    let c = Case::new("legacy-role");
    write_file(
        &c.nodes,
        r#"{"data":[{"id":"boss-gcp","roles":["wireguard-bastion","legacy-stack"]}]}"#,
    );
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "not refused:\n{text}");
    contains_all(
        &text,
        &["REFUSED", "legacy-stack", "converge"],
        "the refusal",
    );
    assert!(c.stopped().is_empty());
    assert_eq!(
        c.dump_calls(),
        "",
        "the database was dumped before the role bound"
    );

    // And when the registry cannot be read at all, the bound cannot be
    // evaluated, which is a refusal — never a pass. The reader's own
    // fallback (a cached declaration, else the sentinel `registry-unread`
    // — non-empty, so an emptiness check alone would pass it) is for a
    // converge; this verb asks where the roles CAME FROM and acts only
    // on a live answer.
    write_file(&c.nodes, "");
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "an unreadable registry was not a refusal:\n{text}");
    contains_all(&text, &["REFUSED", "roles"], "the refusal");
    assert!(c.stopped().is_empty());

    // The same dark registry with a cached declaration that would pass
    // the bound: still a refusal, because the cache is not a live read.
    std::fs::write(
        c.root.join("roles.cache"),
        "wireguard-bastion,ml-batch-host\n",
    )
    .unwrap();
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "a cached declaration passed the bound:\n{text}");
    contains_all(&text, &["REFUSED", "roles"], "the refusal");
    assert!(c.stopped().is_empty());
    assert_eq!(
        c.dump_calls(),
        "",
        "the database was dumped on a cached reading"
    );
}

/// The real run captures first, then disables exactly the listed units
/// that are present, in the list's order, resets their failed state,
/// verifies none is still active, and exits 0.
#[test]
fn the_real_run_captures_then_disables_exactly_the_list_in_order() {
    let c = Case::new("for-real");
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "the real run did not exit 0:\n{text}");
    assert_eq!(
        c.stopped(),
        tree_list(),
        "the units disabled are not the tree's list in the tree's order:\n{text}"
    );
    // Capture before stop: the dump was called, wrote a file under the
    // backup dir, and the credential never reached the record.
    let dumps = c.dump_calls();
    assert!(
        dumps.contains("--file="),
        "pg_dump was not asked for a file: {dumps}"
    );
    assert!(
        dumps.contains(PASSWORD),
        "pg_dump did not receive the connection string: {dumps}"
    );
    let files: Vec<_> = std::fs::read_dir(&c.backups)
        .expect("the backup dir was created")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 1, "expected one dump, got {files:?}");
    assert!(
        files[0].starts_with("second-stack-") && files[0].ends_with(".sql"),
        "{files:?}"
    );
    assert!(
        !text.contains(PASSWORD),
        "the credential was printed:\n{text}"
    );
    contains_all(
        &text,
        &[
            "captured",
            &files[0],
            "stopped+disabled boss-docs-api.service",
            "stopped+disabled boss-jobs-api.service",
            "OK",
        ],
        "the real run's record",
    );
    // The record states the order: the first unit stopped is the first
    // listed, and it appears before the last listed in the text.
    let first = text
        .find(&format!("stopped+disabled {}", tree_list()[0]))
        .expect("first");
    let last = text
        .find(&format!("stopped+disabled {}", tree_list().last().unwrap()))
        .expect("last");
    assert!(first < last, "the record is not in stop order:\n{text}");
    // A kept unit was never touched.
    let log = std::fs::read_to_string(&c.stopped).unwrap();
    for keep in [
        "boss-ops-runner",
        "boss-gcp-converge",
        "boss-estate-observe",
        "boss-codebase-metrics",
        "boss-ml-inference-batch",
        "boss-nobody-listed-me",
    ] {
        assert!(!log.contains(keep), "{keep} reached systemctl:\n{log}");
    }
}

/// A listed unit that is not on the host is reported by name and
/// skipped; the rest are retired.
#[test]
fn a_listed_unit_absent_from_the_host_is_reported_not_refused() {
    let c = Case::new("absent");
    let snapshot = std::fs::read_to_string(c.root.join("snapshot.txt")).unwrap();
    let trimmed: String = snapshot
        .lines()
        .filter(|l| !l.starts_with("boss-docs-api.service"))
        .map(|l| format!("{l}\n"))
        .collect();
    write_file(&c.root.join("snapshot.txt"), &trimmed);
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{text}");
    assert!(
        text.contains("not on this host boss-docs-api.service"),
        "{text}"
    );
    let expected: Vec<String> = tree_list()
        .into_iter()
        .filter(|u| u != "boss-docs-api.service")
        .collect();
    assert_eq!(c.stopped(), expected);
}

/// A stop that fails mid-way exits 1 and the record names what was
/// already done and where it stopped — nothing after it is touched.
#[test]
fn a_failed_stop_exits_1_and_states_what_was_done() {
    let c = Case::new("mid-way");
    let list = tree_list();
    let victim = &list[3];
    let (rc, text) = c.run_env(&["--for-real"], &[("STUB_FAIL_UNIT", victim.clone())]);
    assert_eq!(rc, 1, "{text}");
    assert_eq!(
        c.stopped(),
        list[..4].to_vec(),
        "the stop went past the failure:\n{text}"
    );
    contains_all(
        &text,
        &[
            "FAILED",
            victim,
            "already stopped+disabled",
            &list[0],
            &list[2],
            "not touched",
            &list[4],
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
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"boss-gcp","verb":"retire-second-stack","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}]}}"#
        ),
    );
    let put = c.root.join("put.json");
    let _ = std::fs::remove_file(&put);
    let out = Command::new("sh")
        .arg(repo_root().join("infra/ops/ops-runner.sh"))
        .env_clear()
        .env(
            "PATH",
            format!(
                "{}:{}",
                c.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOST_ID", "boss-gcp")
        .env("BOSS_NODE_ROLES_CACHE", c.root.join("roles.cache"))
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        .env("OPS_VERBS_DIR", verbs)
        .env("STUB_JOBS", c.root.join("jobs.json"))
        .env("STUB_PUT", &put)
        .env("STUB_SNAPSHOT", c.root.join("snapshot.txt"))
        .env("STUB_STOPPED", &c.stopped)
        .env("STUB_DUMPS", &c.dumps)
        .env("STUB_NODES", &c.nodes)
        .env(
            "STUB_PG_URL",
            format!("postgres://boss:{PASSWORD}@127.0.0.1/boss"),
        )
        .env("BOSS_RETIRE_UNITS_FILE", &c.units_file)
        .env("BOSS_RETIRE_BACKUP_DIR", &c.backups)
        .output()
        .expect("ops-runner.sh runs");
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
        assert!(
            c.stopped().is_empty(),
            "a refused packet stopped something:\n{text}"
        );
    }
}

/// And `--dry-run` reaches the script through the runner ON boss-gcp,
/// with the script resolved against the runner's own checkout — the
/// verb is exercisable end to end through the audited door without
/// stopping anything.
#[test]
fn the_runner_on_boss_gcp_answers_a_dry_run() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new("runner-dry-run");
    let verbs = shipped_verbs(&c.root);
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
    contains_all(
        output,
        &["DRY RUN", "would stop+disable boss-docs-api.service"],
        "the packet's output",
    );
    assert!(
        !output.contains(PASSWORD),
        "the credential reached the packet:\n{output}"
    );
    assert!(
        c.stopped().is_empty(),
        "a dry run stopped something:\n{text}"
    );
}
