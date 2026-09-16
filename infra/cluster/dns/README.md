# infra/cluster/dns — the zone and what stands in front of it, declared

| file | what it declares | who reads it |
|---|---|---|
| `algedonic.dev.toml` | every record the zone should hold (`tunnel:` targets by reference, `interlock = "access"` on a record the observer may apply) | `check-declared.sh`, the `dns.observe` handler |
| `access.toml` | the Cloudflare Access applications in front of the proxied hostnames, and the OIDC redirects the gateway needs registered at the IdP | the `dns.observe` handler (applications), the lint `a-public-url-names-a-registered-oidc-redirect.sh` (redirects) |
| `check-declared.sh` | the zone comparator — MATCH / DRIFT / ABSENT / UNDECLARED over a `dns_records` body, by hand or from the handler | operators, tests, the handler |

The `dns.observe` dispatcher handler (rule `dns-observe-on-observe-ready`)
runs once a day on a `dns-zone-observation` packet (rule
`dns-zone-observe-daily`) with the credential broker's Cloudflare root
token, the only credential that can read the zone or the account. It
records one verdict per record and per application on the packet's
`observe` step, raises or refreshes the estate alarm `dns_drift:algedonic.dev`
on any DRIFT or ABSENT, and writes exactly two kinds of thing: an
ABSENT declared Access application (created with its declared policies,
read back before it is judged) and a zone record declaring
`interlock = "access"`, once the application for that name reads
present with an allow policy.

## Runbook: the first observation after 198c5fe9 lands

The car that put `boss.algedonic.dev` behind the tunnel changed the
declaration, not the zone. The first observation after it converges is
the flip, and it will do this, in this order, on one packet:

1. Read the account's Access applications. `boss.algedonic.dev` is
   declared and absent → **created** (self-hosted, 24h session, one
   `allow` policy for `david@algedonic.dev`), then the account is read
   again. `playground.algedonic.dev` was declared blind (its session
   duration and policy were not readable from the pod that wrote the
   file) → expect **DRIFT** with both values on the packet, and the
   `dns_drift:algedonic.dev` alarm raised carrying it. **Correct
   `access.toml` from that read** (the alarm's `findings` show the live
   `session_duration` and `policies` verbatim); the next observation
   reads MATCH and the alarm is refreshed to nothing.
2. Read the zone. `boss.algedonic.dev CNAME` is ABSENT and the old
   `A 10.20.0.33` is UNDECLARED. The interlock reads the application
   just created → **the A record is deleted and the proxied CNAME to
   the tunnel created**, then the zone is read again; the packet's
   verdict for boss. is MATCH with `access: created`, and `applied`
   lists the three writes. If the application did not read back
   (`access: absent`) or has no allow policy, the record is untouched
   and the verdict is `HELD` with `flip held — Access app absent`; the
   next daily reading tries again.
3. From that moment `https://boss.algedonic.dev` answers through the
   edge: Cloudflare Access first (the operator's e-mail), then the
   tunnel, then the gateway's own OIDC login — whose redirect is
   already registered on the Kanidm `boss` client (2026-08-12; declared
   in `access.toml` with that provenance). `BOSS_PUBLIC_URL` flipped in
   the same car, so between the converge and this observation a login
   started from the internet lands on a callback only the LAN/WireGuard
   reaches. To close that window before the daily tick, file an
   observation by hand:

   ```
   cat > observation.json <<'EOF'
   {"kind": "dns-zone-observation", "title": "DNS zone observation: algedonic.dev",
    "subject": {"subject_kind": "custom", "id": "algedonic.dev"},
    "status": "open", "tags": [], "metadata": {"zone": "algedonic.dev", "area": "estate"}}
   EOF
   boss-api POST /api/jobs observation.json
   ```

   (one open packet per zone: the daily spawner files no twin while it
   is open, and the cadence sweep reports it SUPPRESSED, naming it).

4. WebAuthn credentials enrolled under the playground RP id (presence
   passkeys, the break-glass key) do not verify under `boss.` —
   re-enrol them at the new origin. Local break-glass auth is
   unaffected.

What the observer never does: correct a DRIFT application (it is
corrected from the read), write a record without an interlock (the
tunnel rotation owns `playground.`'s CNAME), or judge the zone before
the account has been read.
