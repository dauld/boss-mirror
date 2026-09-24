//! `infra/estate/observe-door.sh` is RUN, not read — against a real
//! listening socket on this host, a refused port, and stubbed
//! `ssh-keyscan`, `getent` and `curl`, so every verdict below is one
//! the observer actually reached.
//!
//! WHY THE OBSERVER EXISTS (backlog e6406701; incident 55d001b0). Both
//! of the dev pod's ssh doors — the LAN VIP and dev.algedonic.dev — were
//! dark for ~36 hours from 2026-09-22T23:23Z and a person found it. The
//! pod judged its own door once, as sshd started, and nothing outside
//! it ever looked again. The observer runs on the forge (a LAN host
//! outside the cluster, under the cluster watchdog's unit), probes each
//! half of each door `infra/estate/doors.toml` declares, keeps when a
//! half first went dark, and posts one `door` observation that
//! `estate.compare` judges against the declared band.
//!
//! What each case pins: the two values doors.toml shares with other
//! files are equal to them (the MetalLB address of Service boss-dev-ssh,
//! and the name the tunnel routes to it); the watchdog's unit runs the
//! observer after the watchdog and best-effort; the parser reads the
//! real doors.toml; an open door is open, a refused port and a port
//! that offers no host key are dark, a name that does not resolve is a
//! dark public half; the first dark reading is remembered and a later
//! one carries it forward, and an open reading forgets it; an empty
//! declaration is refused; and the observation is POSTed to the estate
//! door, or spooled when the door will not take it.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::Value;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCRIPT: &str = "infra/estate/observe-door.sh";

/// `key = "value"` off one line of a TOML table, the way the observer
/// reads it.
fn toml_value<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    block.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix(key)?.trim_start().strip_prefix('=')?;
        Some(rest.trim().trim_matches('"'))
    })
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// The one `[[door]]` table doors.toml declares today.
fn declared_door() -> String {
    let toml = read("infra/estate/doors.toml");
    let blocks: Vec<&str> = toml.split("[[door]]").skip(1).collect();
    assert_eq!(
        blocks.len(),
        1,
        "doors.toml declares {} doors",
        blocks.len()
    );
    blocks[0].to_string()
}

#[test]
fn the_lan_half_is_the_metallb_address_of_the_ssh_service() {
    let manifest = read("infra/cluster/manifests/boss-dev.yaml");
    let service = manifest
        .split("\n---")
        .find(|doc| doc.contains("kind: Service") && doc.contains("name: boss-dev-ssh"))
        .expect("boss-dev.yaml declares Service boss-dev-ssh");
    let ip = service
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("loadBalancerIP:")
                .map(|v| v.trim().trim_matches('"'))
        })
        .expect("Service boss-dev-ssh pins a loadBalancerIP");
    let port = service
        .lines()
        .find_map(|l| l.trim().strip_prefix("port:").map(str::trim))
        .expect("Service boss-dev-ssh names a port");
    let door = declared_door();
    assert_eq!(
        toml_value(&door, "lan"),
        Some(format!("{ip}:{port}").as_str()),
        "doors.toml's lan half must be Service boss-dev-ssh's MetalLB address \
         (infra/cluster/manifests/boss-dev.yaml) — a door watched at the wrong \
         address is watched as dark forever, or as open while the real one is dark"
    );
}

#[test]
fn the_public_half_is_the_name_the_tunnel_routes_to_that_service() {
    let origins = read("infra/cluster/tunnel-origins.toml");
    let origin = origins
        .split("[[origin]]")
        .find(|b| b.contains("ssh://boss-dev-ssh."))
        .expect("tunnel-origins.toml routes a hostname to Service boss-dev-ssh");
    let hostname = toml_value(origin, "hostname").expect("the origin names its hostname");
    assert_eq!(
        toml_value(&declared_door(), "public"),
        Some(hostname),
        "doors.toml's public half must be the name infra/cluster/tunnel-origins.toml \
         routes to the ssh Service"
    );
}

#[test]
fn the_watchdog_unit_runs_the_observer_after_the_watchdog_and_best_effort() {
    let unit = read("infra/forge/cluster-watchdog.service");
    let watchdog = unit
        .find("\nExecStart=/home/david/boss/infra/forge/cluster-watchdog.sh")
        .expect("the unit runs the watchdog");
    let door = unit
        .find("\nExecStart=-/home/david/boss/infra/estate/observe-door.sh")
        .expect(
            "the unit runs the door observer, prefixed `-` so a failed reading never \
             fails the watchdog's unit",
        );
    assert!(
        door > watchdog,
        "the door observer runs AFTER the watchdog: the loop that acts must never \
         wait on a probe of something else"
    );
    assert!(
        unit.contains("EnvironmentFile=/etc/boss/sor.env"),
        "the observer posts to JOBS_API from sor.env"
    );
}

/// A scratch world: a doors file, a state dir, a spool, and a bin dir of
/// stubs put first on PATH.
struct World {
    dir: PathBuf,
    doors: PathBuf,
    state: PathBuf,
    spool: PathBuf,
    bin: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let dir = scratch_dir(tag);
        let bin = dir.join("bin");
        boss_testing::create_dir(&bin);
        // ssh-keyscan: offers a host key only when told to, so a port
        // that accepts and says nothing can be staged.
        write_exec(
            &bin.join("ssh-keyscan"),
            "#!/bin/sh\n\
             if [ -n \"${STUB_KEYSCAN_KEY:-}\" ]; then\n\
             \x20 echo \"# 127.0.0.1:22 SSH-2.0-dropbear\"\n\
             \x20 echo \"127.0.0.1 $STUB_KEYSCAN_KEY AAAAC3NzaC1lZDI1NTE5AAAAIstub\"\n\
             fi\n\
             exit 0\n",
        );
        // getent: resolves every name but *.invalid, as a resolver would.
        write_exec(
            &bin.join("getent"),
            "#!/bin/sh\n\
             [ \"$1\" = hosts ] || exit 2\n\
             case \"$2\" in *.invalid) exit 2;; esac\n\
             echo \"203.0.113.7     $2\"\n",
        );
        World {
            doors: dir.join("doors.toml"),
            state: dir.join("state"),
            spool: dir.join("spool"),
            bin,
            dir,
        }
    }

    fn declare(&self, lan: &str, public: &str) {
        write_file(
            &self.doors,
            &format!(
                "# a scratch declaration\n\n[[door]]\nid = \"dev-ssh\"\nlan = \"{lan}\"\n\
                 public = \"{public}\"\ndark_band_minutes = \"15\"\n"
            ),
        );
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .args(args)
            .env("PATH", path)
            .env("DOORS_FILE", &self.doors)
            .env("DOOR_STATE_DIR", &self.state)
            .env("DOOR_SPOOL_DIR", &self.spool)
            .env("DOOR_TIMEOUT_S", "2")
            .env_remove("JOBS_API")
            .env_remove("STUB_KEYSCAN_KEY");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().expect("run observe-door.sh")
    }

    fn observe(&self, env: &[(&str, &str)]) -> Value {
        let out = self.run(&["--print"], env);
        assert!(
            out.status.success(),
            "observe-door.sh --print failed: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        let json = text
            .lines()
            .find(|l| l.starts_with('{'))
            .unwrap_or_else(|| panic!("no observation printed: {text}"));
        serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}: {json}"))
    }

    fn state_file(&self, half: &str) -> PathBuf {
        self.state.join(format!("dev-ssh.{half}"))
    }
}

fn half<'a>(obs: &'a Value, name: &str) -> &'a Value {
    obs["nodes"][0]["halves"]
        .as_array()
        .expect("halves")
        .iter()
        .find(|h| h["half"] == name)
        .unwrap_or_else(|| panic!("no {name} half in {obs}"))
}

/// A port nothing listens on: bind one, read its number, let it go.
fn refused_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().expect("addr").port();
    drop(l);
    port
}

fn exists(p: &Path) -> bool {
    p.exists()
}

#[test]
fn the_parser_reads_the_real_declaration() {
    let out = Command::new("bash")
        .arg(repo_root().join(SCRIPT))
        .arg("--declared")
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "dev-ssh 10.20.0.35:22 dev.algedonic.dev band_s=900"
    );
}

#[test]
fn an_open_door_is_observed_open_on_both_halves() {
    let w = World::new("door-open");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let lan = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    w.declare(&lan, "dev.example.test");
    let obs = w.observe(&[("STUB_KEYSCAN_KEY", "ssh-ed25519")]);

    assert_eq!(obs["scope"], "door");
    assert_eq!(obs["observer"], "boss-door-observe");
    assert!(obs["observed_at"].as_str().unwrap().ends_with('Z'), "{obs}");
    let node = &obs["nodes"][0];
    assert_eq!(node["id"], "dev-ssh");
    assert_eq!(
        node["band_s"], 900,
        "the band rides the observation in seconds"
    );
    let l = half(&obs, "lan");
    assert_eq!(l["open"], true, "{l}");
    assert_eq!(l["target"], lan.as_str());
    assert_eq!(l["tcp"], true);
    assert_eq!(l["keyscan"], "ssh-ed25519");
    assert_eq!(l["dark_since"], Value::Null);
    let p = half(&obs, "public");
    assert_eq!(p["open"], true, "{p}");
    assert_eq!(p["address"], "203.0.113.7");
    assert!(!exists(&w.state_file("lan")) && !exists(&w.state_file("public")));
    drop(listener);
}

#[test]
fn a_refused_port_is_dark_and_the_first_dark_reading_is_remembered() {
    let w = World::new("door-refused");
    w.declare(&format!("127.0.0.1:{}", refused_port()), "dev.example.test");
    let obs = w.observe(&[("STUB_KEYSCAN_KEY", "ssh-ed25519")]);
    let l = half(&obs, "lan");
    assert_eq!(l["open"], false, "{l}");
    assert_eq!(l["tcp"], false);
    assert!(
        l["reason"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("refused"),
        "the reason says why: {l}"
    );
    // The first dark reading is when the half went dark, as far as
    // this observer can know it.
    assert_eq!(l["dark_since"], obs["observed_at"], "{l}");
    assert!(exists(&w.state_file("lan")));
    // Only the dark half is remembered.
    assert_eq!(half(&obs, "public")["open"], true);

    // A later reading carries the first one forward: the band is
    // measured from when the door went dark, not from this reading.
    write_file(&w.state_file("lan"), "2026-09-24T00:00:00Z\n");
    let later = w.observe(&[("STUB_KEYSCAN_KEY", "ssh-ed25519")]);
    assert_eq!(half(&later, "lan")["dark_since"], "2026-09-24T00:00:00Z");

    // And the door answering again forgets it.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    w.declare(
        &format!("127.0.0.1:{}", listener.local_addr().unwrap().port()),
        "dev.example.test",
    );
    let open = w.observe(&[("STUB_KEYSCAN_KEY", "ssh-ed25519")]);
    assert_eq!(half(&open, "lan")["open"], true);
    assert!(
        !exists(&w.state_file("lan")),
        "an open half keeps no dark_since"
    );
    drop(listener);
}

#[test]
fn a_port_that_offers_no_host_key_is_not_a_door() {
    let w = World::new("door-mute");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    w.declare(
        &format!("127.0.0.1:{}", listener.local_addr().unwrap().port()),
        "dev.example.test",
    );
    // No STUB_KEYSCAN_KEY: the port accepts and offers no key.
    let obs = w.observe(&[]);
    let l = half(&obs, "lan");
    assert_eq!(l["tcp"], true, "{l}");
    assert_eq!(
        l["open"], false,
        "a port that accepts and says nothing is not a door: {l}"
    );
    assert!(l["reason"].as_str().unwrap().contains("host key"), "{l}");
    drop(listener);
}

#[test]
fn a_name_that_does_not_resolve_is_a_dark_public_half() {
    let w = World::new("door-unnamed");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    w.declare(
        &format!("127.0.0.1:{}", listener.local_addr().unwrap().port()),
        "dev.nowhere.invalid",
    );
    let obs = w.observe(&[("STUB_KEYSCAN_KEY", "ssh-ed25519")]);
    let p = half(&obs, "public");
    assert_eq!(p["open"], false, "{p}");
    assert_eq!(p["target"], "dev.nowhere.invalid");
    assert!(
        p["reason"].as_str().unwrap().contains("does not resolve"),
        "{p}"
    );
    assert_eq!(half(&obs, "lan")["open"], true);
    drop(listener);
}

#[test]
fn a_declaration_of_no_doors_is_refused_by_name() {
    let w = World::new("door-none");
    write_file(&w.doors, "# nothing declared\n");
    let out = w.run(&["--print"], &[]);
    assert!(
        !out.status.success(),
        "an observer with nothing to watch must refuse"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(&w.doors.display().to_string()), "{err}");
    assert!(err.contains("no door"), "{err}");
}

#[test]
fn a_door_missing_a_value_is_refused_by_name() {
    let w = World::new("door-partial");
    write_file(
        &w.doors,
        "[[door]]\nid = \"dev-ssh\"\nlan = \"127.0.0.1:22\"\npublic = \"dev.example.test\"\n",
    );
    let out = w.run(&["--print"], &[]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("dark_band_minutes"), "{err}");
}

/// A `curl` stub standing in for the estate door: records the body it
/// was handed and answers with the status in STUB_STATUS, the shape
/// observe-lib's post_observation reads (body, newline, code).
fn stub_curl(w: &World) -> PathBuf {
    let body = w.dir.join("posted.json");
    write_exec(
        &w.bin.join("curl"),
        &format!(
            "#!/bin/sh\ncat > '{}'\nprintf '%s\\n%s' '{{\"recorded\":true}}' \"${{STUB_STATUS:-202}}\"\n",
            body.display()
        ),
    );
    body
}

#[test]
fn the_observation_is_posted_to_the_estate_door() {
    let w = World::new("door-post");
    let posted = stub_curl(&w);
    w.declare(&format!("127.0.0.1:{}", refused_port()), "dev.example.test");
    let out = w.run(&[], &[("JOBS_API", "http://stub")]);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let body: Value = serde_json::from_str(&std::fs::read_to_string(&posted).unwrap()).unwrap();
    assert_eq!(body["scope"], "door");
    assert_eq!(half(&body, "lan")["open"], false);
    // The journal line names the dark half, so the forge's own journal
    // says it with the system of record dark.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("lan") && stdout.contains("DARK"),
        "{stdout}"
    );
}

#[test]
fn a_reading_the_door_will_not_take_is_kept_for_replay() {
    let w = World::new("door-spool");
    stub_curl(&w);
    w.declare(&format!("127.0.0.1:{}", refused_port()), "dev.example.test");
    let out = w.run(&[], &[("JOBS_API", "http://stub"), ("STUB_STATUS", "503")]);
    assert!(
        !out.status.success(),
        "a reading not recorded is a failed run"
    );
    let kept: Vec<_> = std::fs::read_dir(&w.spool)
        .expect("the spool exists")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .collect();
    assert_eq!(kept.len(), 1, "one reading kept for the next run to replay");
}

#[test]
fn posting_needs_the_system_of_record_named() {
    let w = World::new("door-noapi");
    w.declare(&format!("127.0.0.1:{}", refused_port()), "dev.example.test");
    let out = w.run(&[], &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("JOBS_API"));
}
