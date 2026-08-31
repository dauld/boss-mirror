# Design: Kanidm IDM — the front door for real people (and agents)

**Status**: decided — all 5 questions resolved 2026-08-16 via the in-app tracker; see Decisions.
**Origin:** David's direction (2026-08-10): Kanidm provides IDM to
the Playground; originally planned for the GCP box, deployed
in-cluster (see Topology and the Q5 amendment). Item `98816e6a`.
**Related**: [dev-cluster.md](./dev-cluster.md) ·
[payload-encryption.md](./payload-encryption.md) (the other half of
"real people arrive")

## Why now, and why Kanidm

The sim-first strategy gates connecting real people on having the
model's kinks worked out. The other gate is mechanical: real people
need a front door, and header-injected claims from `credentials.toml`
are a bootstrap tool, not one. Kanidm fits the house style: Rust,
single binary, passkey-first, a real OIDC provider, and its own
state — no external database.

Topology: Kanidm runs **in-cluster** — StatefulSet `kanidm-0` in
namespace `kanidm`, served at `<idm-vip>`, terminating its own TLS
(ratified under Q5 below; corrected here from the original "GCP box"
plan via correct-the-record `4c8259ea`). The original invariant —
rebuilding the cluster must not lose the company's logins — is now
carried by state replication rather than host placement: an
online-backup sidecar ships Kanidm's state to boss-gcp hourly
(`/var/backups/kanidm`), so a cluster rebuild restores the IdP from
the last shipment. The inversion has a consequence the old text hid:
cluster repair must never depend on in-cluster Kanidm being up, so
break-glass local auth (below) is load-bearing for exactly that
path, and the DR runbook's access-recovery story is the other half.

## The shape

```
person/agent ──(OIDC auth-code, passkey)──► Kanidm (in-cluster, ns kanidm)
                                              │ id_token: sub, email, groups
                                              ▼
                      boss-gateway (OIDC client, session issuer)
                                              │ maps → EXISTING employee Subject (by email)
                                              │ maps groups → BOSS roles (registry)
                                              ▼
                                x-boss-user claims, as today
```

Two invariants make this BOSS-shaped rather than bolted on:

1. **Kanidm authenticates; it never provisions.** A login maps to an
   *existing* employee Subject or it fails closed. People enter the
   company through the People domain (hiring is a Workflow with an
   audit trail), not as a side effect of first login. The IdP must
   not become a second source of truth for who works here.
2. **Group→role mapping is a registry, not gateway code.** Kanidm
   owns membership; BOSS policy stays BOSS's. The join between them
   is data (`idp_group_roles` or kin), so IT manages access in Kanidm
   and the policy engine never learns Kanidm exists.

Local auth (`credentials.toml`) survives as break-glass: an IdP
outage must not lock the operators out of the system that runs the
company. The migration plan is untouched — the move happens on local
auth; OIDC lands after.

## Open questions

All 5 open questions were resolved 2026-08-16 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q2: What is the employee-mapping key, and what happens on a miss? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> Email is the obvious join (Kanidm account email ↔ employee email).
> A login with no matching employee: fail closed with a message, or
> land in a "pending access" surface an admin can act on? Proposed:
> **fail closed + audit event**; a pending-access Job is a nice later
> step (the Job model doing IdP onboarding) but not v1.

failed closed + audit event sounds good


### Q4: Where does Kanidm's own state live in the backup/migration story? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> Kanidm's DB is the second member of the outside-git-and-Postgres
> class (with `credentials.toml`). Its loss means every real person's
> credentials and passkeys vanish. Proposed: its backup rides the
> existing `backup.sh` timer (kanidm has an online backup facility),
> and dev-cluster.md's copy-set section gains the pointer — the GCP
> box is now stateful in one more way the cluster is not.

That works


### Q1: Does the gateway hold the session, or does every request carry the token? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> Today the gateway issues its own session after local auth. Keeping
> that (gateway session, OIDC only at login) is the small change and
> keeps every downstream service untouched. The alternative — services
> validating bearer tokens themselves — buys per-service revocation at
> the cost of every service growing an OIDC dependency. Proposed:
> **gateway session**, revisit only if service-to-service auth needs it.

Agreed


### Q5: DNS and TLS shape? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> Kanidm terminates its own TLS and historically rejects
> TLS-stripping proxies. Proposed: `id.algedonic.dev`, DNS-only
> (grey-cloud) A record to the GCP box, Kanidm's own cert via its ACME
> support or certbot — verify against current Kanidm docs at install
> time. The gateway's OIDC callback stays behind the existing
> Cloudflare front.
>
> **Amended 2026-08-13 — the host moved, and this is the ratification.**
> What shipped on 2026-08-11 is not what the paragraph above proposes.
> Kanidm runs **in-cluster** as a StatefulSet at `<idm-vip>`, with an
> online-backup sidecar shipping to GCP hourly over a forced-command
> SSH key, reached from outside through the WireGuard hub. The
> TLS reasoning is unchanged and still correct — Kanidm terminates its
> own TLS, so nothing strips it — only the box it runs on changed.
>
> Ratified rather than reverted, on David's stated preference at the
> time ("I don't mind where Kanidm is. Wherever is most secure /
> useful"), because in-cluster is the more secure of the two: it keeps
> the identity provider inside the isolated VLAN alongside the only
> things it serves, and off a shared multi-purpose host that also
> carries the train conductor, the backups, and the public front door.
> A compromise of `boss-gcp` no longer reaches the IdP.
>
> Verified before writing this: `<idm-vip>:443` answers, and
> `/var/backups/kanidm` on boss-gcp is receiving the sidecar's shipments.

Sounds good


### Q3: Do agents get Kanidm service accounts? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> The executor model says agents are CPUs in the same machine. Today
> agent identity is a forged claim header on a trusted box. Kanidm
> service accounts (API tokens, real group membership) would make agent
> identity honest and revocable — and make the audit log's actor claims
> independently verifiable against the IdP. Cost: every agent caller
> grows a token flow. Proposed: **yes, but phase 2** — humans first,
> agents while the header path still works, then the header path dies.

Sounds good

## Decision history

Resolved 2026-08-10 through the in-app review flow — the
`design-doc-review` Job (c57d0b37), answered in the review
surface; flushed here by hand (no flush job queued).

**Q1 — “Agreed”** (emp-bootstrap-admin): Gateway session: OIDC at login only; the gateway keeps issuing its own session and every downstream service stays untouched. Revisit only if service-to-service auth demands per-service tokens.

**Q2 — “failed closed + audit event sounds good”** (emp-bootstrap-admin): Email is the join key to an EXISTING employee Subject. A login with no match fails closed and lands an audit event; a pending-access Job is a later nicety, not v1.

**Q3 — “Sounds good”** (emp-bootstrap-admin): Agents get Kanidm service accounts in phase 2 — humans first, agents while the header path still works, then the header path dies.

**Q4 — “That works”** (emp-bootstrap-admin): Kanidm's online backup rides the existing backup.sh timer; /var/lib/kanidm joins the outside-git-and-Postgres backup set; dev-cluster.md carries the pointer.

**Q5 — “Sounds good”** (emp-bootstrap-admin): id.algedonic.dev grey-cloud (DNS-only); Kanidm terminates its own TLS via certbot/ACME; the gateway's OIDC callback stays behind the Cloudflare front.
