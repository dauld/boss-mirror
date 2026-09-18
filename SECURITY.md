# Security Policy

BOSS is maintained by a single person on the side. There are no
SLAs, no formal coordinated-disclosure window, no embargoed-patch
process. What follows is what you can realistically expect.

## Reporting a vulnerability

Please don't open a public GitHub issue for something with security
impact — that exposes every BOSS deployment between the report and
the fix.

Email <security@algedonic.dev>. Include whatever you have:

- The commit (`git rev-parse HEAD`) or release tag you tested.
- Which component is affected — gateway, a specific service, the
  SPA, an example tenant, the audit-log layer.
- How to reproduce it. The brewery playground tenant
  (<https://playground.algedonic.dev>) is a safe surface for
  proofs of concept.
- Whether you think it's being actively exploited.

I'll read it. I'll do my best to confirm, fix, and credit you in
the release notes (or anonymously, if you prefer). Timeline depends
on how busy I am and how complex the fix is — assume "weeks, not
hours" and we'll both be pleasantly surprised when it's faster.

## What I take seriously

These are the things I'd actually drop other work for:

- Authentication / authorization bypass in `boss-gateway`,
  `boss-policy`, or any row-level policy gate — mis-handled
  `x-boss-user` headers, session-cookie forgery, CF Access JWT
  holes, policy rules that grant broader access than intended.
- Audit-log tampering paths. The chain-hash + REVOKE-protected
  schema is meant to make tampering detectable; a path that
  bypasses `boss-audit-integrity-check` is in scope. See
  [`docs/architecture-decisions.md`](docs/architecture-decisions.md)
  §Correctness protocol & the audit log.
- SQL injection or OS command injection in any service or CLI.
- Secret exposure — credentials in commits, session tokens in
  logs, secrets leaking via error responses.
- Cross-service trust violations — a service accepting
  `x-boss-user` from a non-trusted origin, or trusting
  unauthenticated input from another service.
- SPA-side XSS / CSRF / cookie scope mistakes.
- Denial of service with a trivial unauthenticated trigger.

Things I appreciate the report on but won't drop everything for:

- Self-XSS or attacks requiring a malicious browser extension.
- Session-expiry timing issues without a security impact.
- DoS that requires authenticated abuse from inside an operator's
  own deployment (the operator already has the credentials).
- Issues in third-party services BOSS integrates with (Cloudflare,
  Postgres, NATS) — report those upstream.
- Audit-integrity warnings for `id` gaps from rolled-back
  transactions — documented as suspicious-not-proof in the
  integrity-check output.

## Deployment trust model — read this before you expose BOSS

BOSS authenticates at a single trust boundary: **`boss-gateway`**.
The gateway establishes identity (signed session cookie today) and
forwards it to backend services as an `x-boss-user` header, which
the services trust verbatim. Real per-service auth has not landed
yet (see the *Integrated IAM* item in [`TODO.md`](TODO.md)).

The gateway strips every inbound `x-boss-*` header at its edge
before injecting the session-derived identity, so a client cannot
smuggle identity headers *through* the gateway — with or without a
valid session.

The load-bearing consequence that remains: **the gateway must be
the only thing that can reach the backend service ports.** A client
that can talk to a backend port directly can set its own
`x-boss-user` and assume any identity. So:

- **Do not publish backend ports.** The Docker quickstart already
  does this correctly — only the gateway port (`4443`) is published;
  the backends are reachable only on the internal compose network.
- **Inside the container**, backends bind to `127.0.0.1` (the
  gateway is co-located) — this is what
  `infra/oss-quickstart/generate-configs.sh` emits. If you hand-write
  configs or need a multi-host topology, firewall the backend ports;
  multi-host gets a real IAM story before it gets `0.0.0.0`.
- **Front the gateway with a proxy/IDP** (Cloudflare Access,
  Authelia, etc.) for any internet-facing deployment. Its stripping
  of `x-boss-*` is no longer load-bearing (the gateway strips at its
  own edge), but keep it on as defense-in-depth.

This is a known, deliberate limitation of the preliminary release,
not a defect — but mis-deploying around it is the most likely way
to get a BOSS instance wrong. A path that lets an *external* client
forge `x-boss-user` against a correctly-fronted gateway is in scope
and I want to hear about it.

## Supported versions

Only `main` is supported. There are no backports.

## A note on AI-assisted code

A meaningful share of this codebase was drafted with AI
assistance. If you find a class of issue you suspect is "the LLM
slipping the same pattern in multiple places," tell me — pattern
reports help me sweep for siblings, and that kind of finding is
exactly the shape an outside reviewer is well-placed to catch.

Thanks for helping keep BOSS safe.
