//! The LAN machine door carries every read surface, and the probe
//! reader routes to it by path (design `28d2bed9`, David 2026-09-17).
//!
//! WHAT WAS MEASURED. `boss-jobs-internal` (10.20.0.34, MetalLB,
//! LAN-only) exposed ONE port — the jobs API — and `boss-sor-read`, the
//! one way a car's recorded probe reads the system of record, was
//! `curl $BOSS_JOBS_URL$path`. So two landed cars on 2026-09-17 could
//! not be proven: the locations door (1ec8312a, proved by tree shape
//! instead) and the batch-door events (056f7bd8, UNPROVEN — its probe
//! reads `/api/events/tail`, which lives on boss-events-api and
//! answered 404 on the jobs port). Every future car about people,
//! classes, locations or the audit tail had the same blindness: "no
//! evidence is not a pass", and the instrument could not collect the
//! evidence.
//!
//! THE FACT THAT LIVES THREE TIMES (CLAUDE.md §9a). Which services the
//! door carries, on which port, is written in three places that no
//! build step connects:
//!
//!   1. the Service manifest, `infra/cluster/manifests/boss-jobs-internal.yaml`
//!      — what the cluster actually exposes on 10.20.0.34;
//!   2. `infra/forge/sor-ports.env` — the `name=port` table the forge's
//!      run-car-probe.sh hands the reader as `BOSS_SOR_PORTS` (the forge
//!      has no cargo and no boss-ports binary, so the table is checked
//!      in rather than rendered there);
//!   3. the reader's own path-prefix → service table in
//!      `infra/forge/probe-bin/boss-sor-read` — which services a probe
//!      can be routed to at all.
//!
//! The DEFINITION of a port is `boss-ports`. This test reads all three
//! copies and holds each equal to the roster: every service the reader
//! routes to is a boss-ports service, the manifest exposes exactly those
//! services on their prod ports under their boss-ports names, and the
//! env table says the same. One test, three readers; a drift in any
//! copy names the entry.

use boss_testing::repo_root;
use std::collections::BTreeMap;

const MANIFEST: &str = "infra/cluster/manifests/boss-jobs-internal.yaml";
const PORTS_ENV: &str = "infra/forge/sor-ports.env";
const READER: &str = "infra/forge/probe-bin/boss-sor-read";
const ROUTES_BEGIN: &str = "# SOR-ROUTES-BEGIN";
const ROUTES_END: &str = "# SOR-ROUTES-END";

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{} is readable: {e}", p.display()))
}

/// Copy 1 — the Service's `ports:` list, as `name -> (port, targetPort)`.
/// The manifest writes each entry on one flow-style line
/// (`- {port: 7900, targetPort: 7900, name: jobs}`), which is the shape
/// parsed here; a differently-shaped entry is a loud failure, not a
/// silent omission.
fn manifest_ports() -> BTreeMap<String, (u16, u16)> {
    let yaml = read(MANIFEST);
    let mut out = BTreeMap::new();
    for line in yaml.lines().map(str::trim) {
        let Some(body) = line.strip_prefix("- {").and_then(|l| l.strip_suffix('}')) else {
            continue;
        };
        let fields: BTreeMap<&str, &str> = body
            .split(',')
            .filter_map(|kv| kv.split_once(':'))
            .map(|(k, v)| (k.trim(), v.trim()))
            .collect();
        let field = |k: &str| {
            fields
                .get(k)
                .unwrap_or_else(|| panic!("{MANIFEST}: port entry `{line}` has no `{k}`"))
                .to_string()
        };
        let port: u16 = field("port")
            .parse()
            .unwrap_or_else(|e| panic!("{MANIFEST}: `{line}` port is not a u16: {e}"));
        let target: u16 = field("targetPort")
            .parse()
            .unwrap_or_else(|e| panic!("{MANIFEST}: `{line}` targetPort is not a u16: {e}"));
        let prev = out.insert(field("name"), (port, target));
        assert!(prev.is_none(), "{MANIFEST}: port name repeated in `{line}`");
    }
    assert!(
        !out.is_empty(),
        "{MANIFEST}: no `- {{port: …, targetPort: …, name: …}}` entries parsed — the shape \
         this test reads has changed, so teach it the new one rather than letting the pin go quiet"
    );
    out
}

/// Copy 2 — the env table: `name=port` lines, `#` comments and blanks
/// ignored, exactly as run-car-probe.sh reads it.
fn env_ports() -> BTreeMap<String, u16> {
    let body = read(PORTS_ENV);
    let mut out = BTreeMap::new();
    for line in body.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, port) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("{PORTS_ENV}: `{line}` is not `name=port`"));
        let port: u16 = port
            .parse()
            .unwrap_or_else(|e| panic!("{PORTS_ENV}: `{line}` port is not a u16: {e}"));
        let prev = out.insert(name.to_string(), port);
        assert!(prev.is_none(), "{PORTS_ENV}: `{name}` appears twice");
    }
    assert!(!out.is_empty(), "{PORTS_ENV} names no port at all");
    out
}

/// Copy 3 — the services the reader can route a path to, lifted from
/// between the reader's markers: every `service=<name>` assignment in
/// its routing `case`. Read out of the script, not asserted as a
/// literal, so the pin follows an added route instead of going quiet.
fn reader_services() -> Vec<String> {
    let sh = read(READER);
    let block = sh
        .split_once(ROUTES_BEGIN)
        .unwrap_or_else(|| panic!("{READER} has no {ROUTES_BEGIN} marker"))
        .1
        .split_once(ROUTES_END)
        .unwrap_or_else(|| panic!("{READER} has no {ROUTES_END} marker"))
        .0;
    let mut names: Vec<String> = block
        .split_whitespace()
        .filter_map(|w| w.strip_prefix("service="))
        .map(|w| w.trim_end_matches(';').to_string())
        .collect();
    names.sort();
    names.dedup();
    assert!(
        names.len() > 1,
        "{READER} routes to {names:?} only — the routing table between the markers is gone"
    );
    names
}

fn roster() -> BTreeMap<String, u16> {
    boss_ports::all()
        .map(|s| (s.name.to_string(), s.prod))
        .collect()
}

/// THE PIN. The reader's route table names the services; the roster
/// gives each its port; the manifest and the env table must both say
/// exactly that — no more (a port the reader can never reach is an
/// exposure with no reader), no less (a route to a port the door does
/// not carry is the 404 this car exists to remove).
#[test]
fn the_door_the_table_and_the_reader_agree_with_boss_ports() {
    let roster = roster();
    let services = reader_services();
    for s in &services {
        assert!(
            roster.contains_key(s),
            "{READER} routes to `{s}`, which boss-ports does not name — a service that is not \
             on the roster has no port to expose"
        );
    }
    let expected: BTreeMap<String, u16> = services.iter().map(|s| (s.clone(), roster[s])).collect();

    let manifest = manifest_ports();
    for (name, (port, target)) in &manifest {
        assert_eq!(
            port, target,
            "{MANIFEST}: `{name}` maps port {port} to targetPort {target} — the all-in-one pod \
             listens on the boss-ports port itself, so the two must be equal"
        );
    }
    let manifest: BTreeMap<String, u16> = manifest.into_iter().map(|(n, (p, _))| (n, p)).collect();
    assert_eq!(
        manifest, expected,
        "{MANIFEST} has drifted from boss-ports / the reader's route table \
         (left: the manifest; right: the reader's services at their boss-ports prod ports)"
    );

    assert_eq!(
        env_ports(),
        expected,
        "{PORTS_ENV} has drifted from boss-ports / the reader's route table \
         (left: the env table; right: the reader's services at their boss-ports prod ports)"
    );
}

/// The one port that was always there stays: the jobs API on its
/// boss-ports port is what every conductor and chore write through, and
/// the reader's default when no prefix matches.
#[test]
fn the_jobs_api_is_still_on_the_door() {
    assert_eq!(
        manifest_ports().get("jobs").map(|(p, _)| *p),
        Some(boss_ports::prod("jobs")),
        "{MANIFEST} no longer exposes the jobs API on its boss-ports port"
    );
    assert!(
        reader_services().iter().any(|s| s == "jobs"),
        "{READER} has no `jobs` route — nothing is left to default to"
    );
}
