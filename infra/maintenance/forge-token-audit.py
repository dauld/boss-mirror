#!/usr/bin/env python3
"""Report drift between the declared forge-token inventory and the forge.

Reads `infra/platform/forge-tokens.toml`, the Forgejo SQLite database, and —
when configured — the credentials registry (`GET /api/credentials` on the jobs
API), and prints what disagrees. NEVER deletes a token and never reads one:
the hash and salt columns are excluded by name, and the only credential-shaped
value printed is `token_last_eight`, which is what Forgejo's own UI shows. The
registry side is values-free by construction — its rows carry storage
locations, never contents.

Exit codes follow the maintenance-check convention (see
infra/boss-audit-integrity-check.service): 0 clean, 2 drift found, 1 could not
run (including: a configured registry that could not be read — the run is
honest about being partial). Exit 2 makes the unit fail, which is what fires
the alert and leaves the maintenance Job open and loud.

WHY DRIFT AND NOT HEURISTICS. "Warn on tokens unused for 90 days" answers a
question nobody asked. The question that actually blocked a revocation on
2026-08-27 was "which of these may I delete", and no measurement of age can
answer it — only a statement of what SHOULD exist can. So the declaration is
the authority and this reports the difference, in both directions.

TWO DECLARATIONS, ONE COMPARISON EACH (CLAUDE.md 9a: this run is the equality
test that keeps them from silently disagreeing). forge-tokens.toml is the
forge-host inventory — per-token, hand-maintained, including the UNKNOWNs it
exists to name. The credentials registry is the platform's knowledge base —
per-credential, where a rotation-minted instance carries a packet-derived name
(`{id}-{first 8 of packet id}`), so registry matching is by id-prefix rather
than exact name. Both are compared against the live forge; a live token in
neither is doubly loud.

CAPABILITY, HONESTLY BOUNDED. The live-token side is readable credential-free
only on the forge host (the SQLite database). Reading it remotely would need
an admin-scoped forge token, which lives dispatcher-side as
BOSS_BROKER_FORGEJO_TOKEN and deliberately nowhere else — so run elsewhere,
this script says exactly what it cannot check rather than guessing.
"""

import argparse
import datetime
import json
import os
import sqlite3
import sys
import urllib.request

try:
    import tomllib  # 3.11+
except ModuleNotFoundError:  # pragma: no cover - exercised on older interpreters
    tomllib = None

SAFE_COLUMNS = ("id", "name", "scope", "created_unix", "updated_unix",
                "token_last_eight")
FORBIDDEN = {"token_hash", "token_salt"}


def when(v):
    if not v:
        return "never"
    return datetime.datetime.utcfromtimestamp(v).strftime("%Y-%m-%d %H:%M UTC")


def age_days(v, now):
    if not v:
        return None
    return int((now - v) / 86400)


def parse_declaration_minimal(text):
    """A deliberately tiny, STRICT reader for this one file's shape.

    `tomllib` only arrived in Python 3.11. The forge host runs 3.12 and the CI
    image is bookworm/3.11, so production never needs this — but a developer
    machine on 3.9 would otherwise be unable to run the script OR its tests,
    and a test that cannot run locally is one nobody runs before pushing.

    Handles exactly what forge-tokens.toml uses: `[[token]]` tables whose
    values are double-quoted single-line strings. Everything else RAISES rather
    than guessing, because a declaration silently half-parsed would report
    drift that is not there and, worse, miss drift that is.
    """
    tokens = []
    cur = None
    for lineno, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line == "[[token]]":
            cur = {}
            tokens.append(cur)
            continue
        if line.startswith("["):
            raise ValueError(
                "line %d: only [[token]] tables are understood, got %r" % (lineno, line))
        if cur is None:
            raise ValueError("line %d: key outside any [[token]] table: %r" % (lineno, line))
        key, sep, val = line.partition("=")
        if not sep:
            raise ValueError("line %d: expected `key = \"value\"`, got %r" % (lineno, line))
        key, val = key.strip(), val.strip()
        if len(val) < 2 or not val.startswith('"') or not val.endswith('"'):
            raise ValueError(
                "line %d: %r must be one double-quoted line; multi-line and bare "
                "values are not supported here" % (lineno, key))
        cur[key] = val[1:-1].replace('\\"', '"')
    return tokens


def load_declared(path):
    if tomllib is not None:
        with open(path, "rb") as fh:
            rows = tomllib.load(fh).get("token", [])
    else:
        with open(path, "r") as fh:
            rows = parse_declaration_minimal(fh.read())
    problems = []
    for i, t in enumerate(rows):
        for key in ("name", "consumer", "scope", "installed_at"):
            if not t.get(key):
                problems.append("declared token #%d is missing `%s`" % (i + 1, key))
    return rows, problems


def load_live(db_path, username):
    if not os.path.exists(db_path):
        raise FileNotFoundError(db_path)
    con = sqlite3.connect("file:%s?mode=ro" % db_path, uri=True)
    con.row_factory = sqlite3.Row
    have = {r["name"] for r in con.execute("PRAGMA table_info(access_token)")}
    cols = [c for c in SAFE_COLUMNS if c in have and c not in FORBIDDEN]
    user = con.execute(
        "SELECT id FROM user WHERE lower_name = ?", (username,)
    ).fetchone()
    if user is None:
        raise LookupError(username)
    rows = con.execute(
        "SELECT %s FROM access_token WHERE uid = ? ORDER BY created_unix"
        % ", ".join(cols),
        (user["id"],),
    ).fetchall()
    return [dict(r) for r in rows]


def audit(declared, live, stale_days, now):
    """Return (findings, unknown_count). A finding is (severity, text)."""
    findings = []
    by_name = {t["name"]: t for t in declared}
    live_names = {t["name"] for t in live}

    for t in live:
        name = t["name"]
        d = by_name.get(name)
        if d is None:
            findings.append((
                "UNDECLARED",
                "token %r (%s, ends %s) exists on the forge and is in no declaration. "
                "Either declare its consumer or delete it — an unattributable "
                "credential is one nobody dares revoke."
                % (name, t.get("scope") or "no scope", t.get("token_last_eight")),
            ))
            continue
        if (d.get("scope") or "") != (t.get("scope") or ""):
            findings.append((
                "SCOPE-DRIFT",
                "token %r is declared %r but the forge says %r"
                % (name, d.get("scope"), t.get("scope")),
            ))
        age = age_days(t.get("updated_unix"), now)
        if age is not None and age >= stale_days:
            findings.append((
                "STALE",
                "token %r last used %s (%d days ago), held by: %s"
                % (name, when(t.get("updated_unix")), age, d.get("consumer")),
            ))

    for d in declared:
        if d["name"] not in live_names:
            findings.append((
                "MISSING",
                "token %r is declared (consumer: %s) but does not exist on the "
                "forge. Either it was revoked without updating this file, or a "
                "consumer is about to fail." % (d["name"], d.get("consumer")),
            ))

    unknown = sum(
        1 for d in declared
        if d["name"] in live_names and str(d.get("consumer", "")).strip().upper() == "UNKNOWN"
    )
    return findings, unknown


def load_registry(registry_json, registry_url):
    """Return (rows, limit_message). rows is None when the registry was not
    read; limit_message says why when a configured source failed. Neither
    source needs a credential: the fixture is a file, and the jobs API's
    internal address trusts header-less callers for this read-only surface.
    """
    if registry_json:
        try:
            with open(registry_json) as fh:
                return json.load(fh), None
        except (OSError, ValueError) as e:
            return None, "cannot read registry fixture %s: %s" % (registry_json, e)
    if registry_url:
        url = registry_url.rstrip("/") + "/api/credentials"
        try:
            with urllib.request.urlopen(url, timeout=10) as resp:
                return json.loads(resp.read().decode("utf-8")), None
        except (OSError, ValueError) as e:
            # URLError (and its HTTPError subclass) are OSErrors; a
            # non-JSON body is a ValueError. Either way the direction
            # did not run, and saying so beats a silent skip.
            return None, "credentials registry at %s unreachable: %s" % (url, e)
    return None, None


def audit_registry(registry, live, user):
    """The registry direction, both ways. Returns (findings, skipped_rows).

    A live forge token belongs to a registry row when its name IS the row id
    or starts with `{id}-` — rotation-minted instances carry packet-derived
    names (`boss-dev-forge-token-7ee101aa`), and the row id is the durable
    identity they derive from. Rows whose principal does not mention the
    audited user are skipped (their tokens live under another account and
    this run cannot see them); the count is reported so the skip is visible.
    """
    findings = []

    def owns(row_id, name):
        return name == row_id or name.startswith(row_id + "-")

    forge_rows = [r for r in registry if r.get("kind") == "forgejo-access-token"]
    mine = [r for r in forge_rows if user in (r.get("principal") or "")]
    skipped = len(forge_rows) - len(mine)

    matched_ids = set()
    for t in live:
        name = t["name"]
        owners = [r for r in mine if owns(r["id"], name)]
        if not owners:
            findings.append((
                "REG-UNDECLARED",
                "token %r (%s, ends %s) exists on the forge and matches no "
                "credentials-registry row. Scope questions about it are "
                "experiments, not lookups — register it (GET /api/credentials "
                "is the reader) or delete it."
                % (name, t.get("scope") or "no scope", t.get("token_last_eight")),
            ))
            continue
        # Longest id wins: `boss-dev-forge-token-x` must credit the row
        # `boss-dev-forge-token`, not a hypothetical shorter prefix row.
        row = max(owners, key=lambda r: len(r["id"]))
        matched_ids.add(row["id"])
        declared_scopes = row.get("scopes") or []
        live_scopes = {s.strip() for s in (t.get("scope") or "").split(",") if s.strip()}
        if not declared_scopes:
            findings.append((
                "REG-SCOPE-UNVERIFIED",
                "registry row %r declares no scopes; the forge says its token %r "
                "carries %r — record that in the row, so the next scope question "
                "is a lookup." % (row["id"], name, t.get("scope") or ""),
            ))
        elif set(declared_scopes) != live_scopes:
            findings.append((
                "REG-SCOPE-DRIFT",
                "registry row %r declares scopes %s but forge token %r carries %r"
                % (row["id"], ",".join(sorted(declared_scopes)), name,
                   t.get("scope") or ""),
            ))

    for r in mine:
        if r["id"] not in matched_ids:
            findings.append((
                "REG-MISSING",
                "registry row %r (storage: %s) matches no live forge token for "
                "user %r — either the token was revoked without updating the "
                "registry, or the row does not record its forge-side token name "
                "yet." % (r["id"], r.get("storage_location"), user),
            ))
    return findings, skipped


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--declaration", default="/opt/boss/infra/platform/forge-tokens.toml")
    p.add_argument("--db", default="/opt/forgejo/data/gitea/gitea.db")
    p.add_argument("--user", default="david")
    p.add_argument("--stale-days", type=int, default=30)
    p.add_argument("--now", type=int, default=None,
                   help="unix seconds; for tests, so the run is deterministic")
    p.add_argument("--registry-url", default=None,
                   help="jobs API base (e.g. http://10.20.0.34:7900); enables the "
                        "forge-vs-credentials-registry direction")
    p.add_argument("--registry-json", default=None,
                   help="a file holding the GET /api/credentials response; for "
                        "tests, and for a host that cannot reach the jobs API")
    args = p.parse_args(argv)

    now = args.now if args.now is not None else int(
        datetime.datetime.now(datetime.timezone.utc).timestamp())

    try:
        declared, decl_problems = load_declared(args.declaration)
    except OSError as e:
        print("forge-token-audit: cannot read declaration: %s" % e)
        return 1
    try:
        live = load_live(args.db, args.user)
    except FileNotFoundError as e:
        print("forge-token-audit: no forge database at %s — the live-token side is "
              "readable credential-free only on the forge host. Reading it remotely "
              "needs an admin-scoped forge token, which lives dispatcher-side as "
              "BOSS_BROKER_FORGEJO_TOKEN and deliberately not here; nothing was "
              "checked." % e)
        return 1
    except LookupError as e:
        print("forge-token-audit: no user %r in the forge database" % str(e))
        return 1

    print("forge-token-audit: %d declared, %d live on the forge for user %r"
          % (len(declared), len(live), args.user))

    if decl_problems:
        for m in decl_problems:
            print("  MALFORMED  %s" % m)
        print("forge-token-audit: declaration is malformed; fix it before trusting a clean run")
        return 2

    findings, unknown = audit(declared, live, args.stale_days, now)

    registry, registry_limit = load_registry(args.registry_json, args.registry_url)
    limited = False
    if registry is not None:
        reg_findings, reg_skipped = audit_registry(registry, live, args.user)
        forgejo_rows = sum(1 for r in registry if r.get("kind") == "forgejo-access-token")
        print("forge-token-audit: credentials registry holds %d row(s), %d of kind "
              "forgejo-access-token" % (len(registry), forgejo_rows))
        if reg_skipped:
            print("  %-12s %d registry row(s) belong to other principals and were "
                  "not checked against user %r's tokens"
                  % ("REG-SKIPPED", reg_skipped, args.user))
        findings += reg_findings
    elif registry_limit:
        # A configured registry that could not be read: the run is
        # partial and must say so rather than passing as complete.
        print("  %-12s forge-vs-registry NOT checked: %s" % ("LIMIT", registry_limit))
        limited = True
    else:
        print("forge-token-audit: no --registry-url/--registry-json; "
              "forge-vs-credentials-registry direction not checked")

    for sev, text in sorted(findings):
        print("  %-12s %s" % (sev, text))

    if unknown:
        print("  %-12s %d declared token(s) still have consumer = UNKNOWN. These are "
              "live credentials nobody can attribute; that is the debt this audit "
              "exists to retire." % ("UNATTRIBUTED", unknown))

    write_repo = [t for t in live if "write:repository" in (t.get("scope") or "")]
    if len(write_repo) > 1:
        print("  %-12s %d tokens carry write:repository (%s). Each is a full push "
              "credential; the fewer that exist, the smaller a disclosure is."
              % ("BREADTH", len(write_repo), ", ".join(sorted(t["name"] for t in write_repo))))

    if findings or unknown:
        print("forge-token-audit: %d finding(s), %d unattributed" % (len(findings), unknown))
        return 2
    if limited:
        print("forge-token-audit: no drift found, but the run was PARTIAL (see LIMIT above)")
        return 1
    print("forge-token-audit: clean — the forge matches the declaration%s"
          % (" and the credentials registry" if registry is not None else ""))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
