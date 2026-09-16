#!/usr/bin/env bash
# infra/cluster/dns/check-declared.sh <zone> [live.json] [options] —
# compare a DNS zone's LIVE records against the entries the tree
# declares for it in <zone>.toml beside this script (design 4c565f8c,
# backlog 5e58922c; the Talos comparator infra/cluster/talos/
# check-declared.sh is the sibling shape, design 16115a17 the vocabulary).
#
#   curl -sS -H "Authorization: Bearer $TOKEN" \
#     "https://api.cloudflare.com/client/v4/zones/$ZONE_ID/dns_records?per_page=1000" \
#     | infra/cluster/dns/check-declared.sh algedonic.dev --tunnel cloudflare-tunnel-credentials=<uuid>
#   infra/cluster/dns/check-declared.sh algedonic.dev live.json \
#     --tunnel-credentials cloudflare-tunnel-credentials=credentials.json
#
# WHY. Until 2026-09-16 every record of algedonic.dev was hand-set in
# the Cloudflare UI except one a Job created, and nothing recorded what
# the zone should say. The declaration is half; this is the other half
# — a comparator, so "the zone says what the tree says" is a read and
# not a belief (CLAUDE.md §Mostly sure vs. absolutely sure). The check
# takes the live records as INPUT because the only place the zone can
# be read with a credential is the credential broker (the `dns.observe`
# dispatcher handler, which runs THIS script over what it fetched);
# here it runs without one, on a file or stdin, in tests and by hand.
#
# INPUT: the body of `GET /zones/{id}/dns_records` — the v4 envelope
# (`{"result": [...]}`) or its bare `result` array. Anything else is
# refused with exit 2: an error body read as an empty zone would report
# every declared record ABSENT, and a wrong answer that looks like a
# finding is worse than no answer.
#
# REFERENCES. A declared `target = "tunnel:<credential id>"` is a
# Cloudflare Tunnel named by its credential-registry row; its content is
# `<tunnel id>.cfargotunnel.com`, and the id changes on every rotation,
# so the declaration never carries it. The caller resolves it:
#   --tunnel <credential id>=<tunnel uuid>          (the handler: TunnelID read
#                                                    off the installed Secret)
#   --tunnel-credentials <credential id>=<path>     (an operator: the
#                                                    credentials.json itself)
#   --list-references   print every reference the declaration makes,
#                       one per line, and exit 0 without reading input —
#                       how the handler learns what to resolve.
# An unresolved reference is exit 2 naming it, never a DRIFT.
#
# INTERLOCK. A record may declare `interlock = "access"`: the dns.observe
# handler APPLIES such a record (create when ABSENT, correct when DRIFT)
# only once the Cloudflare Access application access.toml declares for
# the same name reads present with an allow policy (backlog 198c5fe9).
# Or `interlock = "tunnel"`: applied once the latest cluster converge
# packet records the tunnel routing the name (tunnel_ingress) with the
# connector connected — for a hostname the tunnel serves that no Access
# application fronts, such as the identity provider (fd75c641).
# The comparator does not judge the interlock — it has no Access read —
# it carries the key through onto the record's verdict (`interlock`),
# so the handler learns which records it may apply from the one parse of
# the declaration and never re-reads the file itself. A value nothing
# honours is refused (exit 2): a record whose interlock no handler knows
# would be applied by nothing, silently.
#
# VOCABULARY, one line per record, then a summary:
#   MATCH      declared (name, type) present live with the declared
#              content, proxied and ttl
#   DRIFT      present live with a different value — BOTH printed
#   ABSENT     declared, not live
#   UNDECLARED live, named by no declaration — the third state: reported,
#              not a failure. Every record type in the zone is read.
# A (name, type) the zone holds more than once (round-robin A records,
# several MX) is compared against the copy whose content matches, and the
# other copies are UNDECLARED; a declaration naming one (name, type)
# twice is refused (exit 2) — declare the copies as distinct types or
# names, or fix the declaration.
#
#   --json    ONE JSON document on stdout instead of the lines:
#             {zone, declaration, counts{MATCH,DRIFT,ABSENT,UNDECLARED},
#              hard, verdicts:[{record, name, type, verdict, declared?,
#              live?, why?, interlock?}]} — what the observation packet
#              carries.
#
# EXIT: 0 every declared record MATCHes (UNDECLARED may be listed);
#       1 any DRIFT or ABSENT;
#       2 usage — no zone, no declaration for it, a declaration whose
#         `zone` is not the zone asked for, unreadable or non-list input,
#         an unresolved reference;
#      78 python3 (3.11+, for tomllib) is not on this box.
set -u

usage() {
    echo "usage: $0 <zone> [live.json] [--json] [--list-references] [--tunnel <cred>=<uuid>]... [--tunnel-credentials <cred>=<file>]...   (stdin when no file)" >&2
    echo "  declarations: \${BOSS_DNS_DECLARATIONS:-<beside this script>}/<zone>.toml" >&2
    exit 2
}

zone=""
live="-"
mode="lines"
list_refs=0
tunnel_args=()
while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help) usage ;;
        --json) mode="json" ;;
        --list-references) list_refs=1 ;;
        --tunnel)
            [ $# -ge 2 ] || usage
            tunnel_args+=("id" "$2"); shift ;;
        --tunnel-credentials)
            [ $# -ge 2 ] || usage
            tunnel_args+=("file" "$2"); shift ;;
        --*) echo "check-declared: unknown option $1" >&2; usage ;;
        *)
            if [ -z "$zone" ]; then zone="$1"
            elif [ "$live" = "-" ]; then live="$1"
            else usage
            fi ;;
    esac
    shift
done
[ -n "$zone" ] || usage

here="${BASH_SOURCE[0]%/*}"
[ "$here" != "${BASH_SOURCE[0]}" ] || here=.
declarations="${BOSS_DNS_DECLARATIONS:-$here}"
declared="$declarations/$zone.toml"
if [ ! -f "$declared" ]; then
    echo "check-declared: no declaration for zone '$zone' at $declared" >&2
    exit 2
fi
if [ "$live" != "-" ] && [ ! -r "$live" ]; then
    echo "check-declared: cannot read live records file: $live" >&2
    exit 2
fi
if ! command -v python3 >/dev/null 2>&1; then
    echo "check-declared: python3 is not on this box — cannot read the declaration" >&2
    exit 78
fi
if ! python3 -c 'import tomllib' 2>/dev/null; then
    echo "check-declared: python3 has no tomllib (3.11+) — cannot read the declaration" >&2
    exit 78
fi

# The program rides on fd 3, not stdin: stdin is the live zone when no
# file is named, and a heredoc on stdin would BE the program's stdin —
# the check would read an empty document and report every record ABSENT.
python3 /dev/fd/3 "$zone" "$declared" "$live" "$mode" "$list_refs" "${tunnel_args[@]}" 3<<'PY'
import json
import sys
import tomllib

zone, declared_path, live_path, mode, list_refs = sys.argv[1:6]
tunnel_argv = sys.argv[6:]
list_refs = list_refs == "1"

TUNNEL_SUFFIX = ".cfargotunnel.com"
COMPARED = ("content", "proxied", "ttl")
# `tunnel` (2026-09-16, fd75c641): released once the latest cluster
# converge packet's tunnel_ingress line routes the name and the
# connector read connected — the interlock for a hostname the tunnel
# serves that no Access application fronts (the identity provider).
INTERLOCKS = ("access", "tunnel")


def refuse(msg):
    print(f"check-declared: {msg}", file=sys.stderr)
    sys.exit(2)


# ---- the declaration ---------------------------------------------------

try:
    with open(declared_path, "rb") as f:
        dec = tomllib.load(f)
except (OSError, tomllib.TOMLDecodeError) as e:
    refuse(f"cannot read {declared_path}: {e}")

dec_zone = dec.get("zone")
if dec_zone != zone:
    refuse(f"{declared_path} declares zone {dec_zone!r}; asked to check {zone!r} — refusing to compare another zone's declaration")

records = dec.get("record")
if not isinstance(records, list) or not records:
    refuse(f"{declared_path} has no [[record]] entries")

declared = {}
for i, r in enumerate(records):
    if not isinstance(r, dict):
        refuse(f"{declared_path}: record {i} is not a table")
    for key in ("name", "type", "target", "proxied", "ttl", "why"):
        if key not in r:
            refuse(f"{declared_path}: record {i} ({r.get('name', '?')}) lacks `{key}` — every record carries name/type/target/proxied/ttl/why")
    name, rtype = str(r["name"]), str(r["type"]).upper()
    if name != zone and not name.endswith("." + zone):
        refuse(f"{declared_path}: record {name!r} is not in zone {zone!r}")
    if not isinstance(r["proxied"], bool):
        refuse(f"{declared_path}: record {name} {rtype}: `proxied` must be true or false")
    if not isinstance(r["ttl"], int) or isinstance(r["ttl"], bool):
        refuse(f"{declared_path}: record {name} {rtype}: `ttl` must be an integer (1 = auto)")
    if "interlock" in r and r["interlock"] not in INTERLOCKS:
        refuse(f"{declared_path}: record {name} {rtype}: interlock {r['interlock']!r} is one no handler honours (known: {', '.join(INTERLOCKS)})")
    key = (name, rtype)
    if key in declared:
        refuse(f"the declaration itself names {name} {rtype} twice — fix the declaration")
    declared[key] = r

references = sorted({str(r["target"]) for r in declared.values() if str(r["target"]).startswith("tunnel:")})
if list_refs:
    for ref in references:
        print(ref)
    sys.exit(0)

# ---- the references ----------------------------------------------------

resolved = {}
for kind, spec in zip(tunnel_argv[0::2], tunnel_argv[1::2]):
    if "=" not in spec:
        refuse(f"--tunnel{'-credentials' if kind == 'file' else ''} wants <credential id>=<value>, got {spec!r}")
    cred, value = spec.split("=", 1)
    if kind == "file":
        try:
            with open(value) as f:
                creds = json.load(f)
        except (OSError, ValueError) as e:
            refuse(f"cannot read credentials file for {cred}: {e}")
        value = creds.get("TunnelID") if isinstance(creds, dict) else None
        if not value:
            refuse(f"credentials file for {cred} carries no TunnelID")
    resolved["tunnel:" + cred] = str(value) + TUNNEL_SUFFIX

for ref in references:
    if ref not in resolved:
        refuse(f"the declaration references {ref} and nothing resolves it — pass --tunnel {ref[len('tunnel:'):]}=<tunnel uuid> (or --tunnel-credentials {ref[len('tunnel:'):]}=<credentials.json>)")


def declared_values(r):
    target = str(r["target"])
    content = resolved[target] if target.startswith("tunnel:") else target
    return {"content": content, "proxied": bool(r["proxied"]), "ttl": int(r["ttl"])}


# ---- the live zone -----------------------------------------------------

try:
    text = sys.stdin.read() if live_path == "-" else open(live_path).read()
    body = json.loads(text)
except OSError as e:
    refuse(f"cannot read the live records: {e}")
except ValueError as e:
    refuse(f"the live records are not JSON: {e}")

if isinstance(body, dict):
    if "result" not in body:
        refuse(f"the input is not a dns_records body (no `result`): {json.dumps(body)[:300]}")
    rows = body["result"]
else:
    rows = body
if not isinstance(rows, list):
    refuse("the input's `result` is not a list of records")

live = []
for i, row in enumerate(rows):
    if not isinstance(row, dict) or "name" not in row or "type" not in row or "content" not in row:
        refuse(f"live record {i} is not a DNS record (needs name, type, content): {json.dumps(row)[:200]}")
    live.append({
        "name": str(row["name"]),
        "type": str(row["type"]).upper(),
        "content": str(row["content"]),
        "proxied": bool(row.get("proxied", False)),
        "ttl": int(row.get("ttl", 1)),
    })


def live_values(rec):
    return {k: rec[k] for k in COMPARED}


def show(v):
    return "{" + ", ".join(f"{k}: {json.dumps(v[k])}" for k in COMPARED) + "}"


# ---- the comparison ----------------------------------------------------

verdicts = []
claimed = set()  # indexes into `live` matched to a declaration
for (name, rtype), r in declared.items():
    want = declared_values(r)
    label = f"{name} {rtype}"
    copies = [i for i, rec in enumerate(live) if rec["name"] == name and rec["type"] == rtype]
    entry = {"record": label, "name": name, "type": rtype, "why": str(r["why"]),
             "declared": dict(want, target=str(r["target"]))}
    if "interlock" in r:
        entry["interlock"] = str(r["interlock"])
    if not copies:
        entry["verdict"] = "ABSENT"
    else:
        exact = [i for i in copies if live_values(live[i]) == want]
        chosen = exact[0] if exact else copies[0]
        claimed.add(chosen)
        entry["live"] = live_values(live[chosen])
        entry["verdict"] = "MATCH" if exact else "DRIFT"
    verdicts.append(entry)

for i, rec in enumerate(live):
    if i in claimed:
        continue
    verdicts.append({"record": f"{rec['name']} {rec['type']}", "name": rec["name"], "type": rec["type"],
                     "verdict": "UNDECLARED", "live": live_values(rec)})

counts = {v: 0 for v in ("MATCH", "DRIFT", "ABSENT", "UNDECLARED")}
for v in verdicts:
    counts[v["verdict"]] += 1
hard = counts["DRIFT"] + counts["ABSENT"]
verdict = "every declared record matches" if hard == 0 else f"{hard} finding(s) against the declaration"
summary = (f"check-declared: {zone}: {counts['MATCH']} match, {counts['DRIFT']} drift, "
           f"{counts['ABSENT']} absent, {counts['UNDECLARED']} undeclared — {verdict}")

if mode == "json":
    print(json.dumps({"zone": zone, "declaration": declared_path, "counts": counts, "hard": hard,
                      "summary": summary, "verdicts": verdicts}, indent=1))
else:
    print(f"read       {len(live)} live record(s) in {zone} against {declared_path}")
    for v in verdicts:
        if v["verdict"] == "MATCH":
            print(f"MATCH      {v['record']}")
        elif v["verdict"] == "DRIFT":
            print(f"DRIFT      {v['record']}  declared {show(v['declared'])}  live {show(v['live'])}")
        elif v["verdict"] == "ABSENT":
            print(f"ABSENT     {v['record']}  declared, not live: {show(v['declared'])}")
        else:
            print(f"UNDECLARED {v['record']}  live, no declaration names it: {show(v['live'])}")
    print(summary)
sys.exit(1 if hard else 0)
PY
