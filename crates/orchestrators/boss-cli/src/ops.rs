//! Operator command — `boss audit`, a read of the audit log through
//! psql. Designed for both human operators and AI agents; supports
//! `--json` for machine-readable output.
//!
//! Until 2026-09-18 this module also carried `boss status`, `boss
//! restart` and `boss logs`: a hand-typed service roster with ports
//! and systemd unit names, `systemctl` and `journalctl` against a
//! bare-metal host, and a backup check reading /var/backups/boss and
//! boss-backup.timer — both deleted with the bare-metal deploy path
//! (train #443). Nothing in the tree called them, the container
//! launcher is the one way BOSS runs, and the cluster's pg-backup
//! CronJob is the backup; they left with the path (backlog ed64f852).

use anyhow::{Context, Result};
use serde::Serialize;
use std::process::Command;

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
}
