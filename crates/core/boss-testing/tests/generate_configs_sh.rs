//! infra/oss-quickstart/generate-configs.sh — the one writer of every
//! `/etc/boss-*.toml` a container pod reads. It is RUN here against a
//! stub `boss-ports-list` (and, where the tenant matters, the tree's own
//! tenant manifests), so each verdict is one it reached.
//!
//! The demo-agents history (backlog b03f38de, then 1c68aebc) ended with
//! the service it configured: boss-observability retired as
//! superseded-by on 2026-09-23 (backlog 467175e7, car B), and with it
//! the `[demo_agents]` block, the brewery's demo roster and the port
//! row the generator read its bind from. What is pinned now is the
//! absence — the generator must not write a config for a binary no pod
//! starts, whichever tenant it runs for, because a file under /etc that
//! nothing reads is a fact an operator will one day believe.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::process::Command;

const GENERATOR: &str = "infra/oss-quickstart/generate-configs.sh";
const BREWERY: &str = "examples/brewery/seeds/tenant.toml";

/// Every name the generator's `p` and `PORT[...]` lookups ask for,
/// with made-up ports: the script refuses an unknown name (`:?`), so a
/// missing entry here fails loudly rather than skipping a file. The
/// calendar and the subject-kinds registry get ports of their own so a
/// URL read off THEIR rows can be told apart from one read off any
/// other service's.
const STUB_PORTS: &str = "#!/usr/bin/env bash
case \"${1:-}\" in
  --paired)
    for n in shipping messages inventory commerce people accounts assets catalog jobs; do
      echo \"$n:7000:8000\"
    done
    echo \"calendar:7020:8020\" ;;
  --solo)
    for n in ml ledger content policy classes locations events products campaigns customers; do
      echo \"$n:7100\"
    done
    echo \"subject-kinds:7130\" ;;
  *) echo \"stub boss-ports-list: unknown flag ${1:-}\" >&2; exit 2 ;;
esac
";

/// Run the generator into a scratch ETC_DIR with exactly the given
/// environment (plus PATH and ETC_DIR) and return that ETC_DIR.
fn generate(case: &str, env: &[(&str, &std::ffi::OsStr)]) -> std::path::PathBuf {
    let root = scratch_dir(&format!("generate-configs-{case}"));
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(&bin.join("boss-ports-list"), STUB_PORTS);
    let etc = root.join("etc");
    std::fs::create_dir_all(&etc).unwrap();
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(GENERATOR))
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("ETC_DIR", &etc);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("bash runs the generator");
    assert!(
        out.status.success(),
        "generate-configs.sh ({case}) refused: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    etc
}

/// No tenant — not the brewery, which shipped the demo roster, and not
/// a pod that names none — gets a config for the retired service. The
/// stub port table carries no `observability` row either, so a
/// generator still asking `PORT[observability]` refuses (`:?`) and
/// this test names the run that did.
#[test]
fn the_retired_observability_service_gets_no_config() {
    let brewery = repo_root().join(BREWERY);
    for (case, manifest) in [
        ("retired-brewery", Some(brewery.as_os_str())),
        ("retired-no-tenant", None),
    ] {
        let env: Vec<(&str, &std::ffi::OsStr)> = manifest
            .map(|m| ("BOSS_TENANT_MANIFEST_TOML", m))
            .into_iter()
            .collect();
        let etc = generate(case, &env);
        let stray = etc.join("boss-observability.toml");
        assert!(
            !stray.exists(),
            "{case}: the generator wrote {} for a service retired by 467175e7",
            stray.display()
        );
        // The run still produced the fleet's configs, so the absence
        // above is the generator's choice, not a run that stopped early.
        assert!(etc.join("boss-dispatcher.toml").is_file(), "{case}");
    }
}

// ---------------------------------------------------------------------
// file_refs (backlog 6280be03, measured 2026-09-23 on the tree at
// 509f165f). The content-addressed blob store was built and never
// switched on: this generator wrote boss-content-api's config with
// postgres_url, http_bind and nats_url only, so the service mounted its
// 503 "unconfigured" fallback on every /api/files path and the nightly
// files-gc no-opped. The store is on now — but ONLY where the
// deployment names a root, because the root must be DURABLE: a default
// path would put every attachment on the container's writable layer,
// and the next container would restore pointers to nothing.

/// Parse boss-content-api.toml from a generator run.
fn content_config(case: &str, files_root: Option<&str>) -> toml::Value {
    let env: Vec<(&str, &std::ffi::OsStr)> = files_root
        .map(|r| ("BOSS_FILES_ROOT", std::ffi::OsStr::new(r)))
        .into_iter()
        .collect();
    let etc = generate(case, &env);
    let path = etc.join("boss-content-api.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{} is not TOML: {e}\n{text}", path.display()))
}

#[test]
fn a_named_files_root_switches_the_file_store_on() {
    let cfg = content_config("files-on", Some("/var/lib/boss/files"));
    assert_eq!(
        cfg.get("files")
            .and_then(|f| f.get("root"))
            .and_then(|v| v.as_str()),
        Some("/var/lib/boss/files"),
        "BOSS_FILES_ROOT must become the [files] root boss-content-api mounts \
         /api/files on: {cfg:?}"
    );
    // With [files] set and no policy_api_url, the service falls back to
    // PermissivePolicyClient — every caller may attach to anything. The
    // policy engine runs in the same container on the port boss-ports
    // assigns it (the stub table says 7100 for every solo service).
    assert_eq!(
        cfg.get("policy_api_url").and_then(|v| v.as_str()),
        Some("http://127.0.0.1:7100"),
        "a switched-on file store must be policy-checked, not permissive: {cfg:?}"
    );
}

#[test]
fn no_files_root_leaves_the_file_store_off() {
    let cfg = content_config("files-off", None);
    assert!(
        cfg.get("files").is_none(),
        "a deployment that names no durable root must get NO [files] block — a default path \
         would store attachments on the container's writable layer: {cfg:?}"
    );
    // The rest of the service is unchanged either way.
    assert!(cfg.get("postgres_url").is_some() && cfg.get("http_bind").is_some());
}

// ---------------------------------------------------------------------
// The step calendar hook (backlog aa6b4b5c, found by design e1dba350,
// measured 2026-09-24 on origin/main). boss-jobs-api builds its calendar
// client ONLY when `calendar_api_url` is set (JobsApiConfig), and this
// generator — the one writer of /etc/boss-jobs-api.toml on every
// container pod, the live instance included — never wrote it, so the
// reservation hook was a no-op everywhere and no step could reserve.
// The service it points at runs in the same container on every tenant:
// the launcher gates boss-calendar-api on no module (tenant-modules.sh
// `service_module` lists none for it), so a tenant's `calendar = false`
// hides the SPA's Release calendar entry and nothing else.

/// Parse boss-jobs-api.toml from a generator run with no extra env.
fn jobs_config(case: &str) -> toml::Value {
    let etc = generate(case, &[]);
    let path = etc.join("boss-jobs-api.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{} is not TOML: {e}\n{text}", path.display()))
}

#[test]
fn the_jobs_api_reaches_the_calendar_on_the_port_boss_ports_gives_it() {
    let cfg = jobs_config("jobs-calendar");
    // 7020 is the stub table's calendar row and no other service's, so
    // this reads the URL off the calendar's own port — one source of
    // ports (boss-ports), never a second spelled copy here.
    assert_eq!(
        cfg.get("calendar_api_url").and_then(|v| v.as_str()),
        Some("http://127.0.0.1:7020"),
        "boss-jobs-api must be told where boss-calendar-api listens, or the step \
         reservation hook stays off on every instance: {cfg:?}"
    );
}

// ---------------------------------------------------------------------
// Subject-kind validation (backlog b224ab3c, found by builder run
// d31013ad on car aa6b4b5c, measured 2026-09-24). boss-jobs-api builds
// its SubjectKinds client ONLY when `subject_kinds_api_url` is set, and
// this generator never wrote it, so the live instance admitted a packet
// on any subject-kind string. boss-subject-kinds-api runs in the same
// container on every tenant (tenant-modules.sh `service_module` gates it
// on no module), ahead of the jobs API in the launcher's roster.
//
// Measured before enabling it on live: all 15,800 packets the live jobs
// API held on 2026-09-24 carry subject kind `custom` (15,795) or
// `workflow` (5), and both are ACTIVE rows of the live registry, so the
// check refuses none of them. What it adds is a 400 on an unregistered
// or retired kind, and a 502 while the registry is unreachable.

#[test]
fn the_jobs_api_validates_subject_kinds_on_the_port_boss_ports_gives_it() {
    let cfg = jobs_config("jobs-subject-kinds");
    // 7130 is the stub table's subject-kinds row and no other service's.
    assert_eq!(
        cfg.get("subject_kinds_api_url").and_then(|v| v.as_str()),
        Some("http://127.0.0.1:7130"),
        "boss-jobs-api must be told where boss-subject-kinds-api listens, or a packet's \
         subject kind is never checked against the registry: {cfg:?}"
    );
}

/// The packet that filed this asked for four more URLs — people,
/// assets, locations, inventory — for "the subject-existence checker".
/// JobsApiConfig still declares them, but boss-jobs-api reads none of
/// them: since subject-model design R1 (2026-07-15) the existence gate
/// is the Postgres adapter in boss-jobs `subject_existence.rs`, one
/// lookup against the `subjects` table for every kind, wired whenever
/// `postgres_url` is set — so it is already on wherever the jobs API
/// runs on Postgres. (It is described, not named: the pg-feature lint
/// reads any Pg-prefixed identifier here as this test reaching
/// Postgres.) Writing the four would put
/// keys under /etc that nothing reads and that name a checker the
/// binary never builds, which is how this packet came to be filed.
#[test]
fn the_jobs_api_gets_no_url_for_the_retired_http_existence_prober() {
    let cfg = jobs_config("jobs-no-existence-urls");
    for key in [
        "people_api_url",
        "assets_api_url",
        "locations_api_url",
        "inventory_api_url",
    ] {
        assert!(
            cfg.get(key).is_none(),
            "{key} in boss-jobs-api.toml configures nothing — the existence gate is the \
             subjects table, not an HTTP prober: {cfg:?}"
        );
    }
}
