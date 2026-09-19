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
//!      `boss prove --unattended` hands the reader as `BOSS_SOR_PORTS` (read
//!      as data from the converged checkout: the forge
//!      has no cargo and no boss-ports binary, so the table is checked
//!      in rather than rendered there);
//!   3. the path-prefix → service table in
//!      `infra/forge/probe-bin/sor-routes.sh` — which services a path
//!      can be routed to at all. Until backlog de0989d2 (2026-09-17)
//!      that table sat inside `boss-sor-read` itself; it moved into a
//!      sourced file so the pod's door (`infra/dev/boss-api`) routes by
//!      the SAME rules without a second copy (CLAUDE.md §9a). Both
//!      scripts source it, and this test reads it from the one place.
//!
//! The DEFINITION of a port is `boss-ports`. This test reads all three
//! copies and holds each equal to the roster: every service the route
//! table names is a boss-ports service, the manifest exposes exactly
//! those services on their prod ports under their boss-ports names, and
//! the env table says the same. One test, three readers; a drift in any
//! copy names the entry.
//!
//! WHY ACCOUNTS IS ON THE DOOR (backlog de0989d2, measured 2026-09-17
//! 14:50Z). The first real sponsorship's reconcile step needs an account
//! created through `POST /api/people/accounts`, which boss-accounts
//! serves on 7550 — and no machine door reached it: the LAN door carried
//! jobs/events/people/classes/locations and the pod's door had no
//! routing at all, so the agent could not do the first real step of the
//! first real loop through any door. Same trust class as design
//! 28d2bed9: the jobs API's writes are already on this IP.
//!
//! WHY THE GATEWAY IS ON THE DOOR (backlog 240e03f3, 2026-09-19). The
//! hardening claim "an undeclared /api read answers 401 without a
//! session" (b4afd7b9) had no probe that could reach a gateway: the
//! door carried service ports only, and prod's hostname answers
//! Cloudflare Access's 302 from the forge. The gateway is the ONE row
//! here that is not path-routed — `boss-sor-read` never sends a path
//! to it, because every read that reader makes is identified and the
//! gateway's answer to an identified reader is not the fact under
//! proof. Its reader is `boss-gateway-read` (boss_gateway_read_sh.rs),
//! which sends no identity and prints the status alone. So the pin's
//! expected set is the route table's services PLUS this named row.

use boss_testing::repo_root;
use std::collections::BTreeMap;

const MANIFEST: &str = "infra/cluster/manifests/boss-jobs-internal.yaml";
const PORTS_ENV: &str = "infra/forge/sor-ports.env";
const ROUTES: &str = "infra/forge/probe-bin/sor-routes.sh";
const ROUTES_BEGIN: &str = "# SOR-ROUTES-BEGIN";
const ROUTES_END: &str = "# SOR-ROUTES-END";
/// The doors that must route by the one table, not by a copy: the two
/// path-routed readers, and the gateway reader, which swaps the port by
/// the same `sor_port_for_service` / `sor_base_on_port` pair.
const DOORS: [&str; 3] = [
    "infra/forge/probe-bin/boss-sor-read",
    "infra/forge/probe-bin/boss-gateway-read",
    "infra/dev/boss-api",
];

/// The rows on the door that NO path routes to: services a reader of
/// its own reaches by name. Exactly one — the gateway, read by
/// `boss-gateway-read` (backlog 240e03f3). A row added here needs a
/// reader that names it; a row that a path routes to belongs in the
/// route table instead.
const NOT_PATH_ROUTED: [&str; 1] = ["gateway"];

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
/// ignored, exactly as `prove::sor_ports_table` reads it.
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

/// Copy 3 — the services a path can be routed to, lifted from between
/// the route file's markers: every `service=<name>` assignment in its
/// routing `case`. Read out of the script, not asserted as a literal,
/// so the pin follows an added route instead of going quiet.
fn reader_services() -> Vec<String> {
    let sh = read(ROUTES);
    let block = sh
        .split_once(ROUTES_BEGIN)
        .unwrap_or_else(|| panic!("{ROUTES} has no {ROUTES_BEGIN} marker"))
        .1
        .split_once(ROUTES_END)
        .unwrap_or_else(|| panic!("{ROUTES} has no {ROUTES_END} marker"))
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
        "{ROUTES} routes to {names:?} only — the routing table between the markers is gone"
    );
    names
}

fn roster() -> BTreeMap<String, u16> {
    boss_ports::all()
        .map(|s| (s.name.to_string(), s.prod))
        .collect()
}

/// THE PIN. The reader's route table names the services, plus the
/// rows a reader reaches by name ([`NOT_PATH_ROUTED`]); the roster
/// gives each its port; the manifest and the env table must both say
/// exactly that — no more (a port no reader can reach is an exposure
/// with no reader), no less (a route to a port the door does not carry
/// is the 404 this car exists to remove).
#[test]
fn the_door_the_table_and_the_reader_agree_with_boss_ports() {
    let roster = roster();
    let mut services = reader_services();
    for s in NOT_PATH_ROUTED {
        assert!(
            !services.iter().any(|r| r == s),
            "{ROUTES} routes a path to `{s}`, which is also listed as not path-routed — one or the other"
        );
        services.push(s.to_string());
    }
    for s in &services {
        assert!(
            roster.contains_key(s),
            "{ROUTES} routes to `{s}`, which boss-ports does not name — a service that is not \
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
/// The gateway row reaches the door the way every other row does —
/// the manifest exposes it on its boss-ports port, the env table hands
/// it to the probe — and its reader names it by exactly that string.
/// A rename of the row in one place is caught here, by name.
#[test]
fn the_gateway_row_is_on_the_door_and_its_reader_names_it() {
    let port = boss_ports::prod("gateway");
    assert_eq!(
        manifest_ports().get("gateway").map(|(p, _)| *p),
        Some(port),
        "{MANIFEST} does not expose the gateway on its boss-ports port — no probe can ask \
         what the gateway answers a stranger"
    );
    assert_eq!(
        env_ports().get("gateway").copied(),
        Some(port),
        "{PORTS_ENV} has no gateway row — boss-gateway-read refuses without one"
    );
    let reader = read("infra/forge/probe-bin/boss-gateway-read");
    assert!(
        reader.contains("sor_port_for_service gateway"),
        "boss-gateway-read must look the row up by the name the table spells"
    );
}

#[test]
fn the_jobs_api_is_still_on_the_door() {
    assert_eq!(
        manifest_ports().get("jobs").map(|(p, _)| *p),
        Some(boss_ports::prod("jobs")),
        "{MANIFEST} no longer exposes the jobs API on its boss-ports port"
    );
    assert!(
        reader_services().iter().any(|s| s == "jobs"),
        "{ROUTES} has no `jobs` route — nothing is left to default to"
    );
}

/// ONE ROUTE TABLE, EVERY DOOR. The forge's two probe readers and the
/// pod's `boss-api` all source the route file from beside themselves,
/// and none carries a `case` of its own: the prefix rules live in one
/// file, so a service added to the door reaches every reader in one
/// edit (backlog de0989d2).
#[test]
fn both_doors_source_the_one_route_file_and_carry_no_table_of_their_own() {
    for door in DOORS {
        let sh = read(door);
        assert!(
            sh.contains("sor-routes.sh"),
            "{door} does not source {ROUTES} — it must route by the shared table, not a copy"
        );
        assert!(
            !sh.contains(ROUTES_BEGIN),
            "{door} still carries its own {ROUTES_BEGIN} block — the table lives in {ROUTES} only"
        );
    }
}

/// THE RULES THEMSELVES, run. `sor_service_for_path` is the one
/// function both doors call; the prefixes are the ones each service's
/// router mounts (boss-accounts src/*.rs `.route(` lines for accounts,
/// boss-people http.rs for the rest of `/api/people`, and so on).
/// Longest prefix wins: `/api/people/accounts` is boss-accounts,
/// `/api/people/emp-x` is boss-people, and a look-alike
/// (`/api/people/accountsx`) is NOT accounts. The query string is not
/// part of the match.
#[test]
fn the_route_function_sends_every_boss_accounts_path_to_accounts_and_nothing_else() {
    let cases = [
        ("/api/people/accounts", "accounts"),
        ("/api/people/accounts?limit=1", "accounts"),
        ("/api/people/accounts/acct-1", "accounts"),
        ("/api/people/accounts/acct-1/notes/n1", "accounts"),
        ("/api/people/accounts/risk-scores", "accounts"),
        ("/api/people/support-cases", "accounts"),
        ("/api/people/support-cases/sc-1", "accounts"),
        ("/api/people/account-account-team/batch", "accounts"),
        ("/api/people/my-day/actions", "accounts"),
        ("/api/people/my-day/actions?employee_id=emp-x", "accounts"),
        ("/api/people", "people"),
        ("/api/people?limit=1", "people"),
        ("/api/people/emp-david", "people"),
        ("/api/people/accountsx", "people"),
        ("/api/people/support-casesx", "people"),
        ("/api/people/my-day", "people"),
        ("/api/people/pto", "people"),
        ("/api/events/tail?limit=1", "events"),
        ("/api/classes?subject_kind=employee", "classes"),
        ("/api/locations/loc-hq", "locations"),
        // The ledger and the dispatcher joined on 2026-09-17 (backlog
        // 77fd7b5a + 4145d2c1): the agent posts a journal entry and reads
        // the rule registry through the door, each on its own port.
        ("/api/ledger/trial-balance", "ledger"),
        ("/api/ledger/journal-entries?limit=1", "ledger"),
        ("/api/dispatcher/rules", "dispatcher"),
        ("/api/dispatcher/rules/auto-assign/versions", "dispatcher"),
        ("/api/ledgers/x", "jobs"),
        ("/api/dispatchers", "jobs"),
        ("/api/jobs?kind=pr-train", "jobs"),
        ("/api/yard/status", "jobs"),
        ("/api/peoples/x", "jobs"),
        ("/api/eventsource", "jobs"),
    ];
    let routes = repo_root().join(ROUTES);
    for (path, want) in cases {
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(". \"$1\" && sor_service_for_path \"$2\"")
            .arg("sor-routes")
            .arg(&routes)
            .arg(path)
            .output()
            .expect("bash runs");
        assert!(
            out.status.success(),
            "sor_service_for_path {path}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(got, want, "sor_service_for_path {path}");
    }
}
