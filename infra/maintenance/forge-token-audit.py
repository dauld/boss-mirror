#!/usr/bin/env python3
"""Report drift between the declared forge-token inventory and the forge.

Reads `infra/platform/forge-tokens.toml` and the Forgejo SQLite database, and
prints what disagrees. NEVER deletes a token and never reads one: the hash and
salt columns are excluded by name, and the only credential-shaped value printed
is `token_last_eight`, which is what Forgejo's own UI shows.

Exit codes follow the maintenance-check convention (see
infra/boss-audit-integrity-check.service): 0 clean, 2 drift found, 1 could not
run. Exit 2 makes the unit fail, which is what fires the alert and leaves the
maintenance Job open and loud.

WHY DRIFT AND NOT HEURISTICS. "Warn on tokens unused for 90 days" answers a
question nobody asked. The question that actually blocked a revocation on
2026-08-27 was "which of these may I delete", and no measurement of age can
answer it — only a statement of what SHOULD exist can. So the declaration is
the authority and this reports the difference, in both directions.
"""

import argparse
import datetime
import os
import sqlite3
import sys

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


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--declaration", default="/opt/boss/infra/platform/forge-tokens.toml")
    p.add_argument("--db", default="/opt/forgejo/data/gitea/gitea.db")
    p.add_argument("--user", default="david")
    p.add_argument("--stale-days", type=int, default=30)
    p.add_argument("--now", type=int, default=None,
                   help="unix seconds; for tests, so the run is deterministic")
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
        print("forge-token-audit: no forge database at %s" % e)
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

    if not findings and not unknown:
        print("forge-token-audit: clean — the forge matches the declaration")
        return 0
    print("forge-token-audit: %d finding(s), %d unattributed" % (len(findings), unknown))
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
