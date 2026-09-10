#!/usr/bin/env bash
#
# api-path-bypass-smell — the categorical guard against "end-arounds":
# loading DATA into the database outside the public API.
#
# THE PRINCIPLE
# -------------
# Domain + seed data is written by a SERVICE (behind its port adapter)
# reacting to a public-API call — never poured straight into the DB by a
# script or a seed binary. A direct-DB data write skips policy, validation,
# and the audit/event path the rest of the system is built on; the sim and
# the model then drift (see docs/design/seed-vs-emergent-state.md and the
# "prepare the model via the API" rule). This lint fails CI on any such
# end-around so they can't creep back in.
#
# READ VERSUS RUN
# ---------------
# A DML verb in a shell script is not a write. `psql -c "INSERT INTO
# workflows …"` runs it; `grep -rn "INSERT INTO workflows" schema/` reads for
# it. Until 2026-09-10 this lint could not tell the two apart, so every script
# whose JOB is to audit SQL paid a tax: five lints state their DML shapes only
# inside comments, and emitted-kinds-are-declared.sh carried a four-line NB
# explaining that its awk anchor was written `INSERT[ ]+INTO` "so this READER
# of schema SQL is not mistaken for a WRITER of it by api-path-bypass-smell".
# A comment standing where a mechanism belongs is the defect (backlog
# 24d7db5d); the mechanism is below.
#
# The test is the INVOCATION, not the verb, and it is two-stage:
#
#   1. DOES THIS FILE REACH POSTGRES AT ALL?  A DML string can only be
#      executed by something that speaks to the server: psql, pgcli,
#      pg_dump/pg_restore/pgbench, a PG* connection variable, `sudo -u
#      postgres`, a postgres:// URL. A file carrying none of those cannot run
#      SQL, whatever strings it holds. A client token is discounted only when
#      it is (a) in a comment, or (b) inside a quoted string on a line that
#      also invokes a text tool or assigns a pattern variable — i.e. the token
#      is this script's DATA. A BARE client word always counts, wherever it
#      sits, so `… | psql` can never be talked away.
#
#   2. IF IT DOES, EVERY DML LINE IS A WRITE.  No read exemption applies
#      inside a file that holds a live connection, because a text tool there
#      can be GENERATING the SQL: `printf "INSERT INTO schema_migrations …"`
#      in infra/postgres/migrate.sh is piped to psql one line later, and
#      `printf '%s\n' "UPDATE sim_clock SET …"` in infra/seed-brewery-tenant.sh
#      is piped to psql five lines later. Both are runs. Only comments and
#      pure DDL are excused there.
#
#   3. IF IT DOES NOT, a DML line is a READ when its invocation is a text tool
#      (grep/egrep/fgrep/rg/ag/ack/awk/sed/echo/printf), a `=~` test, or an
#      assignment to a pattern-named variable (*_RE, *_REGEX, *_RX, *_PAT,
#      *_PATTERN, *_GREP, *_MATCH, *_NEEDLE, *_PHRASE). "Its invocation" is
#      the line that OPENED the construct the DML sits in — so a DML anchor
#      inside a multi-line `awk '…'` program is read off the `awk` line, and a
#      heredoc body is read off its opener.
#
# AMBIGUOUS STILL FAILS. A false negative here is a silent bypass of the audit
# log, strictly worse than the false positive being removed, so anything the
# ladder above does not recognise is reported: DML handed to an unrecognised
# command (`q "INSERT INTO …"`, migrate.sh:164), DML assigned to a variable
# whose name does not say "pattern", DML inside a heredoc whose opener is not a
# text tool, a quoted client word on a line carrying `$(…)` or a backtick
# (the substitution could be running the client rather than naming it), and
# every DML line in a file that reaches Postgres. The self-test pins each.
#
# WHAT IS ALLOWED (never flagged)
# -------------------------------
#   - service adapters:  crates/*/*/src/postgres.rs   (the Pg port impls —
#                        the ONE legitimate place domain tables are written)
#   - rebuilders:        crates/*/*/src/rebuild.rs    (recompute projections
#                        FROM the audit_log — derived state, not new data)
#   - schema / DDL:      CREATE / DROP / ALTER, dropdb / createdb, migrate.sh
#   - reads:             SELECT / to_regclass / EXISTS …, and the read
#                        positions described above
#   - known stop-gaps:   the ALLOWLIST below — each tied to a reason. Clearing
#                        an entry must re-trip the lint.
#
# WHAT IS FLAGGED
# ---------------
#   1. shell scripts (infra/**/*.sh): a `psql` that runs write-DML
#      (INSERT/UPDATE…SET/COPY/DELETE) or `-f`-loads a tenant-seed .sql.
#   2. seed/sim Rust binaries (crates/**/src/bin/**, boss-sim): a sqlx write
#      (INSERT/UPDATE…SET/DELETE) — those belong in a service adapter, reached
#      over HTTP, not in a one-shot bin.
#
# Following the no-secrets / invariant-register precedent of checks that prove
# themselves, every run starts with a self-test: eleven fixture scripts in a
# temp dir, each asserted read or run by the classifier. `--self-test` runs
# just that and stops. The fixtures are written with printf one-liners, which
# the classifier reads as reads — the lint demonstrating itself.
#
# Usage:  infra/lint/api-path-bypass-smell.sh [--strict|--self-test]
#   --strict ignores the allowlist (use when clearing a stop-gap).
#
# The CI hook lives in .github/workflows/ci.yml alongside the other lints.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

STRICT=0

# Known stop-gaps: "<path-fragment>::<reason>". A hit whose file path contains
# the fragment is skipped (unless --strict). Removing an entry re-trips the
# lint — the workflow is: land the API path, drop the entry.
ALLOWLIST=(
    # init runs pre-API (clock-api isn't up yet), so the demo-epoch sim_clock
    # prime is a direct control-plane write here — the API path can't reach it
    # this early. Tenant DATA seeding (classes/Workflows/policy/accounts/…) has
    # moved onto the converged 'boss-brewery-sim prepare' step (post-API).
    "infra/oss-quickstart/init.sh::pre-API sim_clock prime — control-plane, clock-api not up at init"
    # The restart-epoch baseline marker is clock control-plane with no API yet.
    # Follow-up: a clock-api 'stamp current state as baseline' endpoint.
    "infra/seed-brewery-tenant.sh::sim_clock restart-baseline marker — control-plane, no API yet"
    # Write-roundtrip *diagnostic*: it writes then asserts the write is visible.
    # Not data-loading; the DELETE is its own cleanup.
    "infra/check-service-write-roundtrip.sh::diagnostic write-roundtrip probe"
    # Retention/GC, not data-loading: purges expired message events on a timer.
    "crates/modules/boss-messages/src/bin/boss_messages_events_purge.rs::message-events retention GC"
    # Migration bookkeeping, not domain data: schema_migrations records which
    # manifest entries a database has applied. Permanent — DDL's ledger is
    # control-plane by nature (docs/design/schema-migrations.md).
    "infra/postgres/migrate.sh::schema_migrations bookkeeping — control-plane, the migration runner itself"
)

allowlisted() {  # $1 = "file:line:content"
    [[ "$STRICT" == 1 ]] && return 1
    local file="${1%%:*}"
    for entry in "${ALLOWLIST[@]}"; do
        local frag="${entry%%::*}"
        [[ "$file" == *"$frag"* ]] && return 0
    done
    return 1
}

# The three patterns the shell classifier keys on. Handed to awk through the
# environment, not -v, so no backslash in them is eaten by awk's -v escape
# processing.
export DML_RE='(INSERT[[:space:]]+INTO|UPDATE[[:space:]]+[A-Za-z_."]+[[:space:]]+SET|COPY[[:space:]]+[A-Za-z_."]+|DELETE[[:space:]]+FROM)'
export SEED_SQL_RE='psql[^|]*-f[[:space:]]+[^ ]*(classes|seed|playground)[^ ]*\.sql'
# Anything that can carry a statement to a Postgres server. Matched against a
# lowercased line, so PGPASSWORD and pgpassword read the same.
export DB_CLIENT_RE='(^|[^[:alnum:]_./-])(psql|pgcli|pg_dump|pg_dumpall|pg_restore|pgbench|psycopg|pgpassword|pghost|pgdatabase|pguser|pgport|pgservice)([^[:alnum:]_-]|$)|sudo[^|]*-u[[:space:]]+postgres|postgres(ql)?://'

hits=0

report() {  # $1 = category, $2 = "file:line:content"
    if allowlisted "$2"; then return; fi
    echo "  [$1] ${2}"
    hits=$((hits + 1))
}

# --- the shell classifier -----------------------------------------------------
# scan_shell <root>… — prints "<category>\t<file>:<line>:<content>" for every
# shell line that RUNS write-DML, and nothing for lines that merely read it.
# The awk program below lexes each file once so it can answer two questions a
# line-at-a-time grep cannot: which text is code rather than a comment or the
# inside of a quote, and which line OPENED the construct a given line sits in.
scan_shell() {
    local -a files=()
    local f
    while IFS= read -r f; do
        [[ -n "$f" ]] && files+=("$f")
    done < <(find "$@" -type f -name '*.sh' 2>/dev/null)
    [[ ${#files[@]} -eq 0 ]] && return 0

    awk '
    BEGIN { DML = ENVIRON["DML_RE"]; SEEDSQL = ENVIRON["SEED_SQL_RE"]; CLIENT = ENVIRON["DB_CLIENT_RE"] }

    # read_pos — is this line a READ invocation: a text tool, a =~ test, or an
    # assignment to a pattern-named variable? Matched lowercased so GREP and
    # grep read alike; the leading class keeps a path like no-grep.sh from
    # counting as an invocation of grep.
    function read_pos(s,   low) {
        low = tolower(s)
        if (low ~ /(^|[^[:alnum:]_.\/-])(grep|egrep|fgrep|rg|ag|ack|awk|sed|echo|printf)([^[:alnum:]_-]|$)/) return 1
        if (index(s, "=~") > 0) return 1
        if (low ~ /^[[:space:]]*(local[[:space:]]+|export[[:space:]]+|readonly[[:space:]]+|declare[[:space:]]+-[a-z]+[[:space:]]+)?([a-z_][a-z0-9_]*_)?(re|regex|rx|pattern|pat|grep|match|needle|phrase)s?=/) return 1
        return 0
    }

    # lex — walk one line from the carried quote state, filling CODE (everything
    # that is not a comment), BARE (the same with quoted runs blanked) and
    # QUOTED (only the quoted runs). Sets PENDHD when the line opens a heredoc.
    function lex(line,   i, n, c, d, rest) {
        CODE = ""; BARE = ""; QUOTED = ""; PENDHD = ""
        n = length(line); i = 1
        while (i <= n) {
            c = substr(line, i, 1)
            if (ST == "") {
                if (c == "\\")   { CODE = CODE c substr(line, i+1, 1); BARE = BARE c substr(line, i+1, 1); i += 2; continue }
                if (c == "\047") { ST = "sq"; BARE = BARE " "; QUOTED = QUOTED " "; i++; continue }
                if (c == "\"")   { ST = "dq"; BARE = BARE " "; QUOTED = QUOTED " "; i++; continue }
                if (c == "#" && (i == 1 || substr(line, i-1, 1) ~ /[[:space:];|&()]/)) break
                if (c == "<" && substr(line, i+1, 1) == "<") {
                    rest = substr(line, i+2)
                    if (match(rest, /^-?[[:space:]]*[\047"]?[A-Za-z_][A-Za-z0-9_]*/)) {
                        d = substr(rest, RSTART, RLENGTH)
                        sub(/^-?[[:space:]]*[\047"]?/, "", d)
                        PENDHD = d
                    }
                }
                CODE = CODE c; BARE = BARE c; i++
            } else if (ST == "sq") {
                if (c == "\047") { ST = ""; i++; continue }
                CODE = CODE c; QUOTED = QUOTED c; i++
            } else {
                if (c == "\\")   { CODE = CODE substr(line, i+1, 1); QUOTED = QUOTED substr(line, i+1, 1); i += 2; continue }
                if (c == "\"")   { ST = ""; i++; continue }
                CODE = CODE c; QUOTED = QUOTED c; i++
            }
        }
    }

    FNR == 1 { ST = ""; HD = ""; HDOPEN = 0; CTXOPEN = 0; CONT = 0; files[FILENAME] = 1 }

    {
        ln[FILENAME, FNR] = $0
        if (HD != "") {
            # A heredoc body: every character is data, and the invocation that
            # owns it is the line that opened it.
            ctxline = HDOPEN
            CODE = $0; BARE = $0; QUOTED = ""
            t = $0; sub(/^[[:space:]]*/, "", t); sub(/[[:space:]]*$/, "", t)
            if (t == HD) { HD = ""; CONT = 0; ST = ""; CODE = ""; BARE = "" }
        } else {
            if (ST == "" && CONT == 0) CTXOPEN = FNR
            ctxline = CTXOPEN
            lex($0)
            CONT = (ST == "" && $0 ~ /\\$/) ? 1 : 0
            if (PENDHD != "" && ST == "") { HD = PENDHD; HDOPEN = CTXOPEN; CONT = 0 }
        }
        ctx[FILENAME, FNR] = ctxline
        code[FILENAME, FNR] = CODE
        bare[FILENAME, FNR] = BARE
        quoted[FILENAME, FNR] = QUOTED
        last[FILENAME] = FNR
    }

    END {
        for (f in files) {
            # Stage 1: does this file reach Postgres at all? A bare client word
            # always counts. One inside a quote counts unless the line is a read
            # position — and counts regardless if the line carries a substitution,
            # which could be running the client rather than naming it.
            inv = 0
            for (i = 1; i <= last[f]; i++) {
                if (tolower(bare[f, i]) ~ CLIENT) { inv = 1; break }
                if (tolower(quoted[f, i]) ~ CLIENT) {
                    if (!read_pos(ln[f, i]) || ln[f, i] ~ /\$\(|`/) { inv = 1; break }
                }
            }
            for (i = 1; i <= last[f]; i++) {
                c = code[f, i]
                isdml = (c ~ DML); isseed = (c ~ SEEDSQL)
                if (!isdml && !isseed) continue
                # Pure DDL stays out of scope, as it always has.
                if (isdml && !isseed && tolower(ln[f, i]) ~ /create[[:space:]]|drop[[:space:]]|alter[[:space:]]/) continue
                if (!inv) {
                    j = ctx[f, i]
                    if (read_pos(ln[f, j]) || read_pos(ln[f, i])) continue
                }
                printf "%s\t%s:%d:%s\n", (isdml ? "shell-dml" : "shell-seed-sql"), f, i, ln[f, i]
            }
        }
    }
    ' "${files[@]}" | LC_ALL=C sort
}

# --- self-test ----------------------------------------------------------------
# Eleven fixtures, named for the verdict they must get: read-* must produce no
# hit, run-* must produce one. Each is written with a printf one-liner, so the
# planted DML lives in a read position in THIS file — the classifier's own
# source is its first fixture.
self_test() {
    local tmp out base fails=0 q="'"
    tmp="$(mktemp -d)"

    # READS — no DB client anywhere in the file, DML in a read position.
    printf '#!/usr/bin/env bash\ngrep -rn "INSERT INTO workflows" infra/postgres/schema\n' \
        > "$tmp/read-grep-pattern.sh"
    printf '#!/usr/bin/env bash\necho "  a seed file must never INSERT INTO workflows directly" >&2\n' \
        > "$tmp/read-help-echo.sh"
    printf '#!/usr/bin/env bash\nawk %s\n  /INSERT INTO event_kinds/ { n++ }\n  END { print n }\n%s "$1"\n' "$q" "$q" \
        > "$tmp/read-awk-program.sh"
    printf '#!/usr/bin/env bash\nDML_PATTERN="UPDATE dispatcher_rules SET status"\ngrep -E "$DML_PATTERN" "$1"\n' \
        > "$tmp/read-pattern-variable.sh"
    printf '#!/usr/bin/env bash\ngrep -rn "psql -f brewery-seed.sql" infra\n' \
        > "$tmp/read-grep-seed-load.sh"
    printf '#!/usr/bin/env bash\n# this script asks the API to INSERT INTO workflows for it\npsql -c "SELECT 1"\n' \
        > "$tmp/read-comment-in-a-psql-script.sh"

    # RUNS — a live connection, or a shape the ladder does not recognise.
    printf '#!/usr/bin/env bash\npsql -c "INSERT INTO workflows (name) VALUES (%sx%s)"\n' "$q" "$q" \
        > "$tmp/run-psql-dash-c.sh"
    printf '#!/usr/bin/env bash\npsql <<SQL\nINSERT INTO workflows (name) VALUES (1);\nSQL\n' \
        > "$tmp/run-psql-heredoc.sh"
    printf '#!/usr/bin/env bash\n{\n  printf "INSERT INTO schema_migrations VALUES (1);\\n"\n} | psql -X\n' \
        > "$tmp/run-printf-into-psql.sh"
    printf '#!/usr/bin/env bash\nq "INSERT INTO schema_migrations (id) VALUES (1)"\nPSQL=(psql)\n' \
        > "$tmp/run-opaque-helper.sh"
    printf '#!/usr/bin/env bash\npsql -f examples/brewery/classes.sql\n' \
        > "$tmp/run-psql-loads-seed-sql.sh"

    out="$(scan_shell "$tmp" || true)"

    for base in $(cd "$tmp" && ls ./*.sh | sed 's|^\./||'); do
        case "$base" in
            read-*)
                if printf '%s\n' "$out" | grep -q "/$base:"; then
                    echo "api-path-bypass-smell self-test FAIL: $base is a READ but was reported as a write" >&2
                    printf '%s\n' "$out" | grep "/$base:" >&2
                    fails=1
                fi ;;
            run-*)
                if ! printf '%s\n' "$out" | grep -q "/$base:"; then
                    echo "api-path-bypass-smell self-test FAIL: $base RUNS DML but was not reported" >&2
                    fails=1
                fi ;;
        esac
    done

    # The seed-sql loader must be reported under its own category, and the grep
    # that merely quotes that shape must not be.
    if ! printf '%s\n' "$out" | grep -q "^shell-seed-sql.*/run-psql-loads-seed-sql.sh:"; then
        echo "api-path-bypass-smell self-test FAIL: a psql -f seed load lost its shell-seed-sql category" >&2
        fails=1
    fi

    rm -rf "$tmp"
    if [[ "$fails" -ne 0 ]]; then
        echo "api-path-bypass-smell: self-test FAILED — the read/run classifier cannot be trusted, fix it first" >&2
        exit 1
    fi
    echo "api-path-bypass-smell: self-test ok — 6 reads passed (a grep pattern, a help-text echo, a multi-line awk anchor, a pattern variable, a quoted seed-load shape, a comment inside a psql script) and 5 runs reported (psql -c, a psql heredoc, a printf piped to psql, DML handed to an unrecognised helper, a psql -f seed load)"
}

main_scan() {
    local kind hit line
    while IFS=$'\t' read -r kind hit; do
        [[ -z "${kind:-}" ]] && continue
        report "$kind" "$hit"
    done < <(scan_shell infra)

    # --- rust: sqlx write-DML in seed/sim binaries ----------------------------
    # Scope to bin entrypoints + the sim; exclude the legit writers (adapters,
    # rebuilders) and tests. No read/run split here: nothing under those paths
    # reads SQL as text today, so the day one does is the day this grep grows a
    # classifier too.
    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        report "rust-bin-sqlx" "$line"
    done < <(
        grep -rnE 'INSERT[[:space:]]+INTO|UPDATE[[:space:]]+[A-Za-z_]+[[:space:]]+SET|DELETE[[:space:]]+FROM' \
            crates --include='*.rs' 2>/dev/null \
        | grep -E '/src/bin/|/boss-sim/' \
        | grep -viE 'postgres\.rs|rebuild\.rs|/tests/|in_memory|#\[cfg\(test\)\]' \
        || true
    )

    if [[ "$hits" -gt 0 ]]; then
        echo ""
        echo "api-path-bypass-smell: $hits direct-DB data write(s) bypassing the public API." >&2
        echo "Load data through the API (a service behind its port), or — if genuinely" >&2
        echo "control-plane/maintenance — add an ALLOWLIST entry with a reason." >&2
        echo "If the line only READS SQL, put it in a read position the classifier" >&2
        echo "knows: a grep/awk/sed/echo/printf invocation, or a *_PATTERN variable." >&2
        exit 1
    fi

    echo "api-path-bypass-smell: clean — no direct-DB data-write end-arounds."
}

case "${1:-}" in
    --self-test)
        self_test ;;
    --strict)
        STRICT=1
        self_test
        main_scan ;;
    '')
        self_test
        main_scan ;;
    *)
        echo "usage: infra/lint/api-path-bypass-smell.sh [--strict|--self-test]" >&2
        exit 2 ;;
esac
