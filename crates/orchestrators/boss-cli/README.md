# `boss` CLI

The single operator + developer entry point for BOSS. Wraps the
service systemd units, the deploy-services script, the audit log,
and the agent runtime so day-to-day work goes through one
interface instead of a tour of `journalctl` / `psql` / `cargo`
incantations.

Install:

```sh
cargo install --path crates/boss-cli --bin boss
```

All commands accept `--json` for machine-readable output (designed
for Claude Code / scripted usage). Run `boss --help` or
`boss <subcommand> --help` for full flag listings.

## Operator commands

| Command | Description |
|---------|-------------|
| `boss status [--json]` | Health check: all services, Postgres, NATS, backups |
| `boss restart <service> [--json]` | Restart a service without rebuilding |
| `boss logs <service> [-n 50] [-f] [--json]` | View service logs (journalctl) |
| `boss backup [--json]` | Trigger manual backup (pg_dump + configs) |
| `boss audit [--kind prefix] [--source svc] [--json]` | Query audit_log |
| `boss inspect invoices [--status …] [--account-id …] [-n N] [--json]` | List invoices via HTTP |
| `boss inspect accounts [--name …] [-n N] [--json]` | List accounts via HTTP |
| `boss inspect jobs [--status …] [--kind …] [-n N] [--json]` | List jobs via HTTP |
| `boss inspect employees [--role …] [-n N] [--json]` | List employees via HTTP |
| `boss doctor install` | Post-install end-to-end health check |

The day-to-day operator playbook lives in
[docs/runbooks/operator.md](../../../docs/runbooks/operator.md).
Break-glass procedures and disk-space recovery are documented
there.

## Developer commands

| Command | Description |
|---------|-------------|
| `boss doctor` | Post-install health check (Postgres, NATS, gateway, manifest, SPA, services) |
| `boss deploy list` | List all deployable services |
| `boss deploy run [service]` | Build + deploy a service (or all) |
| `boss deploy web` | Build + deploy the frontend |
| `boss deploy clean` | Remove debug build artifacts |
| `boss upgrade` | Self-update to the latest release |
| `boss emit <kind> [payload]` | Emit an event to NATS |
| `boss script list` | List registered agent scripts |
| `boss script info <id>` | Show script details |
| `boss status` | Per-service systemd status |
| `boss restart <svc>` | Restart a service unit |
| `boss logs <svc>` | Tail a service's journalctl |
| `boss backup` | Trigger a manual backup (pg_dump + configs) |
| `boss fleet rebuild-projection` | Rebuild the `systems` projection from `system_events` |
| `boss inspect ...` | Read-only diagnostic HTTP probes through the gateway |
| `boss audit ...` | Query the audit log for domain events |
| `boss ledger ...` | Ledger operations (rebuild the GL projection) |
| `boss sim ...` | Run the simulator (thin wrapper around `boss-sim`) |

The fresh-box setup walkthrough lives in
[docs/runbooks/dev-environment-bootstrap.md](../../../docs/runbooks/dev-environment-bootstrap.md).
