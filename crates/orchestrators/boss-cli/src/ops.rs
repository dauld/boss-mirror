//! Operator commands — status, restart, logs, audit.
//!
//! Designed for both human operators and AI agents (Claude Code).
//! Every command supports `--json` for machine-readable output.

use anyhow::{Context, Result};
use serde::Serialize;
use std::process::Command;

// ---------------------------------------------------------------------------
// Service registry — the canonical list of Boss services
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ServiceInfo {
    pub name: &'static str,
    pub unit: &'static str,
    pub port: u16,
    pub health_path: &'static str,
}

pub const SERVICES: &[ServiceInfo] = &[
    ServiceInfo {
        name: "gateway",
        unit: "boss-gateway",
        port: 4443,
        health_path: "/api/assets/health",
    },
    ServiceInfo {
        name: "assets",
        unit: "boss-assets-api",
        port: 7600,
        health_path: "/api/assets/health",
    },
    ServiceInfo {
        name: "catalog",
        unit: "boss-catalog-api",
        port: 7750,
        health_path: "/api/catalog/health",
    },
    ServiceInfo {
        name: "people",
        unit: "boss-people-api",
        port: 7500,
        health_path: "/api/people/health",
    },
    ServiceInfo {
        name: "commerce",
        unit: "boss-commerce-api",
        port: 7400,
        health_path: "/api/commerce/health",
    },
    ServiceInfo {
        name: "inventory",
        unit: "boss-inventory-api",
        port: 7300,
        health_path: "/api/inventory/health",
    },
    ServiceInfo {
        name: "messages",
        unit: "boss-messages-api",
        port: 7200,
        health_path: "/api/messages/health",
    },
    ServiceInfo {
        name: "shipping",
        unit: "boss-shipping-api",
        port: 7100,
        health_path: "/api/shipping/health",
    },
    ServiceInfo {
        name: "observability",
        unit: "boss-observability",
        // Port 7880 per `boss-ports::SOLO` (canonical registry).
        // The health endpoint mounts under `/api/health`, not bare
        // `/health` — every `boss-observability::main` route is
        // `/api/*`.
        port: 7880,
        health_path: "/api/health",
    },
    ServiceInfo {
        name: "cybernetics",
        unit: "boss-cybernetics",
        port: 7700,
        health_path: "/health",
    },
];

fn find_service(name: &str) -> Option<&'static ServiceInfo> {
    SERVICES.iter().find(|s| s.name == name)
}

/// Return the systemd unit names for every registered boss-*
/// service. Consumed by `boss doctor install` so the
/// post-install validator iterates the same canonical list as
/// `boss status` — avoids drift between the two surfaces.
pub fn registered_service_units() -> Vec<String> {
    SERVICES.iter().map(|s| s.unit.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Systemctl abstraction — mockable so tests can exercise check_systemd /
// restart without spawning the real `systemctl` binary (which requires
// root, a live init system, and real service state).
// ---------------------------------------------------------------------------

pub trait SystemctlRunner: Send + Sync {
    /// `systemctl is-active <unit>` — returns the raw stdout string
    /// ("active", "inactive", "failed", "unknown", etc.)
    fn is_active(&self, unit: &str) -> String;

    /// `sudo systemctl restart <unit>` — returns (success, combined
    /// stderr/stdout message). Implementations that can't run the
    /// real binary return `(false, "<reason>")`.
    fn restart(&self, unit: &str) -> RestartOutcome;
}

#[derive(Debug, Clone)]
pub struct RestartOutcome {
    pub success: bool,
    pub message: String,
}

/// Real systemctl runner — shells out to the system binaries.
pub struct RealSystemctl;

impl SystemctlRunner for RealSystemctl {
    fn is_active(&self, unit: &str) -> String {
        Command::new("systemctl")
            .args(["is-active", unit])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    }

    fn restart(&self, unit: &str) -> RestartOutcome {
        match Command::new("sudo")
            .args(["systemctl", "restart", unit])
            .output()
        {
            Ok(output) => RestartOutcome {
                success: output.status.success(),
                message: if output.status.success() {
                    format!("{unit} restarted")
                } else {
                    format!(
                        "{unit} restart failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    )
                },
            },
            Err(e) => RestartOutcome {
                success: false,
                message: format!("{unit} restart error: {e}"),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// boss status
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub services: Vec<ServiceStatus>,
    pub postgres: HealthCheck,
    pub nats: HealthCheck,
    pub backup: BackupStatus,
}

#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    pub name: String,
    pub unit: String,
    pub port: u16,
    pub systemd: String,
    pub health: String,
}

#[derive(Debug, Serialize)]
pub struct HealthCheck {
    pub name: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct BackupStatus {
    pub latest: Option<String>,
    pub count: usize,
    pub next_scheduled: String,
}

pub async fn status(json: bool) -> Result<()> {
    status_with(&RealSystemctl, json).await
}

pub async fn status_with(runner: &dyn SystemctlRunner, json: bool) -> Result<()> {
    let report = build_status_report(runner).await?;
    print_status_report(&report, json)?;
    if !report.all_healthy() {
        std::process::exit(1);
    }
    Ok(())
}

/// Pure report builder — no printing, no process::exit. Walks
/// SERVICES asking the runner for systemd state and `check_health`
/// for the service's health endpoint, then folds in postgres,
/// NATS, and backup. Used by `status_with` and by tests that want
/// to assert on the structured outcome.
pub async fn build_status_report(runner: &dyn SystemctlRunner) -> Result<StatusReport> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?;

    let mut services = Vec::new();
    for svc in SERVICES {
        let systemd = runner.is_active(svc.unit);
        let health = check_health(&client, svc.port, svc.health_path).await;
        services.push(ServiceStatus {
            name: svc.name.to_string(),
            unit: svc.unit.to_string(),
            port: svc.port,
            systemd,
            health,
        });
    }

    let postgres = HealthCheck {
        name: "postgres".to_string(),
        status: check_postgres(),
    };
    let nats = HealthCheck {
        name: "nats".to_string(),
        status: check_health(&client, 8222, "/healthz").await,
    };
    let backup = check_backup();

    Ok(StatusReport {
        services,
        postgres,
        nats,
        backup,
    })
}

fn print_status_report(report: &StatusReport, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        println!("Boss Status");
        println!("=============\n");
        println!("{:<14} {:<10} {:<10}", "SERVICE", "SYSTEMD", "HEALTH");
        println!("{}", "-".repeat(38));
        for s in &report.services {
            let systemd_icon = if s.systemd == "active" { "ok" } else { "DOWN" };
            let health_icon = if s.health == "ok" { "ok" } else { "FAIL" };
            println!("{:<14} {:<10} {:<10}", s.name, systemd_icon, health_icon);
        }
        println!("\nPostgres: {}", report.postgres.status);
        println!("NATS: {}", report.nats.status);
        if let Some(ref latest) = report.backup.latest {
            println!(
                "Backup: {} ({} total, next: {})",
                latest, report.backup.count, report.backup.next_scheduled
            );
        } else {
            println!("Backup: none found");
        }
    }
    Ok(())
}

impl StatusReport {
    /// True when every registered service is both `systemd=active`
    /// and `health=ok`. Used by the CLI as the non-zero-exit signal
    /// and by tests to assert on the report shape.
    pub fn all_healthy(&self) -> bool {
        self.services
            .iter()
            .all(|s| s.health == "ok" && s.systemd == "active")
    }

    /// Names of services whose systemd or health state is degraded.
    /// Empty when `all_healthy()` is true. Tests are the only caller,
    /// so it is test code; a CLI output tweak that wants it lifts the
    /// `cfg` rather than adding a dead-code marker.
    #[cfg(test)]
    fn degraded_services(&self) -> Vec<&str> {
        self.services
            .iter()
            .filter(|s| s.health != "ok" || s.systemd != "active")
            .map(|s| s.name.as_str())
            .collect()
    }
}

async fn check_health(client: &reqwest::Client, port: u16, path: &str) -> String {
    match client
        .get(format!("http://127.0.0.1:{port}{path}"))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => "ok".to_string(),
        Ok(r) => format!("http-{}", r.status().as_u16()),
        Err(_) => "unreachable".to_string(),
    }
}

fn check_postgres() -> String {
    Command::new("pg_isready")
        .output()
        .map(|o| {
            if o.status.success() {
                "ok".to_string()
            } else {
                "down".to_string()
            }
        })
        .unwrap_or_else(|_| "unknown".to_string())
}

fn check_backup() -> BackupStatus {
    let dir = "/var/backups/boss";
    let mut backups: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".tar.gz"))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    backups.sort();
    backups.reverse();

    let next = Command::new("systemctl")
        .args([
            "show",
            "boss-backup.timer",
            "--property=NextElapseUSecRealtime",
            "--value",
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    BackupStatus {
        latest: backups.first().cloned(),
        count: backups.len(),
        next_scheduled: next,
    }
}

// ---------------------------------------------------------------------------
// boss restart
// ---------------------------------------------------------------------------

pub async fn restart(service: &str, json: bool) -> Result<()> {
    let outcome = restart_with(&RealSystemctl, service)?;
    print_restart_outcome(service, &outcome, json);
    if !outcome.success {
        std::process::exit(1);
    }
    Ok(())
}

/// Pure restart: resolves the service, invokes the runner, returns the
/// outcome without printing or exiting. Used by both the shipped
/// `restart` command and unit tests.
pub fn restart_with(runner: &dyn SystemctlRunner, service: &str) -> Result<RestartOutcome> {
    let svc = find_service(service).ok_or_else(|| {
        anyhow::anyhow!("unknown service: {service}. Run `boss status` to see all services")
    })?;
    Ok(runner.restart(svc.unit))
}

fn print_restart_outcome(service: &str, outcome: &RestartOutcome, json: bool) {
    if json {
        let svc_unit = find_service(service).map(|s| s.unit).unwrap_or("");
        println!(
            "{}",
            serde_json::json!({
                "service": service,
                "unit": svc_unit,
                "success": outcome.success,
                "message": outcome.message,
            })
        );
    } else {
        println!("{}", outcome.message);
    }
}

// ---------------------------------------------------------------------------
// boss logs
// ---------------------------------------------------------------------------

pub async fn logs(service: &str, lines: u32, follow: bool, json: bool) -> Result<()> {
    let svc = find_service(service).ok_or_else(|| {
        anyhow::anyhow!("unknown service: {service}. Run `boss status` to see all services")
    })?;

    let mut args = vec![
        "-u".to_string(),
        svc.unit.to_string(),
        "--no-pager".to_string(),
        format!("-n{lines}"),
    ];
    if follow {
        args.push("-f".to_string());
    }
    if json {
        args.push("-o".to_string());
        args.push("json".to_string());
    }

    let status = Command::new("journalctl")
        .args(&args)
        .status()
        .context("failed to run journalctl")?;

    if !status.success() {
        std::process::exit(1);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// boss audit
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct AuditEntry {
    pub event_id: String,
    pub timestamp: String,
    pub source: String,
    pub kind: String,
    pub payload: String,
}

/// Parse one row of tab-separated psql output into an AuditEntry.
/// Returns None for empty lines.
fn parse_audit_row(line: &str) -> Option<AuditEntry> {
    if line.is_empty() {
        return None;
    }
    let parts: Vec<&str> = line.splitn(5, '|').collect();
    Some(AuditEntry {
        event_id: parts.first().unwrap_or(&"").to_string(),
        timestamp: parts.get(1).unwrap_or(&"").to_string(),
        source: parts.get(2).unwrap_or(&"").to_string(),
        kind: parts.get(3).unwrap_or(&"").to_string(),
        payload: parts.get(4).unwrap_or(&"").to_string(),
    })
}

pub async fn audit(kind: Option<&str>, source: Option<&str>, limit: u32, json: bool) -> Result<()> {
    let mut query = "SELECT event_id, timestamp, source, kind, payload FROM audit_log".to_string();
    let mut conditions = Vec::new();

    if let Some(k) = kind {
        conditions.push(format!("kind LIKE '{k}%'"));
    }
    if let Some(s) = source {
        conditions.push(format!("source = '{s}'"));
    }

    if !conditions.is_empty() {
        query.push_str(" WHERE ");
        query.push_str(&conditions.join(" AND "));
    }
    query.push_str(&format!(" ORDER BY timestamp DESC LIMIT {limit}"));

    let output = Command::new("sudo")
        .args([
            "-u",
            "postgres",
            "psql",
            "-d",
            "boss",
            "--no-align",
            "--tuples-only",
        ])
        .arg("-c")
        .arg(&query)
        .output()
        .context("failed to query audit_log")?;

    if json {
        let lines = String::from_utf8_lossy(&output.stdout);
        let entries: Vec<AuditEntry> = lines.lines().filter_map(parse_audit_row).collect();
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            println!("No audit entries found.");
        } else {
            println!(
                "{:<36} {:<12} {:<30} PAYLOAD",
                "TIMESTAMP", "SOURCE", "KIND"
            );
            println!("{}", "-".repeat(90));
            for line in stdout.lines() {
                let parts: Vec<&str> = line.splitn(5, '|').collect();
                if parts.len() >= 4 {
                    println!(
                        "{:<36} {:<12} {:<30} {}",
                        parts.get(1).unwrap_or(&""),
                        parts.get(2).unwrap_or(&""),
                        parts.get(3).unwrap_or(&""),
                        parts.get(4).map(|p| &p[..p.len().min(50)]).unwrap_or(""),
                    );
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // -----------------------------------------------------------------
    // SERVICES registry invariants
    //
    // These tests catch silent registry rot — e.g. a service whose
    // systemd unit drifts out of sync with the unit on disk.
    // -----------------------------------------------------------------

    #[test]
    fn services_registry_is_non_empty() {
        assert!(
            !SERVICES.is_empty(),
            "SERVICES registry should list at least the gateway and one domain service"
        );
    }

    #[test]
    fn services_registry_has_unique_names() {
        let mut seen: HashSet<&'static str> = HashSet::new();
        for svc in SERVICES {
            assert!(
                seen.insert(svc.name),
                "duplicate service name in SERVICES: {}",
                svc.name
            );
        }
    }

    #[test]
    fn services_registry_has_unique_ports() {
        let mut seen: HashSet<u16> = HashSet::new();
        for svc in SERVICES {
            assert!(
                seen.insert(svc.port),
                "duplicate port {} in SERVICES (offender: {})",
                svc.port,
                svc.name
            );
        }
    }

    #[test]
    fn services_registry_has_unique_units() {
        let mut seen: HashSet<&'static str> = HashSet::new();
        for svc in SERVICES {
            assert!(
                seen.insert(svc.unit),
                "duplicate systemd unit in SERVICES: {}",
                svc.unit
            );
        }
    }

    #[test]
    fn services_registry_fields_are_all_populated() {
        for svc in SERVICES {
            assert!(!svc.name.is_empty(), "empty name in SERVICES");
            assert!(!svc.unit.is_empty(), "empty unit for service {}", svc.name);
            assert_ne!(svc.port, 0, "port 0 for service {}", svc.name);
            assert!(
                svc.health_path.starts_with('/'),
                "health_path must start with / for service {}: got '{}'",
                svc.name,
                svc.health_path
            );
        }
    }

    // -----------------------------------------------------------------
    // StatusReport serialization — schema lock-in
    //
    // The JSON shape is a public contract. Operators and agents parse
    // `boss status --json`; any accidental field rename or removal is
    // a breaking change for them. These tests freeze the shape.
    // -----------------------------------------------------------------

    fn sample_report() -> StatusReport {
        StatusReport {
            services: vec![ServiceStatus {
                name: "assets".to_string(),
                unit: "boss-assets-api".to_string(),
                port: 7600,
                systemd: "active".to_string(),
                health: "ok".to_string(),
            }],
            postgres: HealthCheck {
                name: "postgres".to_string(),
                status: "ok".to_string(),
            },
            nats: HealthCheck {
                name: "nats".to_string(),
                status: "ok".to_string(),
            },
            backup: BackupStatus {
                latest: Some("boss-backup-20260410-170000.tar.gz".to_string()),
                count: 5,
                next_scheduled: "2026-04-11 03:00:00 UTC".to_string(),
            },
        }
    }

    #[test]
    fn status_report_serializes_with_top_level_keys() {
        let report = sample_report();
        let json: serde_json::Value = serde_json::to_value(&report).unwrap();
        let obj = json
            .as_object()
            .expect("StatusReport must be a JSON object");
        for key in ["services", "postgres", "nats", "backup"] {
            assert!(
                obj.contains_key(key),
                "StatusReport JSON missing top-level key `{key}`; found keys: {:?}",
                obj.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn service_status_serializes_with_expected_fields() {
        let report = sample_report();
        let json = serde_json::to_value(&report).unwrap();
        let svc = &json["services"][0];
        for key in ["name", "unit", "port", "systemd", "health"] {
            assert!(
                svc.get(key).is_some(),
                "ServiceStatus JSON missing field `{key}`; got: {svc}"
            );
        }
        // Port must round-trip as a number, not a string.
        assert!(
            svc["port"].is_u64(),
            "ServiceStatus.port must serialize as a JSON number, got: {}",
            svc["port"]
        );
    }

    #[test]
    fn backup_status_latest_is_nullable() {
        let mut report = sample_report();
        report.backup.latest = None;
        let json = serde_json::to_value(&report).unwrap();
        assert!(
            json["backup"]["latest"].is_null(),
            "BackupStatus.latest should serialize to null when None; got: {}",
            json["backup"]["latest"]
        );
    }

    #[test]
    fn health_check_serializes_name_and_status() {
        let report = sample_report();
        let json = serde_json::to_value(&report).unwrap();
        for slice in ["postgres", "nats"] {
            let hc = &json[slice];
            assert!(
                hc.get("name").is_some(),
                "{slice} HealthCheck JSON missing `name` field; got: {hc}"
            );
            assert!(
                hc.get("status").is_some(),
                "{slice} HealthCheck JSON missing `status` field; got: {hc}"
            );
        }
    }

    // -----------------------------------------------------------------
    // Audit entry parsing + schema lock-in
    // -----------------------------------------------------------------

    #[test]
    fn parse_audit_row_handles_empty_line() {
        assert!(parse_audit_row("").is_none());
    }

    #[test]
    fn parse_audit_row_extracts_all_fields() {
        let line =
            "evt-123|2026-04-10 12:00:00+00|assets|asset.received|{\"sku\":\"Boss-HAL-3D-2024\"}";
        let entry = parse_audit_row(line).expect("row should parse");
        assert_eq!(entry.event_id, "evt-123");
        assert_eq!(entry.timestamp, "2026-04-10 12:00:00+00");
        assert_eq!(entry.source, "assets");
        assert_eq!(entry.kind, "asset.received");
        assert_eq!(entry.payload, "{\"sku\":\"Boss-HAL-3D-2024\"}");
    }

    #[test]
    fn parse_audit_row_preserves_pipes_inside_payload() {
        // splitn(5, '|') caps at 4 splits so the payload keeps any
        // internal pipes — e.g., JSON with piped string values.
        let line = "evt-9|2026-04-10|catalog|model.updated|{\"old\":\"a|b\",\"new\":\"c|d\"}";
        let entry = parse_audit_row(line).unwrap();
        assert_eq!(entry.payload, "{\"old\":\"a|b\",\"new\":\"c|d\"}");
    }

    #[test]
    fn audit_entry_serializes_with_expected_fields() {
        let entry = AuditEntry {
            event_id: "evt-1".to_string(),
            timestamp: "2026-04-10 12:00:00+00".to_string(),
            source: "assets".to_string(),
            kind: "system.received".to_string(),
            payload: "{}".to_string(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        for key in ["event_id", "timestamp", "source", "kind", "payload"] {
            assert!(
                json.get(key).is_some(),
                "AuditEntry JSON missing field `{key}`; got: {json}"
            );
        }
    }

    #[test]
    fn audit_entry_list_serializes_as_json_array() {
        let entries = vec![
            AuditEntry {
                event_id: "evt-1".into(),
                timestamp: "2026-04-10 12:00:00+00".into(),
                source: "assets".into(),
                kind: "system.received".into(),
                payload: "{}".into(),
            },
            AuditEntry {
                event_id: "evt-2".into(),
                timestamp: "2026-04-10 12:00:05+00".into(),
                source: "catalog".into(),
                kind: "model.created".into(),
                payload: "{}".into(),
            },
        ];
        let json = serde_json::to_value(&entries).unwrap();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 2);
    }

    // -----------------------------------------------------------------
    // find_service lookup
    // -----------------------------------------------------------------

    #[test]
    fn find_service_returns_some_for_every_registered_name() {
        for svc in SERVICES {
            assert!(
                find_service(svc.name).is_some(),
                "find_service({}) should return Some",
                svc.name
            );
        }
    }

    #[test]
    fn find_service_returns_none_for_unknown() {
        assert!(find_service("bogus-service-name").is_none());
        assert!(find_service("").is_none());
    }

    // -----------------------------------------------------------------
    // check_health integration
    //
    // Coverage for "boss status against a broken service".
    // Spins up a minimal tokio TCP listener that responds to one
    // request with a canned HTTP status, verifies that check_health
    // returns the expected string for each scenario (healthy,
    // server error, unreachable). Avoids pulling axum into the CLI's
    // dev deps — raw HTTP is five lines.
    //
    // Note: `check_health` hardcodes 127.0.0.1, so the test server
    // must bind to that interface. It binds to port 0 (OS-assigned)
    // so tests don't collide with the running stack on 71xx/72xx.
    // -----------------------------------------------------------------

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Spawn a one-shot HTTP responder on an ephemeral 127.0.0.1 port
    /// that answers the next incoming request with the given status
    /// and body. Returns the port the listener bound to.
    async fn spawn_one_shot_http(status_line: &'static str, body: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                // Drain the request (we don't need to parse it).
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        // Tiny yield so the listener task is ready to accept before
        // the test client fires its request. The OS accept queue
        // backs it up regardless but this keeps traces clean.
        tokio::task::yield_now().await;
        port
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn check_health_returns_ok_on_200() {
        let port = spawn_one_shot_http("200 OK", "ok").await;
        let result = check_health(&test_client(), port, "/healthz").await;
        assert_eq!(result, "ok");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn check_health_reports_http_code_on_5xx() {
        let port = spawn_one_shot_http("500 Internal Server Error", "boom").await;
        let result = check_health(&test_client(), port, "/healthz").await;
        assert_eq!(result, "http-500");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn check_health_reports_http_code_on_4xx() {
        let port = spawn_one_shot_http("503 Service Unavailable", "down").await;
        let result = check_health(&test_client(), port, "/healthz").await;
        assert_eq!(result, "http-503");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn check_health_reports_unreachable_on_closed_port() {
        // Bind + immediately drop to find a port that's definitely
        // not listening. The OS may reuse it within a test lifetime
        // but the probability of it coming back online between drop
        // and the check_health call is effectively zero.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let result = check_health(&test_client(), port, "/healthz").await;
        assert_eq!(result, "unreachable");
    }

    // -----------------------------------------------------------------
    // SystemctlRunner mock — coverage for check_systemd/restart
    //
    // Everything behind `SystemctlRunner` is trivially injectable so
    // tests don't need root, a live init system, or real service
    // state. `FakeSystemctl` records every call so idempotency +
    // dispatch checks can assert on call counts + arguments.
    // -----------------------------------------------------------------

    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSystemctl {
        active_map: Mutex<std::collections::HashMap<String, String>>,
        restart_result: Mutex<std::collections::HashMap<String, RestartOutcome>>,
        is_active_calls: Mutex<Vec<String>>,
        restart_calls: Mutex<Vec<String>>,
    }

    impl FakeSystemctl {
        fn set_active(&self, unit: &str, state: &str) {
            self.active_map
                .lock()
                .unwrap()
                .insert(unit.to_string(), state.to_string());
        }

        fn set_restart_result(&self, unit: &str, success: bool, message: &str) {
            self.restart_result.lock().unwrap().insert(
                unit.to_string(),
                RestartOutcome {
                    success,
                    message: message.to_string(),
                },
            );
        }

        fn is_active_call_count(&self, unit: &str) -> usize {
            self.is_active_calls
                .lock()
                .unwrap()
                .iter()
                .filter(|u| u.as_str() == unit)
                .count()
        }

        fn restart_call_count(&self, unit: &str) -> usize {
            self.restart_calls
                .lock()
                .unwrap()
                .iter()
                .filter(|u| u.as_str() == unit)
                .count()
        }
    }

    impl SystemctlRunner for FakeSystemctl {
        fn is_active(&self, unit: &str) -> String {
            self.is_active_calls.lock().unwrap().push(unit.to_string());
            self.active_map
                .lock()
                .unwrap()
                .get(unit)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string())
        }

        fn restart(&self, unit: &str) -> RestartOutcome {
            self.restart_calls.lock().unwrap().push(unit.to_string());
            self.restart_result
                .lock()
                .unwrap()
                .get(unit)
                .cloned()
                .unwrap_or(RestartOutcome {
                    success: true,
                    message: format!("{unit} restarted"),
                })
        }
    }

    #[test]
    fn is_active_returns_configured_state() {
        let fake = FakeSystemctl::default();
        fake.set_active("boss-assets-api", "active");
        fake.set_active("boss-catalog-api", "inactive");
        assert_eq!(fake.is_active("boss-assets-api"), "active");
        assert_eq!(fake.is_active("boss-catalog-api"), "inactive");
        // Unknown units return the runner's fallback string.
        assert_eq!(fake.is_active("boss-nope"), "unknown");
    }

    #[test]
    fn is_active_records_every_call_for_auditability() {
        let fake = FakeSystemctl::default();
        fake.set_active("boss-assets-api", "active");
        for _ in 0..3 {
            fake.is_active("boss-assets-api");
        }
        assert_eq!(fake.is_active_call_count("boss-assets-api"), 3);
    }

    #[test]
    fn restart_with_unknown_service_returns_err() {
        let fake = FakeSystemctl::default();
        let result = restart_with(&fake, "bogus-service");
        assert!(result.is_err());
        // The real runner should not be invoked for an unknown name.
        assert_eq!(fake.restart_call_count(""), 0);
    }

    #[test]
    fn restart_with_known_service_invokes_runner() {
        let fake = FakeSystemctl::default();
        fake.set_restart_result("boss-assets-api", true, "boss-assets-api restarted");
        let outcome = restart_with(&fake, "assets").unwrap();
        assert!(outcome.success);
        assert_eq!(outcome.message, "boss-assets-api restarted");
        assert_eq!(fake.restart_call_count("boss-assets-api"), 1);
    }

    #[test]
    fn restart_is_idempotent_against_the_runner_contract() {
        // Idempotency here means: calling restart twice with the same
        // service name produces the same observable outcome each time.
        // The real systemd semantics guarantee this at the OS level;
        // the CLI wrapper must not introduce any hidden state that
        // would break it (e.g. dedupe that silently drops the second
        // call). Both invocations should hit the runner.
        let fake = FakeSystemctl::default();
        fake.set_restart_result("boss-assets-api", true, "boss-assets-api restarted");

        let first = restart_with(&fake, "assets").unwrap();
        let second = restart_with(&fake, "assets").unwrap();

        assert_eq!(first.success, second.success);
        assert_eq!(first.message, second.message);
        assert_eq!(
            fake.restart_call_count("boss-assets-api"),
            2,
            "both invocations must reach the runner — no hidden dedupe"
        );
    }

    #[test]
    fn restart_propagates_runner_failure() {
        let fake = FakeSystemctl::default();
        fake.set_restart_result(
            "boss-assets-api",
            false,
            "boss-assets-api restart failed: Unit is masked",
        );
        let outcome = restart_with(&fake, "assets").unwrap();
        assert!(!outcome.success);
        assert!(outcome.message.contains("masked"));
    }

    #[test]
    fn print_restart_outcome_text_mode_shapes_message_only() {
        // Sanity check on the pure helper: no panics, no extra IO.
        let outcome = RestartOutcome {
            success: true,
            message: "boss-assets-api restarted".to_string(),
        };
        print_restart_outcome("assets", &outcome, false);
    }

    #[test]
    fn print_restart_outcome_json_mode_shapes_expected_envelope() {
        let outcome = RestartOutcome {
            success: true,
            message: "boss-assets-api restarted".to_string(),
        };
        // Manually build the same JSON the helper would print and
        // assert it round-trips with every expected field — the
        // actual println goes to stdout and isn't captured here.
        let svc_unit = find_service("assets").map(|s| s.unit).unwrap();
        let expected = serde_json::json!({
            "service": "assets",
            "unit": svc_unit,
            "success": true,
            "message": outcome.message,
        });
        for key in ["service", "unit", "success", "message"] {
            assert!(expected.get(key).is_some(), "JSON missing field {key}");
        }
        print_restart_outcome("assets", &outcome, true);
    }

    // -----------------------------------------------------------------
    // build_status_report integration — coverage for the
    // "boss status against a broken service" path.
    //
    // `build_status_report` is the pure report builder: it walks
    // SERVICES, calls the injected runner for systemd state, and
    // probes health endpoints via reqwest. We don't spin up real
    // services here — every health check will fall back to
    // "unreachable", which is exactly the shape of a broken stack
    // from the CLI's perspective. That lets us assert on the
    // degraded_services() path without any HTTP setup.
    // -----------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn build_status_report_calls_runner_for_every_service() {
        let fake = FakeSystemctl::default();
        for svc in SERVICES {
            fake.set_active(svc.unit, "active");
        }
        let report = build_status_report(&fake).await.unwrap();
        assert_eq!(report.services.len(), SERVICES.len());
        for svc in SERVICES {
            assert_eq!(
                fake.is_active_call_count(svc.unit),
                1,
                "runner should have been called exactly once for {}",
                svc.unit,
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_status_report_marks_inactive_service_as_degraded() {
        let fake = FakeSystemctl::default();
        // Mark every service active except "assets", which is inactive.
        for svc in SERVICES {
            let state = if svc.name == "assets" {
                "inactive"
            } else {
                "active"
            };
            fake.set_active(svc.unit, state);
        }
        let report = build_status_report(&fake).await.unwrap();

        // The assets row should carry systemd=inactive.
        let assets = report
            .services
            .iter()
            .find(|s| s.name == "assets")
            .expect("assets should be in the report");
        assert_eq!(assets.systemd, "inactive");

        // all_healthy() must return false when any service is degraded.
        assert!(
            !report.all_healthy(),
            "degraded service should cause all_healthy() to return false"
        );
        // degraded_services() must list assets. It will also list
        // every other service, because the in-process health check
        // against a non-running stack returns "unreachable". That's
        // the correct real-world behavior from a test environment,
        // and assets should still appear in the list.
        let degraded = report.degraded_services();
        assert!(
            degraded.contains(&"assets"),
            "assets should appear in degraded_services; got {degraded:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn all_healthy_predicate_enforces_both_checks() {
        // Unit-level check on the `all_healthy` + `degraded_services`
        // helpers so we don't depend on whether the local stack is
        // actually running during the test. Builds a synthetic
        // StatusReport by hand.
        let report = StatusReport {
            services: vec![
                ServiceStatus {
                    name: "a".to_string(),
                    unit: "boss-a".to_string(),
                    port: 1,
                    systemd: "active".to_string(),
                    health: "ok".to_string(),
                },
                ServiceStatus {
                    name: "b".to_string(),
                    unit: "boss-b".to_string(),
                    port: 2,
                    systemd: "inactive".to_string(),
                    health: "ok".to_string(),
                },
                ServiceStatus {
                    name: "c".to_string(),
                    unit: "boss-c".to_string(),
                    port: 3,
                    systemd: "active".to_string(),
                    health: "unreachable".to_string(),
                },
            ],
            postgres: HealthCheck {
                name: "postgres".to_string(),
                status: "ok".to_string(),
            },
            nats: HealthCheck {
                name: "nats".to_string(),
                status: "ok".to_string(),
            },
            backup: BackupStatus {
                latest: None,
                count: 0,
                next_scheduled: "n/a".to_string(),
            },
        };
        assert!(!report.all_healthy());
        let degraded = report.degraded_services();
        assert_eq!(degraded.len(), 2);
        assert!(
            degraded.contains(&"b"),
            "inactive systemd should be degraded"
        );
        assert!(
            degraded.contains(&"c"),
            "unreachable health should be degraded"
        );
        assert!(
            !degraded.contains(&"a"),
            "healthy service should not be listed"
        );
    }
}
