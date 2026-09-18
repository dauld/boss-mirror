# `boss` CLI

The single operator + developer entry point for BOSS. Wraps the
jobs API, the train and gate protocol, the audit log,
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
| `boss audit [--kind prefix] [--source svc] [--json]` | Query audit_log |
| `boss inspect invoices [--status …] [--account-id …] [-n N] [--json]` | List invoices via HTTP |
| `boss inspect accounts [--name …] [-n N] [--json]` | List accounts via HTTP |
| `boss inspect jobs [--status …] [--kind …] [-n N] [--json]` | List jobs via HTTP |
| `boss inspect employees [--role …] [-n N] [--json]` | List employees via HTTP |
| `boss doctor install` | Post-install end-to-end health check |
| `boss triage <item> <disposition> --evidence '…' [--of <packet>]` | Complete a backlog-item's or user-feedback's ready `triage` step; the disposition and the text field are read off the step's declared fields, `duplicate` records `--of` as `duplicate_of` |
| `boss fold <design> --change '…'` | Complete a design-doc's ready `fold` step; refuses while the review is open, naming the anchors still undecided |
| `boss hold <car> --reason '…'` / `boss release <car>` | Put the `hold` marker on a parked car's review step (it stays at the dock), or take it off; a branch or 8+ chars of the id |

The day-to-day operator playbook lives in
[docs/runbooks/operator.md](../../../docs/runbooks/operator.md).
Break-glass procedures and disk-space recovery are documented
there.

## Developer commands

| Command | Description |
|---------|-------------|
| `boss doctor` | Post-install health check (Postgres, NATS, gateway, manifest, SPA, services) |
| `boss upgrade` | Self-update to the latest release |
| `boss emit <kind> [payload]` | Emit an event to NATS |
| `boss script list` | List registered agent scripts |
| `boss script info <id>` | Show script details |
| `boss fleet rebuild-projection` | Rebuild the `systems` projection from `system_events` |
| `boss inspect ...` | Read-only diagnostic HTTP probes through the gateway |
| `boss audit ...` | Query the audit log for domain events |
| `boss ledger ...` | Ledger operations (rebuild the GL projection) |
| `boss sim ...` | Run the simulator (thin wrapper around `boss-sim`) |

The fresh-box setup walkthrough lives in
[docs/runbooks/dev-environment-bootstrap.md](../../../docs/runbooks/dev-environment-bootstrap.md).
