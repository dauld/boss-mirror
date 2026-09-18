#!/usr/bin/env bash
#
# sweep-archive-branches — delete the forge branches whose cars landed
# BEFORE the database switch, on TWO sources of evidence — the ARCHIVE
# database's job records, by exactly the rule the arriving train's
# sweep applies, and the forge's own PULL-REQUEST ANCESTRY — and record
# what went, what was kept, and why.
#
# WHY IT EXISTS (backlog dfd83788, measured 2026-09-17 06:30Z on the pod)
# ---------------------------------------------------------------------
# `boss orient` listed 50 forge heads no packet claims. The arrival
# sweep deletes a landed car's branch on the JOB RECORD's evidence
# (crates/orchestrators/boss-cli/src/train.rs: `deletable_branches`
# proves the content landed — a closed car with outcome `merged` —
# and `sweep_guard` proves the branch still holds only that content —
# its head on the forge equals the head recorded at boarding). Every
# car that landed before the 2026-09-16 23:55Z switch
# (switch-instance-database.sh) lives in the ARCHIVE database, not in
# the system of record the sweep reads, so the sweep will never see
# them and those branches are permanent by construction. `boss merged`
# over all 50 could judge only 9 by content (train PRs squash-merge, so
# ancestry never proves a landing); the evidence that decides them is
# the archive's job records, readable through the kubectl-exec psql
# door tenant-census.sh already uses. So: one bounded verb, the
# arrival sweep's own rule, applied to the archive's records.
#
# THE RULE, which is the sweep's and is not loosened here. A branch
# comes off iff
#   - the archive holds a `ship-a-change` job with status `closed` and
#     metadata.outcome `merged` that NAMES it: metadata.branch with
#     metadata.boarded_head, or an entry of metadata.rerail_origins
#     with its branch and head (train.rs `recorded_branches` — the
#     rerail original is the second half, packet 473fda1b);
#   - AND the branch's current head on the forge EQUALS the recorded
#     head (train.rs `sweep_guard`: car 23923b40 is what the head guard
#     costs when it is missing — two commits pushed after boarding,
#     deleted on a job record that was entirely correct).
# Everything else is KEPT and NAMED: a head that differs is `moved`
# (the commits live nowhere else); a claim with no recorded head is
# `no head` (an unknown head is not evidence); a forge head no
# archive car claims is `unrecorded` — an abandoned attempt, or a
# branch never filed as a car: a human's decision, listed for one. A
# claim whose branch is not on the forge is `gone`: COUNTED, never
# named — a gone branch is nothing to act on and its name carries
# nothing (train.rs `sweep_note`, job 1bd1fb3d).
#
# THE SECOND SOURCE (backlog 2c10a25d, measured 2026-09-17 08:00Z)
# ---------------------------------------------------------------------
# The first dry run (ops-request 54547b33) planned 6 of the 50 orphans:
# the archive holds a closed, merged car for only those 6, because the
# cars of trains #384–#411 closed (or tried to) inside the converge
# hold and after the database switch, so NO database recorded their
# landing. The forge did. Forgejo keeps `refs/pull/N/head` for every
# PR; a train PR N is the assembled consist, squash-merged as one main
# commit whose subject ends `(#N)`; so a branch head that is an
# ANCESTOR of `refs/pull/N/head` for a MERGED N landed with that train
# (37 of the 50, by `git merge-base --is-ancestor` on the pod). This
# evidence survives every cutover and needs no database, so it rides
# beside the archive's: a forge head the archive did not plan is
# planned with `evidence: "pull-request", pr: N` when a merged PR's
# head contains it — the NEWEST such N. A head that is also in an OPEN
# PR is still deletable when a merged one contains it (merged wins); a
# head ONLY in unmerged PRs is not evidence. The archive's evidence is
# untouched: a branch it plans is `evidence: "archive"`.
#
# THE SAME GUARD applies to both. The archive's rule compares the
# recorded head to the forge's; for PR evidence the recorded head IS
# the forge's head that proved ancestry, so the guard reduces to "still
# points there at delete time" — and that is now what the forge is
# asked, for every delete: `--force-with-lease=refs/heads/<b>:<head>`
# makes the delete conditional on the ref still being at the head the
# evidence vouches for. A branch that moves between the read and the
# push is refused by the forge (`stale info`), recorded `failed`, and
# read back still there.
#
# TWO DEFECTS THE SAME RUN SHOWED, fixed here. (1) The record named
# all 1,014 GONE branches — 184 KB — and the runner kept 102,400 bytes
# (OPS_OUTPUT_CAP), so the unrecorded list was cut off the packet.
# Gone is a COUNT now: `gone=<n>` on the verdict and `gone` in the
# record, no list; a test holds a record with 1,000 gone claims and 50
# heads under 100,000 bytes. (2) The `unrecorded` set held the LIVE
# system of record's own open cars' branches, because the verb read
# only the archive. It now reads the live open `ship-a-change` cars
# through `boss-sor-read` (infra/forge/probe-bin — the reader the
# recorded probes use, on this host by construction) as a read-scoped
# actor, and a forge head an open car names is `live`: counted and
# named, never unrecorded, never planned — even where PR ancestry
# would call it landed, because a follow-up car branched from a landed
# head is live work. A live record that cannot be read is a REFUSAL:
# unreadable live = cannot tell live from stale, and a wrong target
# answers instead of erroring (CLAUDE.md §Doors).
#
# THE BOUNDS, in the order they are applied — each refuses loudly and
# names the bound; a refusal changes nothing:
#
#   1. THE ARGUMENTS. `--dry-run` or `--for-real`, a namespace of the
#      shape instances.toml admits (`boss` or `boss-<name>`; never
#      boss-dev, the pipeline's), and a database name that is a plain
#      lowercase identifier — so it is never quoted, escaped or split.
#      The allowlist checks these first; the script re-checks rather
#      than relying on one layer (the retire-second-stack convention).
#   2. THE ARCHIVE IS NOT THE LIVE DATABASE. Secret `boss-secrets` key
#      `database-url` in the namespace is parsed in a variable (the
#      switch verb's parsing, copied); every line printed names
#      user@host:port/db — THE PASSWORD IS NEVER PRINTED, on any path,
#      and a test asserts it. The database that Secret names is the
#      live system of record, and the LIVE sweep owns it: asking to
#      sweep from it is refused. A Secret that cannot be read or
#      parsed is a refusal, never a pass; a host that is not the
#      instance's postgres Service is refused, because the read below
#      goes through sts/postgres in the namespace.
#   3. THE RECORD. ONE psql round trip (json_agg) through the same
#      kubectl-exec door the census uses, the session set read-only in
#      its own -c before the query — the archive is never written.
#      An archive that cannot be read is a refusal.
#   4. THE FORGE. The heads are read with `git ls-remote --heads
#      forgejo` in the converged checkout — the remote the converge
#      itself fetches through (cluster-deploy-lib.sh, tenant_source_
#      check). That remote's URL may carry a token in its userinfo, so
#      THE URL IS NEVER PRINTED and every git message is redacted
#      before it reaches the packet (the lib's redact_url shape). A
#      forge that cannot be read is a refusal. The checkout is a
#      user's and the ops-runner is root, so git runs as the
#      directory's owner (delete-orphan-object.sh's as_owner).
#   5. THE LIVE RECORD. `boss-sor-read` reads the open ship-a-change
#      cars (one listing, limit 200) as a read-scoped `audit-readonly`
#      actor — the identity run-car-probe.sh builds for a probe, for
#      the same reason: an unidentified reader is answered a NARROWER
#      WORLD in silence (backlog 61085a9e). The reader missing,
#      refusing, answering the wrong shape, or answering a listing cut
#      at its limit (`total` above what came back) is a refusal.
#   6. THE PULL REQUESTS. One fetch through the same remote, under the
#      checkout's lock (checkout-lock.sh — one lock for every git user
#      of the forge checkout): every `refs/heads/*` and every
#      `refs/pull/*/head` into this verb's own namespace
#      `refs/sweep-archive-branches/`, never into refs/remotes or the
#      worktree. Measured on the pod, 417 PRs: the one fetch is 0.3 s,
#      incremental after; ls-remote plus a fetch per sha is 417 round
#      trips. The merged set is the `(#N)` subjects on the FETCHED
#      main. Ancestry is one `git for-each-ref --contains <head>` over
#      the pull namespace per forge head (24 ms each). A fetch that
#      fails is a refusal.
#   7. THE NAME. A branch name is data the forge handed back; only a
#      name of the shape `[A-Za-z0-9][A-Za-z0-9._/-]*` is ever handed
#      to git, and always as the full ref `refs/heads/<name>` so it can
#      never read as an option. A deletable branch whose name is
#      outside that shape is kept and named `unsafe name`.
#
# Then, `--for-real` only: `git push forgejo --delete
# --force-with-lease=refs/heads/<b>:<head> refs/heads/<b>` for each
# planned branch, each one READ BACK with ls-remote — a forge answer
# is not a forge effect — and recorded as `deleted` (absent on
# read-back) or `failed` (the push refused, or the ref still there).
# The dry run prints the same plan and pushes nothing.
#
# OUTPUT ORDER IS LOAD-BEARING (backlog 5323f3ef: the ops-runner keeps
# the first 100 KB of a verb's output, OPS_OUTPUT_CAP). The VERDICT
# LINE PRINTS FIRST — `sweep-archive-branches: <mode> archive=<db>
# recorded=<n> planned=<n> by_archive=<n> by_pr=<n> deleted=<n>
# moved=<n> unrecorded=<n> live=<n> gone=<n>` — then the ONE JSON
# record line on stdout, then the per-branch lines, so a cut listing
# never costs the verdict. In a dry run `deleted` is 0 by construction
# and `planned` is the count the real run would delete.
#
# USAGE
#   sweep-archive-branches.sh --dry-run | --for-real <namespace> <archive-db>
#
# EXIT
#   0  done (or, with --dry-run, the plan)
#   2  refused — the reason names the bound; nothing changed
#   1  a real run in which a deletion failed or could not be read
#      back — the record states each branch's outcome
#
# ENV (test seams — the ops-runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   BOSS_SWEEP_TREE            the checkout whose `forgejo` remote is
#                              the forge (default: the one this script
#                              is in)
#   BOSS_SWEEP_SOR_READ        the live-record reader (default: this
#                              checkout's infra/forge/probe-bin/
#                              boss-sor-read, which needs BOSS_JOBS_URL
#                              — the ops-runner's unit pins it)
#   BOSS_KUBECTL / KUBECONFIG  see undeclared-objects.sh; resolved once

set -uo pipefail

ME="sweep-archive-branches"
say() { echo "$ME: $*" >&2; }
# What the reads found (the live database, the archive's count, the
# forge's count) is HELD until the verdict has printed, so the verdict
# is the packet's first line; a refusal prints the notes first, because
# there the notes are the diagnosis.
NOTES=()
note() { NOTES+=("$*"); }
flush_notes() { local n; for n in "${NOTES[@]+"${NOTES[@]}"}"; do say "$n"; done; NOTES=(); }
refuse() { flush_notes; say "REFUSED — $*"; say "  Nothing was changed."; exit 2; }

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
RESOLVE="$REPO/infra/cluster/undeclared-objects.sh"
LOCK_LIB="$REPO/infra/forge/checkout-lock.sh"
TREE="${BOSS_SWEEP_TREE:-$REPO}"
REMOTE="forgejo"
# This verb's own ref namespace in the checkout: the forge's heads and
# pull heads land here, never in refs/remotes (the converge's) or the
# worktree, so the fetch disturbs nothing the converge reads.
NS_REF="refs/sweep-archive-branches"
# The live record's reader and the one listing it is asked for.
SOR_READ="${BOSS_SWEEP_SOR_READ:-$REPO/infra/forge/probe-bin/boss-sor-read}"
LIVE_LIMIT=200

# The target, as infra/cluster/manifests/boss.yaml declares it: the
# StatefulSet `postgres`, container `postgres`, POSTGRES_USER=boss; the
# namespace is the packet's. The cars are `ship-a-change` packets
# (infra/platform/workflows/ship-a-change.toml), the kind the sweep reads.
. "$(dirname "$0")/forge-defaults.sh"
SECRET_NAME="boss-secrets"
SECRET_KEY="database-url"
CAR_KIND="ship-a-change"

# --- bound 1: the arguments -------------------------------------------------
usage() {
    say "usage: $ME --dry-run | --for-real <namespace> <archive-db>"
    say "  --dry-run   read the archive and the forge, print the plan; pushes nothing"
    say "  --for-real  delete the planned branches on the forge, read each back"
    exit 2
}
[ "$#" -eq 3 ] || usage
DRY=""
case "$1" in
    --dry-run) DRY=1 ;;
    --for-real) DRY=0 ;;
    *) say "the only modes are --dry-run and --for-real, not \`$1\`"; usage ;;
esac
MODE="$1"
NS="$2"
ARCHIVE="$3"
if ! [[ "$NS" =~ ^boss(-[a-z0-9]+)*$ ]]; then
    refuse "\`$NS\` is not an instance namespace (boss, or boss-<name>)"
fi
[ "$NS" != "boss-dev" ] || refuse "boss-dev is the pipeline's namespace, not an instance's"
if ! [[ "$ARCHIVE" =~ ^[a-z][a-z0-9_]{0,62}$ ]]; then
    refuse "\`$ARCHIVE\` is not a database name this verb will read: lowercase letters, digits and _ only, leading letter, 63 at most"
fi

command -v jq >/dev/null 2>&1 || { say "jq is not on PATH, so nothing can be read. Nothing was changed."; exit 1; }
command -v git >/dev/null 2>&1 || { say "git is not on PATH, so the forge cannot be read. Nothing was changed."; exit 1; }

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

# --- the kubectl, resolved once, by the derivation the census uses ---------
KUBECTL_LINE=$("$RESOLVE" --kubectl) || {
    say "no kubectl to read with (see above). Nothing was changed."
    exit 1
}
read -r -a KUBECTL <<<"$KUBECTL_LINE"
k() { "${KUBECTL[@]}" -n "$NS" "$@"; }

# --- bound 2: the archive is not the live database -------------------------
# The Secret, parsed into variables and never printed whole. `redact`
# masks the credential in any URL-shaped text (the converge lib's
# redact_url shape, so a forge remote's token is covered too); `scrub`
# removes the literal password from anything a tool said back.
redact() { sed -E 's#://[^/@[:space:]]+@#://<redacted>@#g'; }
PG_PASS=""
scrub() { # stdin -> stdout, the literal password replaced, URLs redacted
    local line
    while IFS= read -r line || [ -n "$line" ]; do
        if [ -n "$PG_PASS" ]; then line="${line//"$PG_PASS"/***}"; fi
        printf '%s\n' "$line" | redact
    done
}
read_secret_url() { # -> stdout: the decoded URL; stderr: kubectl's words
    k get secret "$SECRET_NAME" -o "jsonpath={.data.$SECRET_KEY}" | base64 -d
}
if ! URL=$(read_secret_url 2> "$TMP/secret.err") || [ -z "$URL" ]; then
    flush_notes
    say "REFUSED — cannot read Secret $SECRET_NAME key $SECRET_KEY in $NS:"
    scrub < "$TMP/secret.err" | sed 's/^/    /' >&2
    say "  The live database is the bound this verb must not cross, and it cannot be read. Nothing was changed."
    exit 2
fi
# postgres://user:pass@host[:port]/db[?query]
REST="${URL#*://}"
AUTHORITY="${REST%%/*}"
PATHQ="${REST#"$AUTHORITY"}"
PATHQ="${PATHQ#/}"
USERINFO="${AUTHORITY%@*}"
HOSTPORT="${AUTHORITY##*@}"
PG_USER_IN_URL="${USERINFO%%:*}"
PG_PASS="${USERINFO#*:}"
PG_HOST="${HOSTPORT%%:*}"
PG_PORT="${HOSTPORT#*:}"
[ "$PG_PORT" != "$HOSTPORT" ] || PG_PORT=5432
LIVE="${PATHQ%%\?*}"
if [ "$REST" = "$URL" ] || [ "$AUTHORITY" = "$HOSTPORT" ] || [ -z "$PG_USER_IN_URL" ] || [ -z "$PG_HOST" ] || [ -z "$LIVE" ] || [ "$USERINFO" = "$PG_PASS" ]; then
    refuse "Secret $SECRET_NAME key $SECRET_KEY in $NS is not of the shape postgres://user:pass@host[:port]/db (read as $(printf '%s' "$URL" | redact)), so the live database cannot be named"
fi
SHOWN_LIVE="$PG_USER_IN_URL@$PG_HOST:$PG_PORT/$LIVE"
note "live: $SECRET_NAME/$SECRET_KEY in $NS names $SHOWN_LIVE"
case "$PG_HOST" in
    postgres|postgres.*) ;;
    *) refuse "the Secret's host is $PG_HOST, not this instance's postgres Service — the archive is read through $PG_WORKLOAD in $NS, which is not where that URL points" ;;
esac
[ "$ARCHIVE" != "$LIVE" ] || refuse "$ARCHIVE is the database the Secret names ($SHOWN_LIVE) — the live system of record, whose branches the arriving train's own sweep owns. This verb reads an ARCHIVE"

# --- bound 3: the record, one read -----------------------------------------
# The rows the sweep's rule needs and nothing else: every closed,
# merged ship-a-change car's id, branch, boarded head and rerail
# origins, oldest first (the order `decided_branches` lets a first
# claim win in). Read-only in its own -c, the census's way; `-At`
# prints the one json value bare. The database name is bound 1's plain
# identifier and the kind is a literal, so the SQL carries no quoting.
SQL="-- $ME:merged_cars
SELECT coalesce(json_agg(json_build_object(
    'id', id,
    'branch', metadata->>'branch',
    'boarded_head', metadata->>'boarded_head',
    'rerail_origins', coalesce(metadata->'rerail_origins', '[]'::jsonb)
) ORDER BY created_at, id), '[]'::json)
FROM jobs
WHERE kind = '$CAR_KIND' AND status = 'closed' AND metadata->>'outcome' = 'merged'"
if ! k exec "$PG_WORKLOAD" -c "$PG_CONTAINER" -- \
        psql -X -q -At -v ON_ERROR_STOP=1 -U "$PG_USER" -d "$ARCHIVE" \
        -c 'SET default_transaction_read_only = on' \
        -c "$SQL" > "$TMP/cars.out" 2> "$TMP/psql.err"; then
    flush_notes
    say "REFUSED — cannot read the archive $ARCHIVE through $PG_WORKLOAD in $NS; psql said:"
    scrub < "$TMP/psql.err" | sed 's/^/    /' >&2
    say "  Nothing was changed."
    exit 2
fi
if ! jq -c 'if type == "array" then . else error("not an array") end' < "$TMP/cars.out" > "$TMP/cars.json" 2> "$TMP/jq.err"; then
    flush_notes
    say "REFUSED — the archive $ARCHIVE answered, but not with one JSON array:"
    sed 's/^/    /' "$TMP/jq.err" >&2
    head -c 400 "$TMP/cars.out" | scrub | sed 's/^/    /' >&2
    say "  Nothing was changed."
    exit 2
fi
CARS_READ=$(jq 'length' "$TMP/cars.json")
note "archive: $ARCHIVE holds $CARS_READ closed, merged $CAR_KIND car(s)"

# --- bound 4: the forge ----------------------------------------------------
case "$TREE" in
    *[[:space:]\'\"]*) refuse "the tree path \`$TREE\` contains whitespace or a quote; name one without" ;;
esac
[ -d "$TREE" ] || refuse "the tree $TREE does not exist, so the forge cannot be read"
OWNER="$(stat -c %U "$TREE" 2>/dev/null)"
if [ -z "$OWNER" ] || [ "$OWNER" = "UNKNOWN" ] || ! id -u "$OWNER" >/dev/null 2>&1; then
    refuse "cannot resolve the owner of $TREE (stat says '${OWNER:-}'), so there is no account to read its git as"
fi
OWNER_HOME="$(getent passwd "$OWNER" | cut -d: -f6)"
as_owner() { # <command string>
    if [ "$(id -un)" = "$OWNER" ]; then
        bash -c "$1"
    else
        runuser -u "$OWNER" -- env HOME="${OWNER_HOME:-/}" PATH="$PATH" bash -c "$1"
    fi
}
# GIT_TERMINAL_PROMPT=0: a unit has no terminal, and a prompt would
# hang, not fail. The remote's URL is never asked for and never shown.
if ! as_owner "GIT_TERMINAL_PROMPT=0 git -C '$TREE' ls-remote --heads '$REMOTE'" > "$TMP/heads.tsv" 2> "$TMP/git.err"; then
    flush_notes
    say "REFUSED — cannot read the forge's heads through remote $REMOTE of $TREE (as $OWNER); git said:"
    scrub < "$TMP/git.err" | sed 's/^/    /' >&2
    say "  Nothing was changed."
    exit 2
fi
HEADS_JSON=$(jq -R -n '[inputs | select(length > 0) | split("\t") | select(length == 2 and (.[1] | startswith("refs/heads/"))) | {key: (.[1] | sub("^refs/heads/"; "")), value: .[0]}] | from_entries' < "$TMP/heads.tsv")
note "forge: $(printf '%s' "$HEADS_JSON" | jq 'length') head(s) on remote $REMOTE"

# --- bound 5: the live record ----------------------------------------------
# The open cars in the system of record, read as a NAMED, READ-SCOPED
# actor through the same reader a recorded probe uses (run-car-probe.sh
# builds this identity for the same reason: role `audit-readonly` is
# Read at Scope::All and nothing else, and an unidentified reader is
# answered a narrower world in silence — backlog 61085a9e). The reader
# takes its base from BOSS_JOBS_URL, which the ops-runner's unit pins;
# it refuses without one, and that refusal is this verb's.
LIVE_PATH="/api/jobs?kind=$CAR_KIND&status=open&limit=$LIVE_LIMIT"
READER_USER='{"id":"automation:sweep-archive-branches-reader","role":"audit-readonly","access_tier":"auditor","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'
[ -x "$SOR_READ" ] || refuse "the live record's reader $SOR_READ is not here, so an open car's branch cannot be told from a stale one (the live record is read through boss-sor-read, which the converged checkout carries at infra/forge/probe-bin)"
if ! BOSS_SOR_USER="$READER_USER" "$SOR_READ" "$LIVE_PATH" > "$TMP/live.out" 2> "$TMP/live.err"; then
    flush_notes
    say "REFUSED — cannot read the live open $CAR_KIND cars ($LIVE_PATH); the reader said:"
    scrub < "$TMP/live.err" | sed 's/^/    /' >&2
    say "  An unreadable live record cannot tell a live car's branch from a stale one. Nothing was changed."
    exit 2
fi
if ! LIVE_JSON=$(jq -c --argjson limit "$LIVE_LIMIT" --arg kind "$CAR_KIND" '
        if type != "object" or (.data | type) != "array" then error("not a listing: no data array") else . end
        | (.total // (.data | length)) as $total
        | if $total > (.data | length) then error("the listing is cut: total \($total), \(.data | length) returned, limit \($limit) — a cut listing cannot name every live car") else . end
        | [ .data[] | select(type == "object") | {car: (.id // ""), branch: (.metadata.branch // "")}
            | select(.car != "" and .branch != "") ]' < "$TMP/live.out" 2> "$TMP/live-jq.err"); then
    flush_notes
    say "REFUSED — the live record answered $LIVE_PATH, but not with a listing this verb can read:"
    sed 's/^/    /' "$TMP/live-jq.err" >&2
    head -c 400 "$TMP/live.out" | scrub | sed 's/^/    /' >&2
    say "  An unreadable live record cannot tell a live car's branch from a stale one. Nothing was changed."
    exit 2
fi
note "live: the system of record holds $(printf '%s' "$LIVE_JSON" | jq 'length') open $CAR_KIND car(s) naming a branch"

# --- bound 6: the pull requests --------------------------------------------
# One fetch, under the checkout's lock, into this verb's own namespace:
# every head (so every forge head's objects are here for the ancestry
# test) and every pull head. `--refmap=` (empty) is load-bearing: a
# fetch with explicit refspecs ALSO opportunistically updates the
# remote's configured tracking refs, and refs/remotes/forgejo/main is
# the converge's (measured in the fixture: it moved). `--prune` keeps
# the namespace the forge's mirror; `--no-tags` leaves the checkout's
# tags alone. Output is captured and printed only on failure (a quiet
# log is not free).
if ! as_owner ". '$LOCK_LIB' && GIT_TERMINAL_PROMPT=0 checkout_git '$TREE' fetch --refmap= --no-tags --prune '$REMOTE' '+refs/heads/*:$NS_REF/heads/*' '+refs/pull/*/head:$NS_REF/pull/*'" > "$TMP/fetch.out" 2>&1; then
    flush_notes
    say "REFUSED — cannot fetch the forge's pull heads through remote $REMOTE of $TREE (as $OWNER); git said:"
    scrub < "$TMP/fetch.out" | sed 's/^/    /' >&2
    say "  Nothing was changed."
    exit 2
fi
# The merged set: every `(#N)` that closes a subject on the FETCHED
# main — the conductor's title for a train's squash commit — as one
# JSON array of numbers, deduplicated.
if ! as_owner "git -C '$TREE' log --format=%s '$NS_REF/heads/main'" > "$TMP/subjects.txt" 2> "$TMP/log.err"; then
    flush_notes
    say "REFUSED — cannot read main's history from the fetched $NS_REF/heads/main; git said:"
    scrub < "$TMP/log.err" | sed 's/^/    /' >&2
    say "  Nothing was changed."
    exit 2
fi
MERGED_JSON=$(sed -nE 's/.*\(#([0-9]+)\)$/\1/p' "$TMP/subjects.txt" | sort -un | jq -R -n '[inputs | tonumber]')
# Ancestry: for every forge head but main, the pull heads that contain
# it, as `<branch>\t<sha>\t<n> <n> …`. A sha the checkout does not hold
# (the branch moved between the ls-remote and the fetch) yields no
# refs — a missing object is not evidence — and is counted below.
: > "$TMP/ancestry.tsv"
UNVERIFIABLE=0
while IFS=$'\t' read -r branch sha; do
    [ -n "$branch" ] || continue
    [[ "$sha" =~ ^[0-9a-f]{40}$ ]] || continue
    if refs=$(as_owner "git -C '$TREE' for-each-ref --contains '$sha' --format='%(refname)' '$NS_REF/pull/'" 2>/dev/null); then
        prs=$(printf '%s\n' "$refs" | sed -n "s#^$NS_REF/pull/##p" | tr '\n' ' ')
    else
        prs=""; UNVERIFIABLE=$((UNVERIFIABLE + 1))
    fi
    printf '%s\t%s\t%s\n' "$branch" "$sha" "$prs" >> "$TMP/ancestry.tsv"
done < <(printf '%s' "$HEADS_JSON" | jq -r 'to_entries[] | select(.key != "main") | [.key, .value] | @tsv')
ANCESTRY_JSON=$(jq -R -n '[inputs | select(length > 0) | split("\t") | {branch: .[0], head: .[1], prs: ((.[2] // "") | split(" ") | map(select(length > 0) | tonumber))}]' < "$TMP/ancestry.tsv")
PULL_HEADS=$(as_owner "git -C '$TREE' for-each-ref '$NS_REF/pull/'" 2>/dev/null | wc -l | tr -d ' ')
note "pull requests: $PULL_HEADS pull head(s) fetched, $(printf '%s' "$MERGED_JSON" | jq 'length') merged on main$( [ "$UNVERIFIABLE" = 0 ] || echo "; $UNVERIFIABLE forge head(s) not in the checkout after the fetch, not tested")"

# --- the decision, pure -----------------------------------------------------
# train.rs `recorded_branches`, then `decided_branches`, then
# `sweep_guard`, in jq: per car, its own branch first (with its boarded
# head) then each rerail origin oldest first (with its head); `main`,
# unnamed and duplicate branches drop out; across cars the FIRST claim
# wins; then the head guard against the forge's answer. Then the two
# sources this car added, in this order: a forge head an OPEN LIVE
# CAR names is `live` before anything else (a follow-up car branched
# from a landed head is live work); a head the archive did not plan
# whose current sha a MERGED pull request's head contains is planned
# on that evidence, the newest such PR named — so a claim the archive
# saw as `moved` or `no head` lands on PR evidence when the forge's
# head is in a merged consist, and an unclaimed head does the same.
# Bound 7 — the name shape — is judged here too, so an unsafe name
# never reaches a git argv. `unclaimed` is every forge head no claim,
# no live car and no merged PR names, `main` excepted.
jq -c --argjson heads "$HEADS_JSON" --argjson live "$LIVE_JSON" \
      --argjson ancestry "$ANCESTRY_JSON" --argjson merged "$MERGED_JSON" '
    def nonempty: if . == null or . == "" then null else . end;
    def safe_name: test("^[A-Za-z0-9][A-Za-z0-9._/-]*$") and (contains("..") | not);
    (reduce $live[] as $l ({}; if has($l.branch) then . else .[$l.branch] = $l.car end)) as $live_car
    | (reduce $ancestry[] as $a ({};
          .[$a.branch] = ([ $a.prs[] | select(. as $n | $merged | index($n) != null) ] | max))) as $landed_pr
    | def judge:
        if .current == null then .verdict = "gone"
        elif $live_car[.branch] != null then .verdict = "live" | .live_car = $live_car[.branch]
        elif .head != null and .head == .current then
            .evidence = "archive" | .verdict = (if (.branch | safe_name) then "delete" else "unsafe" end)
        elif $landed_pr[.branch] != null then
            .evidence = "pull-request" | .pr = $landed_pr[.branch] | .head = .current
            | .verdict = (if (.branch | safe_name) then "delete" else "unsafe" end)
        elif .head == null then .verdict = "no_head"
        else .verdict = "moved" end;
    ([ .[] | . as $c
        | ([{branch: (.branch // ""), head: (.boarded_head | nonempty), rerail_origin: false}]
           + [ (.rerail_origins // [])[] | select(type == "object")
               | {branch: (.branch // ""), head: (.head | nonempty), rerail_origin: true} ])
        | reduce .[] as $b ([]; if $b.branch == "" or $b.branch == "main" or (map(.branch) | index($b.branch)) != null then . else . + [$b] end)
        | .[] | . + {car: ($c.id // "")} | select(.car != "")
     ]
     | reduce .[] as $b ([]; if (map(.branch) | index($b.branch)) != null then . else . + [$b] end)
     | map(. + {current: ($heads[.branch] // null), recorded: (.head)} | judge)
    ) as $claims
    | { claims: $claims,
        unclaimed: ([ $heads | to_entries[] | select(.key != "main") | select(.key as $b | $claims | map(.branch) | index($b) | not)
                      | {branch: .key, current: .value, head: null, car: null, rerail_origin: false} | judge
                      | if .verdict == "no_head" then .verdict = "unclaimed" else . end ] | sort_by(.branch)) }
' "$TMP/cars.json" > "$TMP/plan.json"

# --- the deletes, --for-real only --------------------------------------------
# Each planned branch: push the delete as the full ref, LEASED to the
# head the evidence vouches for (the forge refuses if the branch moved
# since the read), then READ IT BACK — absent is `deleted`, anything
# else is `failed` with the reason. Outcomes land in a TSV the record
# is built from; nothing is printed until the verdict line can carry
# the counts.
#
# Rows are joined on the UNIT SEPARATOR, not a tab: a PR-planned entry
# has no car and an archive one no PR, and `read` with a tab IFS folds
# consecutive tabs, so an empty field would shift every column after
# it (measured on the first run of the PR test: the car column read the
# rerail flag). US is not IFS whitespace, so an empty field stays one.
US=$'\x1f'
row() { local IFS="$US"; printf '%s\n' "$*"; }
# The ONE line of a git failure worth recording: the rejection or the
# error, not `To <remote>` — which is git's first line on a refused
# push and names nothing (measured in the lease test: the recorded
# reason was `push refused: To /…/forge.git`). Scrubbed first, so a
# URL's userinfo never rides it.
said_why() { # stdin -> the reason line (one awk: no early-exit grep under pipefail)
    scrub | awk 'NF { if (!first) first = $0
                      if (!why && ($0 ~ /^[[:space:]]*!/ || $0 ~ /rejected/ || $0 ~ /^(error|fatal|remote):/)) why = $0 }
                 END { if (why) print why; else if (first) print first }'
}
: > "$TMP/done.tsv"
if [ "$DRY" = 0 ]; then
    while IFS="$US" read -r branch car head evidence pr; do
        [ -n "$branch" ] || continue
        if ! as_owner "GIT_TERMINAL_PROMPT=0 git -C '$TREE' push '$REMOTE' --delete '--force-with-lease=refs/heads/$branch:$head' 'refs/heads/$branch'" > "$TMP/push.out" 2> "$TMP/push.err"; then
            reason=$(said_why < "$TMP/push.err")
            row "$branch" "$car" "$head" failed "push refused: ${reason:-no message}" "$evidence" "$pr" >> "$TMP/done.tsv"
            continue
        fi
        if ! as_owner "GIT_TERMINAL_PROMPT=0 git -C '$TREE' ls-remote --heads '$REMOTE' 'refs/heads/$branch'" > "$TMP/back.out" 2> "$TMP/back.err"; then
            reason=$(said_why < "$TMP/back.err")
            row "$branch" "$car" "$head" failed "push answered but the read-back failed: ${reason:-no message}" "$evidence" "$pr" >> "$TMP/done.tsv"
            continue
        fi
        if [ -s "$TMP/back.out" ]; then
            row "$branch" "$car" "$head" failed "push answered but the ref is still on the forge at $(awk -F'\t' 'NR == 1 { print substr($1, 1, 40) }' "$TMP/back.out")" "$evidence" "$pr" >> "$TMP/done.tsv"
        else
            row "$branch" "$car" "$head" deleted absent "$evidence" "$pr" >> "$TMP/done.tsv"
        fi
    done < <(jq -r '(.claims[], .unclaimed[]) | select(.verdict == "delete") | [.branch, (.car // ""), .head, .evidence, (.pr // "")] | join("")' "$TMP/plan.json")
fi

# --- the record, assembled ---------------------------------------------------
RECORD=$(jq -c -n -R --rawfile done "$TMP/done.tsv" --slurpfile plan "$TMP/plan.json" \
    --arg verb "$ME" --argjson dry "$( [ "$DRY" = 1 ] && echo true || echo false )" \
    --arg ns "$NS" --arg archive "$ARCHIVE" --arg live "$SHOWN_LIVE" --arg remote "$REMOTE" \
    --argjson cars "$CARS_READ" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
    ($plan[0]) as $p
    | ([ $done | split("\n")[] | select(length > 0) | split("\u001f")
         | {branch: .[0], car: (.[1] | if . == "" then null else . end), head: .[2], outcome: .[3], read_back: .[4],
            evidence: .[5], pr: (.[6] | if . == "" or . == null then null else tonumber end)} ]) as $d
    | ($p.claims + $p.unclaimed) as $all
    | ($all | map(select(.verdict == "delete"))) as $planned
    | def entry: {branch, evidence, pr, car, head, rerail_origin};
      { verb: $verb, dry_run: $dry, namespace: $ns, archive_db: $archive, live_db: $live, remote: $remote,
        cars_read: $cars,
        recorded: ($p.claims | length),
        planned: ($planned | length),
        by_archive: ($planned | map(select(.evidence == "archive")) | length),
        by_pr: ($planned | map(select(.evidence == "pull-request")) | length),
        deleted: ($d | map(select(.outcome == "deleted")) | length),
        failed: ($d | map(select(.outcome == "failed")) | length),
        moved: ($p.claims | map(select(.verdict == "moved")) | length),
        unrecorded: (($p.claims | map(select(.verdict == "no_head")) | length) + ($p.unclaimed | map(select(.verdict == "unclaimed")) | length)),
        live: ($all | map(select(.verdict == "live")) | length),
        gone: ($p.claims | map(select(.verdict == "gone")) | length),
        unsafe: ($all | map(select(.verdict == "unsafe")) | length),
        branches: {
          delete: [ $planned[] | entry ],
          deleted: [ $d[] | select(.outcome == "deleted") | {branch, evidence, pr, car, head, read_back} ],
          failed: [ $d[] | select(.outcome == "failed") | {branch, evidence, pr, car, head, reason: .read_back} ],
          live: [ $all[] | select(.verdict == "live") | {branch, car: .live_car, current} ],
          moved: [ $p.claims[] | select(.verdict == "moved") | {branch, car, recorded, current, rerail_origin} ],
          no_head: [ $p.claims[] | select(.verdict == "no_head") | {branch, car, current, rerail_origin} ],
          unclaimed: [ $p.unclaimed[] | select(.verdict == "unclaimed") | {branch, current} ],
          unsafe: [ $all[] | select(.verdict == "unsafe") | entry ] },
        at: $at }')
n() { printf '%s' "$RECORD" | jq -r ".$1"; }

# --- the verdict FIRST, then the record, then the lines ----------------------
VERDICT="$MODE archive=$ARCHIVE recorded=$(n recorded) planned=$(n planned) by_archive=$(n by_archive) by_pr=$(n by_pr) deleted=$(n deleted) moved=$(n moved) unrecorded=$(n unrecorded) live=$(n live) gone=$(n gone)"
[ "$(n unsafe)" = 0 ] || VERDICT="$VERDICT unsafe=$(n unsafe)"
[ "$(n failed)" = 0 ] || VERDICT="$VERDICT failed=$(n failed)"
say "$VERDICT"
printf '%s\n' "$RECORD"
flush_notes

id8() { printf '%s' "$1" | cut -c1-8; }
# What vouches for a planned entry, in words: the archive's car, or
# the merged pull request whose consist contains the head.
vouch() { # <evidence> <pr> <car> <rerail_origin|""> <head>
    if [ "$1" = pull-request ]; then
        printf 'landed with pull request #%s, at %s = the forge'"'"'s head' "$2" "$(id8 "$5")"
    else
        case "$4" in true) kind="rerail original of car" ;; false) kind="branch of car" ;; *) kind="car" ;; esac
        printf '%s %s, at %s = boarded' "$kind" "$(id8 "$3")" "$(id8 "$5")"
    fi
}
if [ "$DRY" = 1 ]; then
    printf '%s' "$RECORD" | jq -r '.branches.delete[] | "\(.branch)\u001f\(.evidence)\u001f\(.pr // "")\u001f\(.car // "")\u001f\(.rerail_origin)\u001f\(.head)"' | while IFS="$US" read -r b e p c o h; do
        say "would delete $b ($(vouch "$e" "$p" "$c" "$o" "$h"))"
    done
else
    printf '%s' "$RECORD" | jq -r '.branches.deleted[] | "\(.branch)\u001f\(.evidence)\u001f\(.pr // "")\u001f\(.car // "")\u001f\(.head)"' | while IFS="$US" read -r b e p c h; do
        say "deleted $b ($(vouch "$e" "$p" "$c" "" "$h"); read back: absent)"
    done
    printf '%s' "$RECORD" | jq -r '.branches.failed[] | "\(.branch)\u001f\(.evidence)\u001f\(.pr // "")\u001f\(.car // "")\u001f\(.head)\u001f\(.reason)"' | while IFS="$US" read -r b e p c h r; do
        say "FAILED $b ($(vouch "$e" "$p" "$c" "" "$h")) — $r"
    done
fi
printf '%s' "$RECORD" | jq -r '.branches.live[] | "\(.branch)\u001f\(.car)\u001f\(.current)"' | while IFS="$US" read -r b c cur; do
    say "live $b (open car $(id8 "$c") in the system of record, at $(id8 "$cur")) — kept: a live car's branch is never a candidate"
done
printf '%s' "$RECORD" | jq -r '.branches.moved[] | "\(.branch)\u001f\(.car)\u001f\(.recorded)\u001f\(.current)"' | while IFS="$US" read -r b c r cur; do
    say "moved $b (car $(id8 "$c") boarded $(id8 "$r"), the forge now holds $(id8 "$cur")) — kept: the commits after boarding live nowhere else, and no merged pull request contains them"
done
printf '%s' "$RECORD" | jq -r '.branches.no_head[] | "\(.branch)\u001f\(.car)\u001f\(.current)"' | while IFS="$US" read -r b c cur; do
    say "no head $b (car $(id8 "$c") landed, recorded no head; the forge holds $(id8 "$cur")) — kept: an unknown head is not evidence, and no merged pull request contains the head the forge holds"
done
printf '%s' "$RECORD" | jq -r '.branches.unsafe[] | "\(.branch)\u001f\(.evidence)\u001f\(.pr // "")\u001f\(.car // "")"' | while IFS="$US" read -r b e p c; do
    [ "$e" = pull-request ] && who="pull request #$p" || who="car $(id8 "$c")"
    say "unsafe name $b ($who) — kept: the name is outside the shape this verb hands to git"
done
printf '%s' "$RECORD" | jq -r '.branches.unclaimed[] | "\(.branch)\u001f\(.current)"' | while IFS="$US" read -r b cur; do
    say "unrecorded $b (at $(id8 "$cur")) — kept: no closed, merged $CAR_KIND car in $ARCHIVE names it, no merged pull request contains it, no open car rides it; an abandoned attempt or a branch never filed — a human's decision"
done

if [ "$DRY" = 1 ]; then
    say "DRY RUN — would delete $(n planned) branch(es) from remote $REMOTE ($(n by_archive) on the archive's record, $(n by_pr) on pull-request ancestry); $(n moved) moved, $(n unrecorded) unrecorded, $(n live) live, $(n gone) gone. Nothing was changed."
    exit 0
fi
if [ "$(n failed)" != 0 ]; then
    say "FAILED — deleted $(n deleted) of $(n planned) planned branch(es); $(n failed) could not be deleted or read back (named above). The rest stand as recorded."
    exit 1
fi
say "OK — deleted $(n deleted) of $(n planned) planned branch(es) from remote $REMOTE ($(n by_archive) on the archive's record, $(n by_pr) on pull-request ancestry), each read back absent; $(n moved) moved, $(n unrecorded) unrecorded and $(n live) live kept and named above."
exit 0
