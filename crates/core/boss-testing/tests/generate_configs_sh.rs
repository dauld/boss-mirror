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
use std::collections::BTreeSet;
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

/// The PTO endpoint (backlog 6777fecf, found by the equality pin below):
/// boss-people-api answers 503 on POST /api/people/pto unless it is told
/// where the calendar listens, and this generator never told it.
#[test]
fn the_people_api_reaches_the_calendar_on_the_port_boss_ports_gives_it() {
    let path = generate("people-calendar", &[]).join("boss-people-api.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let cfg: toml::Value = toml::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not TOML: {e}\n{text}", path.display()));
    // 7020 is the stub table's calendar row and no other service's.
    assert_eq!(
        cfg.get("calendar_api_url").and_then(|v| v.as_str()),
        Some("http://127.0.0.1:7020"),
        "boss-people-api must be told where boss-calendar-api listens, or PTO answers 503: \
         {cfg:?}"
    );
}

// ---------------------------------------------------------------------
// Which URLs each service is told (backlogs 839ba062 then 6777fecf,
// CLAUDE.md §9a). The set of `*_api_url` fields a service's config
// struct declares and the set this generator writes into its toml are
// one fact in two files, and for the jobs API they drifted both ways in
// one day: calendar_api_url (aa6b4b5c) and subject_kinds_api_url
// (b224ab3c) were declared and never written, so what each switches on
// was off on every instance with one log line to say so; and four more
// (people, assets, locations, inventory) were declared and read by
// NOTHING, under a comment claiming a checker needed them — which is how
// b224ab3c was asked to write them. 839ba062 pinned the jobs API alone;
// the same pair exists for every service block below, so one table holds
// every block the generator writes, in both directions. No boot refusal:
// a boot guard that refuses to start takes the system of record down;
// this is caught at the gate instead.

/// What a generated toml is read by.
enum Reader {
    /// The binary whose `--config` defaults to this toml deserializes
    /// it into `name`, a struct declared at top level of `file`. Found
    /// by reading each binary's `load`/`toml::from_str` call, and held
    /// to that binary below: its source must name both the struct and
    /// `/etc/<toml>`.
    Struct {
        bin: &'static str,
        file: &'static str,
        name: &'static str,
    },
    /// No binary reads the file — its configuration comes from the
    /// environment. Held below: the binary names no `/etc/<toml>`, and
    /// the file may carry no `*_api_url` key, because one there would
    /// configure nothing.
    Unread {
        bin: &'static str,
        why: &'static str,
    },
}

/// Every file generate-configs.sh writes, and what reads it. A new block
/// without a row here fails `every_service_is_told_exactly_the_urls_its_config_declares`.
const SERVICES: &[(&str, Reader)] = &[
    (
        "boss-shipping-api.toml",
        Reader::Struct {
            bin: "crates/modules/boss-shipping/src/bin/boss_shipping_api.rs",
            file: "crates/modules/boss-shipping/src/shipping_config.rs",
            name: "ShippingApiConfig",
        },
    ),
    (
        "boss-messages-api.toml",
        Reader::Struct {
            bin: "crates/modules/boss-messages/src/bin/boss_messages_api.rs",
            file: "crates/modules/boss-messages/src/messages_config.rs",
            name: "MessagesApiConfig",
        },
    ),
    (
        "boss-inventory-api.toml",
        Reader::Struct {
            bin: "crates/modules/boss-inventory/src/bin/boss_inventory_api.rs",
            file: "crates/modules/boss-inventory/src/inventory_config.rs",
            name: "InventoryApiConfig",
        },
    ),
    (
        "boss-commerce-api.toml",
        Reader::Struct {
            bin: "crates/modules/boss-commerce/src/bin/boss_commerce_api.rs",
            file: "crates/modules/boss-commerce/src/commerce_config.rs",
            name: "CommerceApiConfig",
        },
    ),
    (
        "boss-people-api.toml",
        Reader::Struct {
            bin: "crates/modules/boss-people/src/bin/boss_people_api.rs",
            file: "crates/modules/boss-people/src/people_config.rs",
            name: "PeopleApiConfig",
        },
    ),
    (
        "boss-accounts-api.toml",
        Reader::Struct {
            bin: "crates/modules/boss-accounts/src/bin/boss_accounts_api.rs",
            file: "crates/modules/boss-accounts/src/accounts_api_config.rs",
            name: "AccountsApiConfig",
        },
    ),
    (
        "boss-assets-api.toml",
        Reader::Struct {
            bin: "crates/modules/boss-assets/src/bin/boss_assets_api.rs",
            file: "crates/modules/boss-assets/src/asset_config.rs",
            name: "AssetsApiConfig",
        },
    ),
    (
        "boss-catalog-api.toml",
        Reader::Struct {
            bin: "crates/modules/boss-catalog/src/bin/boss_catalog_api.rs",
            file: "crates/modules/boss-catalog/src/kb_config.rs",
            name: "KbApiConfig",
        },
    ),
    (
        "boss-calendar-api.toml",
        Reader::Struct {
            bin: "crates/core/boss-calendar/src/bin/boss_calendar_api.rs",
            file: "crates/core/boss-calendar/src/calendar_config.rs",
            name: "CalendarApiConfig",
        },
    ),
    (
        "boss-jobs-api.toml",
        Reader::Struct {
            bin: "crates/core/boss-jobs/src/bin/boss_jobs_api.rs",
            file: "crates/core/boss-jobs/src/jobs_config.rs",
            name: "JobsApiConfig",
        },
    ),
    (
        "boss-ml-api.toml",
        Reader::Struct {
            bin: "crates/orchestrators/boss-ml-api/src/main.rs",
            file: "crates/core/boss-ml/src/config.rs",
            name: "MlApiConfig",
        },
    ),
    (
        "boss-ledger-api.toml",
        Reader::Struct {
            bin: "crates/modules/boss-ledger/src/bin/boss_ledger_api.rs",
            file: "crates/modules/boss-ledger/src/config.rs",
            name: "LedgerApiConfig",
        },
    ),
    (
        "boss-content-api.toml",
        Reader::Struct {
            bin: "crates/core/boss-content/src/bin/boss_content_api.rs",
            file: "crates/core/boss-content/src/config.rs",
            name: "ContentApiConfig",
        },
    ),
    (
        "boss-policy-api.toml",
        Reader::Unread {
            bin: "crates/core/boss-policy/src/bin/boss_policy_api.rs",
            why: "boss-policy-api reads BOSS_POSTGRES_URL and BOSS_POLICY_PORT from the \
                  environment and takes no --config",
        },
    ),
    (
        "boss-classes-api.toml",
        Reader::Struct {
            bin: "crates/core/boss-classes/src/bin/boss_classes_api.rs",
            file: "crates/core/boss-classes/src/classes_config.rs",
            name: "ClassesApiConfig",
        },
    ),
    (
        "boss-locations-api.toml",
        Reader::Struct {
            bin: "crates/core/boss-locations/src/bin/boss_locations_api.rs",
            file: "crates/core/boss-locations/src/locations_config.rs",
            name: "LocationsApiConfig",
        },
    ),
    (
        "boss-subject-kinds-api.toml",
        Reader::Struct {
            bin: "crates/core/boss-subject-kinds/src/bin/boss_subject_kinds_api.rs",
            file: "crates/core/boss-subject-kinds/src/subject_kinds_config.rs",
            name: "SubjectKindsApiConfig",
        },
    ),
    (
        "boss-events-api.toml",
        Reader::Struct {
            bin: "crates/core/boss-events/src/bin/boss_events_api.rs",
            file: "crates/core/boss-events/src/events_api_config.rs",
            name: "EventsApiConfig",
        },
    ),
    (
        "boss-products-api.toml",
        Reader::Struct {
            bin: "crates/modules/boss-products/src/bin/boss_products_api.rs",
            file: "crates/modules/boss-products/src/config.rs",
            name: "ProductsApiConfig",
        },
    ),
    (
        "boss-campaigns-api.toml",
        Reader::Struct {
            bin: "crates/modules/boss-campaigns/src/bin/boss_campaigns_api.rs",
            file: "crates/modules/boss-campaigns/src/bin/boss_campaigns_api.rs",
            name: "Config",
        },
    ),
    (
        "boss-customers-api.toml",
        Reader::Struct {
            bin: "crates/modules/boss-customers/src/bin/boss_customers_api.rs",
            file: "crates/modules/boss-customers/src/bin/boss_customers_api.rs",
            name: "Config",
        },
    ),
    (
        "boss-dispatcher.toml",
        Reader::Unread {
            bin: "crates/orchestrators/boss-dispatcher-handlers/src/bin/boss_dispatcher.rs",
            why: "boss-dispatcher builds DispatcherConfig::default(), which reads BOSS_*_URL \
                  from the environment and falls back to boss_ports::url for each",
        },
    ),
];

/// `(toml, field, reason)`: fields a config struct declares that the
/// generator deliberately does not write. Each must still be declared,
/// must not be written, and must say why.
const URLS_NOT_WRITTEN: &[(&str, &str, &str)] = &[(
    "boss-people-api.toml",
    "subject_kinds_api_url",
    "boss-people-api builds the client into PeopleApiState.subject_kinds and no handler \
     reads it: http.rs says the validator never fires, scaffolding for a Subject::Custom \
     write the people surface does not accept. Writing it would log \"SubjectKind registry \
     validation enabled\" for a check that does not run (backlog 6777fecf).",
)];

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The `*_api_url` fields of struct `name` in `file`, read off the
/// declaration with `syn` — not a grep of its text. Panics when the
/// struct is absent or has no named field, so a moved struct cannot
/// make the equality below vacuous.
fn declared_urls(file: &str, name: &str) -> BTreeSet<String> {
    let parsed = syn::parse_file(&read(file)).unwrap_or_else(|e| panic!("parse {file}: {e}"));
    let fields: Vec<String> = parsed
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Struct(s) if s.ident == name => Some(s),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no struct {name} in {file}"))
        .fields
        .iter()
        .filter_map(|f| f.ident.as_ref().map(ToString::to_string))
        .collect();
    assert!(
        fields.iter().any(|f| f == "http_bind"),
        "{name} in {file}: the parse found no http_bind, so it is not the service config \
         this row claims: {fields:?}"
    );
    fields
        .into_iter()
        .filter(|f| f.ends_with("_api_url"))
        .collect()
}

#[test]
fn every_service_is_told_exactly_the_urls_its_config_declares() {
    let etc = generate("url-equality", &[]);
    // Every file the generator wrote has a row, and every row a file.
    let generated: BTreeSet<String> = std::fs::read_dir(&etc)
        .unwrap_or_else(|e| panic!("read {}: {e}", etc.display()))
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let tabled: BTreeSet<String> = SERVICES.iter().map(|(t, _)| t.to_string()).collect();
    assert_eq!(
        generated, tabled,
        "SERVICES must name every file generate-configs.sh writes, and only those"
    );
    for (toml_name, field, reason) in URLS_NOT_WRITTEN {
        assert!(
            !reason.trim().is_empty(),
            "{toml_name} {field}: an excuse needs a reason"
        );
    }

    let mut failures = Vec::new();
    let mut declared_total = 0;
    for (toml_name, reader) in SERVICES {
        let text = std::fs::read_to_string(etc.join(toml_name))
            .unwrap_or_else(|e| panic!("read {toml_name}: {e}"));
        let cfg: toml::Value =
            toml::from_str(&text).unwrap_or_else(|e| panic!("{toml_name} is not TOML: {e}"));
        let written: BTreeSet<String> = cfg
            .as_table()
            .unwrap_or_else(|| panic!("{toml_name} is not a table"))
            .keys()
            .filter(|k| k.ends_with("_api_url"))
            .cloned()
            .collect();
        let etc_path = format!("\"/etc/{toml_name}\"");
        let declared = match reader {
            Reader::Struct { bin, file, name } => {
                let bin_src = read(bin);
                assert!(
                    bin_src.contains(&etc_path) && bin_src.contains(name),
                    "{bin} must default --config to {etc_path} and load {name} — \
                     the row for {toml_name} names the wrong reader"
                );
                declared_urls(file, name)
            }
            Reader::Unread { bin, why } => {
                assert!(
                    !read(bin).contains(&etc_path),
                    "{bin} names {etc_path} now, so the row saying nothing reads \
                     {toml_name} ({why}) is stale: map it to the struct it loads"
                );
                BTreeSet::new()
            }
        };
        declared_total += declared.len();
        let excused: BTreeSet<String> = URLS_NOT_WRITTEN
            .iter()
            .filter(|(t, _, _)| t == toml_name)
            .map(|(_, f, _)| f.to_string())
            .collect();
        for field in excused.difference(&declared) {
            failures.push(format!(
                "{toml_name}: {field} is excused in URLS_NOT_WRITTEN but its config no \
                 longer declares it — drop the excuse"
            ));
        }
        for field in excused.intersection(&written) {
            failures.push(format!(
                "{toml_name}: {field} is excused in URLS_NOT_WRITTEN but the generator \
                 writes it — drop the excuse"
            ));
        }
        for field in declared.difference(&written) {
            if !excused.contains(field) {
                failures.push(format!(
                    "{toml_name}: its config declares {field} and generate-configs.sh does \
                     not write it, so whatever it switches on is off on every container pod \
                     (aa6b4b5c, b224ab3c). Write it in the block, delete the field, or \
                     excuse it in URLS_NOT_WRITTEN with the reason"
                ));
            }
        }
        for field in written.difference(&declared) {
            failures.push(format!(
                "{toml_name}: generate-configs.sh writes {field}, which its reader does not \
                 declare: the loader ignores unknown keys, so it is a line under /etc that \
                 configures nothing"
            ));
        }
    }
    // Control: a table whose parses all came back empty would pass the
    // checks above vacuously. The jobs API alone declares three.
    assert!(
        declared_total >= 3,
        "the struct parses found {declared_total} *_api_url fields in all"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
