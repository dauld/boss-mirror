//! `infra/gcp/retire-cloudflared.sh` is RUN, not read — against a
//! stubbed `systemctl`, a scratch `/etc/systemd/system` and a stubbed
//! system of record, so every verdict below is one the script actually
//! reached.
//!
//! WHY THE VERB EXISTS (design 4c565f8c, David 2026-09-16: "agreed,
//! fold it in and file the cars"; backlog 0b7804f3, car 4). boss-gcp
//! has run a hand-written `cloudflared.service` since 2026-06-09 — its
//! tunnel token inline on the ExecStart line, which is how the token
//! reached the system of record when the unit was read through
//! unit-cat (9c760dd7). The in-cluster connector (car 1) now carries
//! both hostnames: the converge of 2026-09-16 08:25Z recorded
//! `cloudflared: connected` and a `tunnel_ingress` naming
//! boss.algedonic.dev and playground.algedonic.dev. The VM connector is
//! redundancy nobody chose, and the leak class is closed on this host
//! by there being no token there. So the retirement is an ops verb:
//! it verifies the hand-over through the system of record BEFORE it
//! stops anything, snapshots the unit with the token masked, stops,
//! disables and removes exactly that one unit and its drop-ins, and
//! refuses success while the unit is still loaded. It never touches
//! the tunnel in Cloudflare.
//!
//! What each case pins: the hand-over bound reads the newest converge
//! that OBSERVED the connector and refuses when there is none, when it
//! is stale, when the connector was not ready, and when the ingress
//! does not name every hostname the tree declares; the dry run prints
//! the plan and the masked unit and changes nothing; the real run
//! stops, removes, reloads, verifies and reports with the token masked
//! everywhere; a unit systemd still holds after the removal is a
//! failure, not an OK; the second run is an OK with nothing to do; and
//! the allowlist's own validation, exercised THROUGH `ops-runner.sh`
//! with the real verb files, as the runner on boss-gcp would.
//!
//! Nothing here touches a host or Cloudflare. `systemctl` and `curl`
//! are stubs on every path and the unit files live in a scratch
//! directory this process owns.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

const SCRIPT: &str = "infra/gcp/retire-cloudflared.sh";
const UNIT: &str = "cloudflared.service";
/// A token-shaped value that must never appear in any record the verb
/// prints. Not a real credential; the shape is what the mask matches.
const TOKEN: &str = "eyJhIjoiZmFrZSIsInQiOiJmYWtlIiwicyI6ImZha2UifQ";
/// The unit as `cloudflared service install` writes it, as read off
/// boss-gcp on 2026-09-16 (token replaced).
fn unit_text() -> String {
    format!(
        "[Unit]\nDescription=cloudflared\nAfter=network-online.target\nWants=network-online.target\n\n\
         [Service]\nTimeoutStartSec=0\nType=notify\n\
         ExecStart=/usr/bin/cloudflared --no-autoupdate tunnel run --token {TOKEN}\n\
         Restart=on-failure\nRestartSec=5s\n\n[Install]\nWantedBy=multi-user.target\n"
    )
}

/// Every hostname `infra/cluster/instances.toml` declares — the set the
/// verb demands the converge's `tunnel_ingress` to name, read here the
/// same way (one `hostname = "..."` per instance).
fn declared_hostnames() -> Vec<String> {
    let toml = std::fs::read_to_string(repo_root().join("infra/cluster/instances.toml"))
        .expect("infra/cluster/instances.toml");
    let out: Vec<String> = toml
        .lines()
        .filter_map(|l| l.trim().strip_prefix("hostname"))
        .filter_map(|rest| rest.split('"').nth(1))
        .map(str::to_string)
        .collect();
    assert!(out.len() >= 2, "instances.toml declares {out:?}");
    out
}

fn iso(ago: Duration) -> String {
    let t = SystemTime::now() - ago;
    let secs = t.duration_since(UNIX_EPOCH).unwrap().as_secs();
    // 1970-01-01 + secs, rendered by the shell's date so the test does
    // not carry a calendar.
    let out = Command::new("date")
        .args([
            "-u",
            "-d",
            &format!("@{secs}"),
            "+%Y-%m-%dT%H:%M:%S.123456Z",
        ])
        .output()
        .expect("date");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// One converge packet as the jobs API returns it: the `run` step
/// carries what cluster-deploy-runner recorded. `fields` is the run
/// step's metadata beyond `result`.
fn converge(id: &str, completed_at: &str, fields: &[(&str, &str)]) -> String {
    let mut meta = String::from(r#""authority_role":"platform-admin","result":"ok""#);
    for (k, v) in fields {
        meta.push_str(&format!(r#","{k}":"{v}""#));
    }
    format!(
        r#"{{"id":"{id}","kind":"maintenance-cluster-converge","status":"closed","steps":[{{"id":"{id}-run","spec_slug":"run","status":"completed","completed_at":"{completed_at}","metadata":{{{meta}}}}}]}}"#
    )
}

fn ingress_for(hosts: &[String]) -> String {
    hosts
        .iter()
        .map(|h| format!("{h} → boss"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The converge list the stub SoR answers: a newest no-op tick that
/// carries no `cloudflared` field (`unchanged` alone — the shape every
/// no-op tick had before 0b7804f3, and what an older one still holds),
/// then the deploying converge that read the connector, then an older
/// one. The verb must pick the one that carries a `cloudflared` field,
/// not simply the newest.
fn healthy_converges() -> String {
    let hosts = declared_hostnames();
    let ingress = ingress_for(&hosts);
    format!(
        r#"{{"data":[{},{},{}],"total":3}}"#,
        converge(
            "c0000000-0000-4000-8000-000000000003",
            &iso(Duration::from_secs(5 * 60)),
            &[("unchanged", "cd17a12")],
        ),
        converge(
            "c0000000-0000-4000-8000-000000000002",
            &iso(Duration::from_secs(40 * 60)),
            &[
                ("cloudflared", "connected"),
                ("tunnel_ingress", &ingress),
                ("tunnel_ingress_render", "4be61afbea17 (connector rolled)"),
            ],
        ),
        converge(
            "c0000000-0000-4000-8000-000000000001",
            &iso(Duration::from_secs(90 * 60)),
            &[("cloudflared", "not-ready")],
        ),
    )
}

struct Case {
    root: PathBuf,
    bin: PathBuf,
    etc: PathBuf,
    log: PathBuf,
    converges: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("retire-cloudflared-{name}"));
        let bin = root.join("bin");
        let etc = root.join("etc");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&etc).unwrap();
        let log = root.join("systemctl.log");
        let converges = root.join("converges.json");

        write_file(&etc.join(UNIT), &unit_text());
        let d = etc.join(format!("{UNIT}.d"));
        std::fs::create_dir_all(&d).unwrap();
        write_file(
            &d.join("override.conf"),
            "[Service]\nEnvironment=TUNNEL_LOGLEVEL=info\n",
        );
        // Units the verb must never touch, whatever else is on the host.
        for u in [
            "boss-ops-runner.service",
            "boss-ops-runner.timer",
            "wg-quick@wg0.service",
            "caddy.service",
        ] {
            write_file(&etc.join(u), "[Unit]\nDescription=foreign\n");
        }

        write_exec(
            &bin.join("systemctl"),
            r##"#!/bin/sh
# stub systemctl: `cat` prints the unit and its drop-ins from the
# scratch etc (or "No files found", exit 1, when absent); `list-units`
# lists cloudflared.service as loaded+active while its file exists
# (STUB_STILL_LOADED keeps it loaded after the file is gone, as a
# systemd that was not reloaded would); disable --now, daemon-reload
# and reset-failed append to the log; STUB_FAIL_UNIT fails its disable.
case "$1" in
  cat)
    shift; [ "$1" = "--no-pager" ] && shift; [ "$1" = "--" ] && shift
    u="$1"
    if [ ! -f "$STUB_ETC/$u" ]; then
      echo "No files found for $u." >&2; exit 1
    fi
    echo "# $STUB_ETC/$u"; cat "$STUB_ETC/$u"
    for f in "$STUB_ETC/$u.d"/*.conf; do
      [ -e "$f" ] || continue
      echo; echo "# $f"; cat "$f"
    done
    ;;
  list-units)
    if [ -f "$STUB_ETC/cloudflared.service" ] || [ -n "${STUB_STILL_LOADED:-}" ]; then
      echo "cloudflared.service loaded active running cloudflared"
    fi
    ;;
  disable)
    shift 2; [ "$1" = "--" ] && shift
    echo "disable --now $1" >> "$STUB_LOG"
    if [ "$1" = "${STUB_FAIL_UNIT:-}" ]; then
      echo "Failed to disable $1: stub" >&2; exit 1
    fi
    ;;
  daemon-reload) echo "daemon-reload" >> "$STUB_LOG" ;;
  reset-failed) shift; [ "$1" = "--" ] && shift; echo "reset-failed $1" >> "$STUB_LOG" ;;
  *) echo "stub systemctl: unexpected $*" >&2; exit 99 ;;
esac
"##,
        );
        // stub curl: the jobs API. A read with no x-boss-user header
        // answers an empty envelope, as the real server does for an
        // unidentified reader (total 0 is a denied scope, not data).
        write_exec(
            &bin.join("curl"),
            r#"#!/bin/sh
signed=""
for a in "$@"; do case "$a" in x-boss-user:*) signed=1;; esac; done
for a in "$@"; do case "$a" in *kind=maintenance-cluster-converge*)
  echo "$*" >> "$STUB_CURL_LOG"
  if [ -z "$signed" ]; then echo '{"data":[],"total":0}'; exit 0; fi
  [ -n "${STUB_SOR_DARK:-}" ] && { echo "curl: (7) Failed to connect" >&2; exit 7; }
  cat "$STUB_CONVERGES"; exit 0;; esac; done
echo "stub curl: unexpected $*" >&2; exit 99
"#,
        );
        write_file(&converges, &healthy_converges());
        Self {
            root,
            bin,
            etc,
            log,
            converges,
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
            .env("STUB_LOG", &self.log)
            .env("STUB_CURL_LOG", self.root.join("curl.log"))
            .env("STUB_ETC", &self.etc)
            .env("STUB_CONVERGES", &self.converges)
            .env("INSTALL_ETC", &self.etc)
            .env("BOSS_JOBS_URL", "http://sor.invalid");
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
        let out = cmd.output().expect("retire-cloudflared.sh runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    fn log_lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn disabled(&self) -> Vec<String> {
        self.log_lines()
            .into_iter()
            .filter_map(|l| l.strip_prefix("disable --now ").map(str::to_string))
            .collect()
    }

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

fn never_leaks(text: &str, what: &str) {
    assert!(
        !text.contains(TOKEN),
        "{what}: the token reached the record:\n{text}"
    );
}

// ---------------------------------------------------------------------------
// The hand-over bound, read through the system of record.
// ---------------------------------------------------------------------------

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

/// No converge has observed the connector — a dark system of record, an
/// empty list, or a list of no-op ticks only — and the bound cannot be
/// evaluated. A bound that cannot be evaluated is a refusal, never a
/// pass, under both modes.
#[test]
fn refuses_when_no_converge_observed_the_connector() {
    let c = Case::new("no-converge");
    let before = c.etc_listing();
    let ticks_only = format!(
        r#"{{"data":[{}],"total":1}}"#,
        converge(
            "c0000000-0000-4000-8000-000000000009",
            &iso(Duration::from_secs(60)),
            &[("unchanged", "cd17a12")],
        )
    );
    for (label, body, extra) in [
        ("empty", r#"{"data":[],"total":0}"#.to_string(), vec![]),
        ("ticks only", ticks_only, vec![]),
        (
            "dark",
            healthy_converges(),
            vec![("STUB_SOR_DARK", "1".to_string())],
        ),
    ] {
        write_file(&c.converges, &body);
        for mode in ["--dry-run", "--for-real"] {
            let (rc, text) = c.run_env(&[mode], &extra);
            assert_eq!(rc, 2, "{label} under {mode} was not a refusal:\n{text}");
            contains_all(&text, &["REFUSED", "converge"], "the refusal");
            never_leaks(&text, label);
        }
    }
    // And with no system of record named at all: a refusal naming the
    // variable, not a guess at a default.
    write_file(&c.converges, &healthy_converges());
    let (rc, text) = c.run_env(&["--dry-run"], &[("BOSS_JOBS_URL", String::new())]);
    assert_eq!(rc, 2, "an unset BOSS_JOBS_URL was not a refusal:\n{text}");
    contains_all(&text, &["REFUSED", "BOSS_JOBS_URL"], "the refusal");
    assert!(c.log_lines().is_empty(), "a refused run acted");
    assert_eq!(
        c.etc_listing(),
        before,
        "a refused run changed the unit files"
    );
}

/// The newest converge that observed the connector is older than the
/// two-hour ceiling: what it saw is not evidence about now.
#[test]
fn refuses_a_stale_converge() {
    let c = Case::new("stale");
    let before = c.etc_listing();
    let hosts = declared_hostnames();
    write_file(
        &c.converges,
        &format!(
            r#"{{"data":[{}],"total":1}}"#,
            converge(
                "c0000000-0000-4000-8000-000000000002",
                &iso(Duration::from_secs(3 * 3600)),
                &[
                    ("cloudflared", "connected"),
                    ("tunnel_ingress", &ingress_for(&hosts)),
                ],
            )
        ),
    );
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "a stale converge was not a refusal:\n{text}");
    contains_all(
        &text,
        &["REFUSED", "c0000000", "older than"],
        "the refusal names the converge and the ceiling",
    );
    assert!(c.log_lines().is_empty());
    assert_eq!(c.etc_listing(), before);
}

/// The newest observation says the in-cluster connector was not
/// connected — `not-ready`, or `skipped (secret absent: …)` — so the
/// hand-over is not real, whatever an older converge said.
#[test]
fn refuses_when_the_connector_was_not_connected() {
    let c = Case::new("not-ready");
    let before = c.etc_listing();
    let hosts = declared_hostnames();
    for value in [
        "not-ready",
        "skipped (secret absent: cloudflare-tunnel-credentials)",
    ] {
        write_file(
            &c.converges,
            &format!(
                r#"{{"data":[{},{}],"total":2}}"#,
                converge(
                    "c0000000-0000-4000-8000-000000000003",
                    &iso(Duration::from_secs(10 * 60)),
                    &[
                        ("cloudflared", value),
                        ("tunnel_ingress", &ingress_for(&hosts))
                    ],
                ),
                converge(
                    "c0000000-0000-4000-8000-000000000002",
                    &iso(Duration::from_secs(40 * 60)),
                    &[
                        ("cloudflared", "connected"),
                        ("tunnel_ingress", &ingress_for(&hosts))
                    ],
                ),
            ),
        );
        let (rc, text) = c.run(&["--for-real"]);
        assert_eq!(rc, 2, "`{value}` was not a refusal:\n{text}");
        contains_all(
            &text,
            &["REFUSED", "c0000000-0000-4000-8000-000000000003", value],
            "the refusal names the converge and what it saw",
        );
    }
    assert!(c.log_lines().is_empty());
    assert_eq!(c.etc_listing(), before);
}

/// The ingress the converge applied does not route every hostname the
/// tree declares (infra/cluster/instances.toml): a hostname the
/// in-cluster connector does not serve is one the VM connector might
/// still be serving, and the refusal names it.
#[test]
fn refuses_an_ingress_missing_a_declared_hostname() {
    let c = Case::new("ingress");
    let before = c.etc_listing();
    let hosts = declared_hostnames();
    let missing = hosts.last().unwrap().clone();
    let partial: Vec<String> = hosts[..hosts.len() - 1].to_vec();
    write_file(
        &c.converges,
        &format!(
            r#"{{"data":[{}],"total":1}}"#,
            converge(
                "c0000000-0000-4000-8000-000000000002",
                &iso(Duration::from_secs(10 * 60)),
                &[
                    ("cloudflared", "connected"),
                    ("tunnel_ingress", &ingress_for(&partial))
                ],
            )
        ),
    );
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(
        rc, 2,
        "an ingress missing {missing} was not a refusal:\n{text}"
    );
    contains_all(
        &text,
        &["REFUSED", &missing, "tunnel_ingress"],
        "the refusal",
    );
    // No ingress field at all is the same refusal.
    write_file(
        &c.converges,
        &format!(
            r#"{{"data":[{}],"total":1}}"#,
            converge(
                "c0000000-0000-4000-8000-000000000002",
                &iso(Duration::from_secs(10 * 60)),
                &[("cloudflared", "connected")],
            )
        ),
    );
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 2, "{text}");
    contains_all(&text, &["REFUSED", "tunnel_ingress"], "the refusal");
    assert!(c.log_lines().is_empty());
    assert_eq!(c.etc_listing(), before);
}

// ---------------------------------------------------------------------------
// The plan, the run, the record.
// ---------------------------------------------------------------------------

/// `--dry-run` passes every bound, names the converge it read, prints
/// the unit with the token masked and the plan, and changes nothing:
/// no systemctl call that acts, no file touched. The read is signed —
/// an unsigned read of the jobs API sees an empty world.
#[test]
fn dry_run_prints_the_plan_and_the_masked_unit_and_changes_nothing() {
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
    never_leaks(&text, "the dry run's record");
    contains_all(
        &text,
        &[
            "DRY RUN",
            "hand-over: converge c0000000-0000-4000-8000-000000000002",
            "cloudflared: connected",
            "--token <masked by unit-cat>",
            &format!("would stop+disable+remove {UNIT}"),
            &format!("would remove {}", c.etc.join(format!("{UNIT}.d")).display()),
            "Cloudflare",
        ],
        "the dry run's record",
    );
    for h in declared_hostnames() {
        assert!(text.contains(&h), "the record does not name {h}:\n{text}");
    }
    let curl_log = std::fs::read_to_string(c.root.join("curl.log")).unwrap_or_default();
    assert!(
        curl_log.contains("x-boss-user:"),
        "the converge read was not signed:\n{curl_log}"
    );
}

/// The real run stops+disables the unit, removes its file and its
/// drop-in directory, reloads, verifies the unit is gone, and reports
/// it — with the token masked in the before-snapshot and absent from
/// everything after; every other unit file is untouched; nothing is
/// asked of Cloudflare.
#[test]
fn the_real_run_removes_exactly_the_unit_and_masks_the_token() {
    let c = Case::new("for-real");
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "the real run did not exit 0:\n{text}");
    never_leaks(&text, "the real run's record");
    assert_eq!(c.disabled(), vec![UNIT.to_string()], "{text}");
    let log = c.log_lines();
    assert!(
        log.iter().any(|l| l == "daemon-reload"),
        "{}",
        log.join("\n")
    );
    assert!(!c.present(UNIT), "{UNIT} is still on disk");
    assert!(
        !c.etc.join(format!("{UNIT}.d")).exists(),
        "the drop-in directory is still on disk"
    );
    for u in [
        "boss-ops-runner.service",
        "boss-ops-runner.timer",
        "wg-quick@wg0.service",
        "caddy.service",
    ] {
        assert!(c.present(u), "{u} was removed");
        assert!(!log.iter().any(|l| l.contains(u)), "{u} reached systemctl");
    }
    contains_all(
        &text,
        &[
            "--token <masked by unit-cat>",
            &format!("stopped+disabled+removed {UNIT}"),
            "daemon-reload",
            "after",
            "OK",
            "Cloudflare",
        ],
        "the real run's record",
    );
    let before_i = text.find("before").expect("a before-snapshot");
    let stop_i = text.find("stopped+disabled+removed").unwrap();
    assert!(
        before_i < stop_i,
        "the snapshot came after the stop:\n{text}"
    );
    let curl_log = std::fs::read_to_string(c.root.join("curl.log")).unwrap_or_default();
    assert_eq!(
        curl_log.lines().count(),
        1,
        "the real run made more than the one converge read:\n{curl_log}"
    );
}

/// `disable --now` reported success but systemd still lists the unit
/// after the reload: the verb refuses to claim success — exit 1 with
/// the unit named — rather than an OK the after-snapshot contradicts.
#[test]
fn refuses_success_while_the_unit_is_still_loaded() {
    let c = Case::new("still-loaded");
    let (rc, text) = c.run_env(&["--for-real"], &[("STUB_STILL_LOADED", "1".to_string())]);
    assert_eq!(rc, 1, "a still-loaded unit passed as OK:\n{text}");
    contains_all(
        &text,
        &["FAILED", UNIT, "still"],
        "the failure names the unit",
    );
    assert!(!text.contains("OK —"), "{text}");
    never_leaks(&text, "the failure record");
}

/// A disable that fails exits 1, names the unit, and leaves its file
/// where it is: nothing is removed from under a unit that would not
/// stop.
#[test]
fn a_failed_disable_exits_1_and_leaves_the_file() {
    let c = Case::new("disable-fails");
    let (rc, text) = c.run_env(&["--for-real"], &[("STUB_FAIL_UNIT", UNIT.to_string())]);
    assert_eq!(rc, 1, "{text}");
    contains_all(&text, &["FAILED", UNIT], "the failure");
    assert!(
        c.present(UNIT),
        "{UNIT} was removed after its disable failed"
    );
    assert!(
        !c.log_lines().iter().any(|l| l == "daemon-reload"),
        "a reload ran after the failure"
    );
    never_leaks(&text, "the failure record");
}

/// The second run — the unit already gone — passes the same bounds,
/// finds nothing to retire, and is an OK with an empty plan, not a
/// refusal: a converged host is the goal.
#[test]
fn the_second_run_is_an_ok_with_nothing_to_do() {
    let c = Case::new("second-run");
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{text}");
    std::fs::remove_file(&c.log).unwrap();
    let (rc, text) = c.run(&["--for-real"]);
    assert_eq!(
        rc, 0,
        "a second run on a converged host did not exit 0:\n{text}"
    );
    assert!(c.disabled().is_empty(), "a second run acted:\n{text}");
    contains_all(
        &text,
        &[&format!("not on this host {UNIT}"), "OK"],
        "the second run's record",
    );
    let (rc, text) = c.run(&["--dry-run"]);
    assert_eq!(rc, 0, "{text}");
    contains_all(
        &text,
        &[&format!("not on this host {UNIT}"), "DRY RUN"],
        "the dry run after",
    );
}

// ---------------------------------------------------------------------------
// THROUGH THE RUNNER, with the real allowlist, as boss-gcp.
// ---------------------------------------------------------------------------

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
/// stub `curl` answers the runner's jobs read with the packet, records
/// the PUT, and answers the converge read the script makes.
fn run_runner(c: &Case, verbs: &Path, args: &str) -> (String, Option<serde_json::Value>) {
    write_exec(
        &c.bin.join("curl"),
        "#!/bin/sh\n\
         for a in \"$@\"; do case \"$a\" in @*) cp \"${a#@}\" \"$STUB_PUT\"; printf 200; exit 0;; esac; done\n\
         for a in \"$@\"; do case \"$a\" in *kind=maintenance-cluster-converge*) cat \"$STUB_CONVERGES\"; exit 0;; esac; done\n\
         cat \"$STUB_JOBS\"\n",
    );
    write_file(
        &c.root.join("jobs.json"),
        &format!(
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"boss-gcp","verb":"retire-cloudflared","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}]}}"#
        ),
    );
    let put = c.root.join("put.json");
    let _ = std::fs::remove_file(&put);
    let mut cmd = Command::new("sh");
    cmd.arg(repo_root().join("infra/ops/ops-runner.sh"));
    c.env(&mut cmd);
    cmd.env("HOST_ID", "boss-gcp")
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

/// `--dry-run` reaches the script through the runner ON boss-gcp, with
/// the script resolved against the runner's own checkout, and the
/// packet's output carries the masked unit and never the token.
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
    contains_all(
        output,
        &[
            "DRY RUN",
            "--token <masked by unit-cat>",
            &format!("would stop+disable+remove {UNIT}"),
        ],
        "the packet's output",
    );
    never_leaks(output, "the packet's output");
    assert!(c.log_lines().is_empty(), "a dry run acted:\n{text}");
    assert_eq!(c.etc_listing(), before, "a dry run changed the unit files");
}

/// The verb is registered the way the other boss-gcp mutating verbs
/// are: admitted by name in the hosts lint with its authorization, and
/// says in its own `about` that the tunnel in Cloudflare is not its to
/// touch.
#[test]
fn the_verb_is_admitted_and_bounded_in_prose() {
    let json = std::fs::read_to_string(repo_root().join("infra/ops/verbs/retire-cloudflared.json"))
        .expect("infra/ops/verbs/retire-cloudflared.json");
    let spec: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(spec["hosts"], serde_json::json!(["boss-gcp"]));
    assert_eq!(spec["argv"][0], SCRIPT);
    let about = spec["about"].as_str().unwrap();
    for phrase in [
        "MUTATING",
        "David",
        "Cloudflare",
        "revoke",
        "--dry-run",
        "--for-real",
    ] {
        assert!(about.contains(phrase), "about does not say {phrase}");
    }
    let lint = std::fs::read_to_string(
        repo_root().join("infra/lint/a-verb-declares-the-hosts-it-serves.sh"),
    )
    .unwrap();
    assert!(
        lint.contains(r#""retire-cloudflared": "David 2026-09-16"#),
        "the hosts lint does not admit retire-cloudflared by name with its authorization"
    );
}
