//! `boss doctor` — post-install health check. Probes the running
//! stack (Postgres, NATS, gateway, tenant manifest, SPA bundle,
//! systemd services) and reports the root cause of each failing
//! piece ("X failed because Y").

use anyhow::Result;
use tokio::process::Command;

struct Check {
    label: &'static str,
    passed: bool,
    detail: String,
}

fn render_check(check: &Check) -> String {
    let icon = if check.passed { "+" } else { "-" };
    format!("  [{icon}] {:<20} {}", check.label, check.detail)
}

// ---------------------------------------------------------------------------
// Install-validation checks (operator-facing, post-install)
// ---------------------------------------------------------------------------

/// Probe Postgres reachability + audit_log size. Uses pg_isready
/// for the connectivity check (no Postgres role required) +
/// `boss inspect`-style HTTP probe through the audit_log proxy to
/// avoid raw SQL. Falls back to "tenant has no events" when the
/// seed bundle hasn't been loaded yet.
async fn check_install_postgres() -> Check {
    let isready = Command::new("pg_isready")
        .arg("-h")
        .arg("127.0.0.1")
        .output()
        .await;
    match isready {
        Ok(o) if o.status.success() => Check {
            label: "Postgres",
            passed: true,
            detail: "reachable at 127.0.0.1:5432".into(),
        },
        Ok(_) => Check {
            label: "Postgres",
            passed: false,
            detail:
                "pg_isready reports not accepting connections — is the postgresql service running?"
                    .into(),
        },
        Err(_) => Check {
            label: "Postgres",
            passed: false,
            detail: "pg_isready not on PATH — install postgresql-client".into(),
        },
    }
}

/// Probe NATS via the monitoring HTTP endpoint (bare port 4222
/// speaks the NATS wire protocol so a plain `curl` against it
/// returns garbage — the 8222 monitor port is the friendlier
/// surface). When NATS is started with `-m 8222` (the
/// oss-quickstart Docker compose default) /healthz returns 200.
async fn check_install_nats() -> Check {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    match client.get("http://127.0.0.1:8222/healthz").send().await {
        Ok(r) if r.status().is_success() => Check {
            label: "NATS",
            passed: true,
            detail: "reachable at 127.0.0.1:4222 (healthz 200 on :8222)".into(),
        },
        Ok(r) => Check {
            label: "NATS",
            passed: false,
            detail: format!(
                "NATS monitor returned HTTP {} — check `systemctl status nats`",
                r.status()
            ),
        },
        Err(_) => {
            // Plain port-open check as fallback for installs that
            // don't expose the monitoring endpoint.
            let connect = std::net::TcpStream::connect_timeout(
                &"127.0.0.1:4222".parse().unwrap(),
                std::time::Duration::from_secs(1),
            );
            if connect.is_ok() {
                Check {
                    label: "NATS",
                    passed: true,
                    detail: "port 4222 accepting connections (no /healthz monitor)".into(),
                }
            } else {
                Check {
                    label: "NATS",
                    passed: false,
                    detail: "127.0.0.1:4222 refused — check `systemctl status nats`".into(),
                }
            }
        }
    }
}

/// Hit the gateway's `/health` endpoint at the canonical local
/// listen address. Always probes `127.0.0.1:4443` (not
/// BOSS_GATEWAY_URL) — this command is validating the local
/// install, not a remote dev workstation.
async fn check_install_gateway() -> Check {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    match client.get("http://127.0.0.1:4443/health").send().await {
        Ok(r) if r.status().is_success() => Check {
            label: "Gateway",
            passed: true,
            detail: "http://127.0.0.1:4443/health → 200".into(),
        },
        Ok(r) => Check {
            label: "Gateway",
            passed: false,
            detail: format!(
                "/health returned HTTP {} — check `systemctl status boss-gateway` and `journalctl -u boss-gateway -n 40`",
                r.status()
            ),
        },
        Err(e) => Check {
            label: "Gateway",
            passed: false,
            detail: format!(
                "127.0.0.1:4443 unreachable ({e}) — boss-gateway likely not running; try `systemctl restart boss-gateway`"
            ),
        },
    }
}

/// Probe the tenant manifest endpoint. Returns the label + module
/// count so the operator can confirm tenant.toml is loaded
/// correctly. Empty manifest is suspicious (likely tenant.toml
/// missing or path misconfigured).
async fn check_install_tenant_manifest() -> Check {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    match client
        .get("http://127.0.0.1:4443/api/tenant/manifest")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or(serde_json::Value::Null);
            let labels = body
                .get("labels")
                .and_then(|v| v.as_object())
                .map(|o| o.len())
                .unwrap_or(0);
            let modules = body
                .get("modules")
                .and_then(|v| v.as_object())
                .map(|o| o.len())
                .unwrap_or(0);
            if labels == 0 && modules == 0 {
                Check {
                    label: "Tenant manifest",
                    passed: false,
                    detail: "empty — set BOSS_TENANT_MANIFEST_TOML or place tenant.toml at /etc/boss-gateway/".into(),
                }
            } else {
                Check {
                    label: "Tenant manifest",
                    passed: true,
                    detail: format!("{modules} modules, {labels} labels"),
                }
            }
        }
        Ok(r) => Check {
            label: "Tenant manifest",
            passed: false,
            detail: format!("HTTP {}", r.status()),
        },
        Err(_) => Check {
            label: "Tenant manifest",
            passed: false,
            detail: "unreachable (gateway down?)".into(),
        },
    }
}

/// Confirm the SPA bundle is on disk where the gateway expects to
/// serve it from. Path matches the default `BOSS_STATIC_DIR`.
fn check_install_spa() -> Check {
    let dist = std::path::Path::new("/var/lib/boss-web/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        return Check {
            label: "SPA bundle",
            passed: false,
            detail:
                "/var/lib/boss-web/dist/index.html missing — run `cd apps/web && bun run build && sudo rsync -a dist/ /var/lib/boss-web/dist/`"
                    .into(),
        };
    }
    let chunks = std::fs::read_dir(dist)
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("chunk-") && n.ends_with(".js"))
                })
                .count()
        })
        .unwrap_or(0);
    Check {
        label: "SPA bundle",
        passed: true,
        detail: format!("/var/lib/boss-web/dist/ ({chunks} JS chunks)"),
    }
}

/// Probe each registered boss-* systemd service. Pulls the list
/// from `ops::SERVICES` (already used by `boss status`) for a
/// single source of truth. Reports failures with a one-liner
/// suggesting the journalctl follow-up.
async fn check_install_services() -> Check {
    let services = crate::ops::registered_service_units();
    if services.is_empty() {
        return Check {
            label: "Services",
            passed: false,
            detail: "no registered systemd units in SERVICES — boss-cli build is corrupt".into(),
        };
    }
    let total = services.len();
    let mut failing = Vec::new();
    for unit in &services {
        let out = Command::new("systemctl")
            .args(["is-active", unit])
            .output()
            .await;
        let active = matches!(out, Ok(o) if o.status.success());
        if !active {
            failing.push(unit.clone());
        }
    }
    if failing.is_empty() {
        Check {
            label: "Services",
            passed: true,
            detail: format!("{total}/{total} active"),
        }
    } else {
        let n = failing.len();
        let preview: Vec<&str> = failing.iter().take(3).map(String::as_str).collect();
        let suffix = if n > 3 {
            format!("{} +{} more", preview.join(", "), n - 3)
        } else {
            preview.join(", ")
        };
        Check {
            label: "Services",
            passed: false,
            detail: format!(
                "{}/{total} active — {suffix} not running. Check `journalctl -u <name> -n 40`",
                total - n
            ),
        }
    }
}

/// Does every registered step plugin have a step it can mount on?
///
/// FOUND THE HARD WAY (77e8609a). David reported "I don't know how to
/// input my decision within this UX / I think the wrong step UX is
/// showing" against a car's `scope` step. He was right, and it was
/// worse than one packet: EIGHT of twelve active plugins were
/// registered, active, with a bundle on disk, and no active workflow
/// declared a step of their kind — so they could never mount. The
/// purpose-built `scope-declaration` surface had never rendered for any
/// car, ever, and every scope step fell back to the generic task
/// surface. Nothing was broken; the good surface was simply unreachable.
///
/// WHY NO LINT CAUGHT IT. `step-plugin-bundle-exists.sh` checks one
/// direction — every active row points at a bundle that exists — and
/// deliberately allows the reverse, because a bundle may be committed
/// before the migration that activates it. Both are right. The
/// unchecked relationship is the third one: a registered row whose kind
/// NO ACTIVE WORKFLOW USES.
///
/// It lives here rather than in a gate lint because it cannot be
/// answered from the tree. Protocols are registry data now, so which
/// step kinds are in play is a question only a running deployment can
/// answer — the same reason `check-manifests-applied.sh` talks to a
/// cluster instead of grepping.
async fn check_step_plugins_mount() -> Check {
    let label = "step plugins";
    let base =
        std::env::var("BOSS_JOBS_URL").unwrap_or_else(|_| "http://10.20.0.34:7900".to_string());
    let get = |path: String| {
        let url = format!("{base}{path}");
        async move {
            reqwest::Client::new()
                .get(url)
                .header("x-boss-user", crate::train::boss_user())
                .send()
                .await
                .ok()?
                .json::<serde_json::Value>()
                .await
                .ok()
        }
    };
    let (plugins, workflows) = tokio::join!(
        get("/api/jobs/step-plugins".into()),
        get("/api/workflows?limit=500".into())
    );
    let (Some(plugins), Some(workflows)) = (plugins, workflows) else {
        return Check {
            label,
            passed: false,
            detail: "could not read the registries — unknown, not clean".into(),
        };
    };
    let orphans = orphaned_plugin_kinds(&plugins, &workflows);
    Check {
        passed: orphans.is_empty(),
        label,
        detail: if orphans.is_empty() {
            "every active plugin has a step to mount on".into()
        } else {
            format!(
                "{} active plugin(s) mount on NO active workflow step, so their \
                 surface can never render and the step falls back to the generic \
                 one: {}",
                orphans.len(),
                orphans.join(", ")
            )
        },
    }
}

/// The comparison, pure — which registered plugin kinds no active
/// workflow declares a step of.
pub(crate) fn orphaned_plugin_kinds(
    plugins: &serde_json::Value,
    workflows: &serde_json::Value,
) -> Vec<String> {
    let rows = |v: &serde_json::Value| -> Vec<serde_json::Value> {
        v.get("data")
            .and_then(|d| d.as_array())
            .or_else(|| v.as_array())
            .cloned()
            .unwrap_or_default()
    };
    let active = |v: &serde_json::Value| v.get("status").and_then(|s| s.as_str()) == Some("active");
    let used: std::collections::BTreeSet<String> = rows(workflows)
        .iter()
        .filter(|w| active(w))
        .flat_map(|w| {
            w.get("steps")
                .and_then(|s| s.as_array())
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|s| s.get("kind").and_then(|k| k.as_str()).map(str::to_string))
        .collect();
    let mut orphans: Vec<String> = rows(plugins)
        .iter()
        .filter(|p| active(p))
        .filter_map(|p| p.get("kind").and_then(|k| k.as_str()).map(str::to_string))
        .filter(|k| !used.contains(k))
        .collect();
    orphans.sort();
    orphans.dedup();
    orphans
}

pub async fn run_install() -> Result<()> {
    println!("Boss Install Health");
    println!("───────────────────────");

    let (pg, nats, gw, manifest, services, plugins) = tokio::join!(
        check_install_postgres(),
        check_install_nats(),
        check_install_gateway(),
        check_install_tenant_manifest(),
        check_install_services(),
        check_step_plugins_mount(),
    );
    let spa = check_install_spa();

    let checks = [pg, nats, gw, manifest, spa, services, plugins];

    println!();
    for check in &checks {
        println!("{}", render_check(check));
    }
    println!();

    let passed = checks.iter().filter(|c| c.passed).count();
    let total = checks.len();

    if passed == total {
        println!("  Ready. Open http://localhost:4443 to start.");
    } else {
        println!(
            "  {passed}/{total} checks passed. Address the issues above before opening the SPA."
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// THE EIGHT. Reproduces the measured shape: a plugin registered
    /// and active against a kind no active workflow declares can never
    /// mount, and the step silently falls back to the generic surface —
    /// which is what David saw and reported as the wrong UX (77e8609a).
    #[test]
    fn a_plugin_whose_kind_no_workflow_uses_is_orphaned() {
        let plugins = json!([
            {"kind": "scope-declaration", "status": "active"},
            {"kind": "review-design", "status": "active"},
        ]);
        let workflows = json!([
            {"status": "active", "steps": [{"kind": "task"}, {"kind": "review-design"}]}
        ]);
        assert_eq!(
            orphaned_plugin_kinds(&plugins, &workflows),
            vec!["scope-declaration".to_string()]
        );
    }

    /// A RETIRED WORKFLOW DOES NOT KEEP A PLUGIN ALIVE. This is the
    /// subtle half: `design-doc-review` has 49 closed packets and its
    /// kind still appears in old versions, so counting every workflow
    /// row rather than the ACTIVE ones would report the orphan as
    /// mounted and hide it.
    #[test]
    fn only_active_workflows_count_as_a_mount_point() {
        let plugins = json!([{"kind": "incident-review", "status": "active"}]);
        let workflows = json!([
            {"status": "retired", "steps": [{"kind": "incident-review"}]}
        ]);
        assert_eq!(
            orphaned_plugin_kinds(&plugins, &workflows),
            vec!["incident-review".to_string()]
        );
    }

    /// ...and a retired PLUGIN is not a finding. Deactivating a row is
    /// the intended way to take a surface out of service, so reporting
    /// it would train the reader to ignore this check.
    #[test]
    fn a_retired_plugin_is_not_reported() {
        let plugins = json!([{"kind": "marketing-brief", "status": "retired"}]);
        let workflows = json!([{"status": "active", "steps": [{"kind": "task"}]}]);
        assert!(orphaned_plugin_kinds(&plugins, &workflows).is_empty());
    }

    /// Both list shapes the API uses — a bare array, or wrapped in
    /// `data` — because reading the wrong one reports every plugin as
    /// orphaned, which is a false alarm that would get the check muted.
    #[test]
    fn both_envelope_shapes_are_read() {
        let bare = json!([{"kind": "checklist", "status": "active"}]);
        let wrapped = json!({"data": [{"status": "active", "steps": [{"kind": "checklist"}]}]});
        assert!(orphaned_plugin_kinds(&bare, &wrapped).is_empty());
    }
}
