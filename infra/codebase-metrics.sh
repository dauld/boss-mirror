#!/usr/bin/env bash
# codebase-metrics.sh — the codebase states its own trend, from the log
# it already has.
#
# David, 2026-09-11: "once we have code base analysis statistics that we
# can start to get a sense for whether we are getting simpler and more
# reliable or more complex as we go."
#
# NOTHING HAD TO START COLLECTING. Every landing on main is a
# first-parent commit carrying a diffstat, so the entire series is
# reconstructible retroactively — which is the only reason this cadence
# is cheap, and why its first run BACKFILLS the whole history instead of
# starting an empty series that becomes useful in a month. A projection
# of the log, in the Hickey sense: a pure function of commits, rebuilt
# rather than accumulated, so a bug in the arithmetic is fixed by
# re-running it and not by apologising for the data already gathered.
#
# WHAT IT REPORTS, and what it deliberately does not
# --------------------------------------------------
# Two ratios are the headline, because they are the two the founding
# ideas actually commit to:
#
#   1. DELETE:ADD per landing — `boss-codebase-shrinks`: "deletion is a
#      goal; registries should retire the code they replace". Measured
#      2026-09-11 over 2026-08-26..2026-09-11: 20%, and net growth on
#      every single day with no exceptions. Unmeasured before this, which
#      is why it could be 20% for a fortnight without anybody deciding
#      that was the number they wanted.
#
#   2. REGISTRY ROWS — CLAUDE.md §9: new behaviour lands as data in
#      append-only registries, not as new branches in core code. The
#      registry half is counted exactly (rule files, workflow rows, step
#      types, step plugins). The code-branch half — `match kind {
#      "refurb-used" => … }`, the named anti-pattern — is counted too, by
#      `boss-leaked-policy`, an AST pass rather than a regex. The first
#      cut of this script left it `null` with its reason in words,
#      because a regex over match arms cannot tell a leaked policy from
#      the dispatcher's own handler table and a number nobody can
#      interpret is worse than a blank. That reason still stands; what
#      changed is that something now understands Rust items. Measured
#      2026-09-11 over crates/core: 53 string-literal matches, of which
#      3 are leaked policy, 0 unclassified.
#
#      `code_branches_on_kind` is the integer;
#      `code_branches_by_class` is the whole ladder it came off;
#      `code_branches_sites` names the file and line of every leaked and
#      every undecided one, so the number is auditable without re-running
#      the pass; `code_branches_method` states the rule.
#      `code_branches_not_counted_why` survives for the ONE case it is
#      still true of: a machine with no counter on it. That is a fact
#      about the machine, and it is not the same fact as a zero.
#
# Everything else — totals by area, lint count, migration count, crates
# per tier, test:prod — is context. It rides in the row and is not the
# headline.
#
# THE PROD/TEST SPLIT IS THE HARD HALF, AND THE METHOD IS ON THE RECORD
# ---------------------------------------------------------------------
# A path bucket (`crates/**/tests/*.rs`, `*.test.ts`) is easy. Inline
# `#[cfg(test)] mod tests` inside a production `.rs` file is not, and in
# this repo it is most of the Rust tests: measured 2026-09-11, 72,612 of
# the 248,738 lines under `crates/**` outside a `tests/` directory are
# inside a `#[cfg(test)]` item — 29%. A path-only bucket reports 248,738
# lines of "production Rust", and that number is wrong by a margin that
# changes the conclusion.
#
# So the two halves are measured differently, on purpose:
#
#   * the SNAPSHOT (totals at one commit) attributes inline test code
#     EXACTLY, by matching the braces of the item each `#[cfg(test)]`
#     attribute governs. 338 of this tree's 346 attributes sit at column
#     zero; the scan handles indented ones too, and a `#[cfg(test)] use`
#     line with no block attributes only itself.
#   * the SERIES (per-landing diffs) attributes by PATH only. Deciding
#     whether a deleted line was inside a `#[cfg(test)]` block requires
#     reconstructing the block structure of the file as it was BEFORE
#     that commit, for every commit — and the answer would still be
#     approximate across a refactor that moves a test module.
#
# An honest approximation with its limits written down beats a
# precise-looking number that silently counts tests as production code,
# so the limits ride in `method` on the same row as the figures, and the
# snapshot's exact split is what tells a reader how large the series'
# bias is.
#
# EXIT STATUS — the vocabulary `infra/lint/lib/git-answer.sh` defines:
#   0  it read the repository and here is the measurement
#   1  it read the repository and something else failed (filing, usually)
#   3  it never read the repository — an infrastructure refusal, NOT a
#      day with no landings. A zero-landing series and a machine where
#      git refused are different facts and only one of them is about the
#      codebase, which is exactly the distinction four lints threw away
#      on 2026-09-11 (backlog 6b2f4a1a).
#
# USAGE
#   codebase-metrics.sh snapshot [--repo DIR] [--ref REF]
#   codebase-metrics.sh series   [--repo DIR] [--ref REF] [--since SHA]
#   codebase-metrics.sh row      [--repo DIR] [--ref REF] [--since SHA]
#   codebase-metrics.sh file     [--repo DIR] [--ref REF]
#
# `row` is snapshot + series in the shape the packet carries. `file`
# computes a row whose window starts at the last FILED measurement and
# PATCHes it onto the open `maintenance-codebase-metrics` packet — the
# daily cadence's whole body of work.

set -uo pipefail

# Every instant this script emits is UTC, including git's own date
# formatting (`--date=format-local` reads TZ). One timezone in the series
# or the trend is an artefact of whose machine ran it.
export TZ=UTC

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lint/lib/git-answer.sh
. "$SELF_DIR/lint/lib/git-answer.sh"
# shellcheck source=infra/lint/lib/trunk-ref.sh
. "$SELF_DIR/lint/lib/trunk-ref.sh"

NAME="codebase-metrics"
KIND="maintenance-codebase-metrics"

usage() { sed -n '/^# USAGE/,/^$/p' "$0" | sed 's/^# \{0,1\}//' >&2; }

CMD="${1:-}"
[ -n "$CMD" ] && shift
REPO="$SELF_DIR/.."
REF=""
SINCE=""
while [ $# -gt 0 ]; do
    case "$1" in
        --repo) REPO="${2:?--repo needs a directory}"; shift 2 ;;
        --ref) REF="${2:?--ref needs a ref}"; shift 2 ;;
        --since) SINCE="${2:?--since needs a sha}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "$NAME: unknown argument '$1'" >&2; usage; exit 2 ;;
    esac
done
case "$CMD" in
    snapshot|series|row|file) ;;
    *) echo "$NAME: expected one of snapshot|series|row|file" >&2; usage; exit 2 ;;
esac

for tool in git jq awk tar; do
    # `boss-leaked-policy` is deliberately NOT in this list. A missing
    # counter costs one field; a missing `git` costs the whole
    # measurement, and collapsing the two would turn "this box has no
    # Rust build" into an infrastructure refusal of the entire row.
    command -v "$tool" >/dev/null 2>&1 || {
        echo "$NAME: $LINT_CANNOT_ANSWER_MARKER — no $tool on PATH, so nothing was measured." >&2
        exit "$LINT_CANNOT_ANSWER"
    }
done

# A directory that is not there is a fact about the MACHINE, and it gets
# the same refusal a git failure gets — never an empty series.
if ! cd "$REPO" 2>/dev/null; then
    {
        printf '%s: %s — --repo %s is not a directory this process can enter,\n' \
            "$NAME" "$LINT_CANNOT_ANSWER_MARKER" "$REPO"
        printf '  so no commit was read. An INFRASTRUCTURE refusal (exit %s), not a\n' \
            "$LINT_CANNOT_ANSWER"
        printf '  measurement of a codebase with nothing in it.\n'
    } >&2
    exit "$LINT_CANNOT_ANSWER"
fi
git_can_answer "$NAME" || exit $?

# WHICH REF. `forge/main` then `origin/main` then `main`, the walk every
# baseline-comparing lint already uses — remote names are per-clone, and
# on the host this cadence runs on `origin` is the public GitHub mirror,
# which lags. A genuinely absent trunk falls back to HEAD and SAYS so in
# the output's `ref` field rather than guessing silently.
if [ -z "$REF" ]; then
    REF=$(resolve_trunk_ref "$NAME")
    case $? in
        0) ;;
        1)
            REF="HEAD"
            echo "$NAME: no trunk ref (tried $(trunk_candidates)) — measuring HEAD, which is what this checkout is at; the output says ref=HEAD" >&2
            ;;
        *) exit "$LINT_CANNOT_ANSWER" ;;
    esac
fi

HEAD_SHA=$(git_answer "$NAME" 0 rev-parse "$REF") || exit $?

# ---------------------------------------------------------------------
# THE CLASSIFIER — one definition, shared by the diff walk and the
# snapshot walk (CLAUDE.md §9a: a fact that lives twice drifts, and
# "which bucket is this file in" is the fact this whole measurement rests
# on). Prepended to both awk programs rather than written twice.
#
# ORDER IS THE DEFINITION. Test code is claimed first, in every
# language, because a test file under `apps/` is a test before it is web
# code. `crates/core/boss-testing/src` is deliberately left in
# `rust_prod`: it is a library other crates link, and moving a whole
# crate's src into "test" on the strength of its name is the kind of
# judgement a path rule should not be making silently.
# ---------------------------------------------------------------------
AWK_LIB='
# Generated or binary: counted by neither bucket. A lockfile churns by
# thousands of lines for a one-line dependency bump, and a .woff2 has no
# lines at all, so both would be noise in a trend about what humans
# wrote.
function skip(p) {
    if (p ~ /(^|\/)(Cargo\.lock|bun\.lock|package-lock\.json|bun\.lockb)$/) return 1
    if (p ~ /\.(woff2?|png|jpe?g|gif|ico|pdf|gz|tgz|zip|tar|bin|wasm)$/) return 1
    return 0
}
function bucket(p) {
    if (p ~ /\.rs$/) {
        if (p ~ /(^|\/)(tests|benches)\//) return "rust_test"
        return "rust_prod"
    }
    if (p ~ /\.(test|spec)\./ && p ~ /\.(ts|js|mjs)$/) return "web_test"
    if (p ~ /^apps\/[^\/]+\/tests\//) return "web_test"
    if (p ~ /^(apps|libs)\// && p ~ /\.(ts|js|mjs|svelte|css|html)$/) return "web_prod"
    if (p ~ /^infra\/postgres\/schema\/.*\.sql$/) return "schema"
    if (p ~ /^infra\/(dispatcher\/rules|platform\/workflows|step-plugins)\//) return "registry"
    # A crate that ships a `seeds/` directory is shipping registry rows —
    # step types, subject kinds, ML model descriptors, manual content.
    # They read as `other` on a naive bucket, which is the one place this
    # measurement would under-report the thing CLAUDE.md §9 is about.
    if (p ~ /^crates\/[^\/]+\/[^\/]+\/seeds\//) return "registry"
    if (p ~ /^infra\/lint\//) return "lint"
    if (p ~ /^(infra|\.forgejo|\.github|\.devcontainer|\.cargo)\//) return "infra"
    if (p ~ /^examples\//) return "seed"
    if (p ~ /\.md$/ || p ~ /^docs\//) return "docs"
    return "other"
}
# A test bucket, for the two-number split each landing row carries. The
# snapshots inline-test bucket counts here too; the series cannot
# produce it, which is the stated limit.
function is_test(b) { return (b == "rust_test" || b == "web_test" || b == "rust_test_inline") }
function jstr(s) {
    gsub(/\\/, "\\\\", s); gsub(/"/, "\\\"", s)
    gsub(/\t/, " ", s); gsub(/\r/, "", s)
    return "\"" s "\""
}
# Every bucket, always emitted, zeros included: a stable row shape is
# what makes the series queryable without the reader guessing which keys
# a given day happened to have.
BEGIN {
    split("rust_prod rust_test rust_test_inline web_prod web_test schema registry lint infra seed docs other", BUCKETS, " ")
}
'

# ---------------------------------------------------------------------
# THE SERIES — one git pass over the first-parent chain.
#
# `--first-parent` also makes the diff of a merge commit be the diff
# against its first parent, which is exactly "what this landing brought
# to main". `-M` means a pure file move is +0/-0 rather than a full
# delete plus a full add: moving a file is not growth, and without it
# every refactor reads as a 100% delete:add ratio.
# ---------------------------------------------------------------------
AWK_SERIES='
function flush() {
    if (sha == "") return
    rows = rows sep sprintf("{\"sha\":%s,\"at\":%s,\"subject\":%s,\"add\":%d,\"del\":%d,\"test_add\":%d,\"test_del\":%d}", \
        jstr(sha), jstr(at), jstr(subj), c_add + 0, c_del + 0, c_tadd + 0, c_tdel + 0)
    sep = ","
    n++
    w_add += c_add; w_del += c_del
    sha = ""; c_add = 0; c_del = 0; c_tadd = 0; c_tdel = 0
}
BEGIN { FS = "\t"; sep = ""; n = 0; w_add = 0; w_del = 0 }
/^\001/ {
    flush()
    sha = substr($1, 2); at = $2
    subj = $3
    for (i = 4; i <= NF; i++) subj = subj "\t" $i
    next
}
NF >= 3 {
    a = $1; d = $2; p = $3
    for (i = 4; i <= NF; i++) p = p "\t" $i
    # A rename: numstat writes `old => new` or `dir/{old => new}`. The
    # new path is the one that decides the bucket.
    if (p ~ /\{[^{}]* => [^{}]*\}/) { sub(/\{[^{}]* => /, "", p); sub(/\}/, "", p) }
    else if (p ~ / => /) { sub(/^.* => /, "", p) }
    if (skip(p)) next
    # A binary file has no lines; git writes `-` for both counts.
    if (a == "-") a = 0
    if (d == "-") d = 0
    b = bucket(p)
    c_add += a; c_del += d
    b_add[b] += a; b_del[b] += d
    if (is_test(b)) { c_tadd += a; c_tdel += d }
}
END {
    flush()
    printf "{\"landings\":[%s],\"window\":{\"landings\":%d,\"adds\":%d,\"dels\":%d,\"net\":%d,", \
        rows, n, w_add, w_del, w_add - w_del
    # NO DENOMINATOR IS NOT ZERO PERCENT. A window that added nothing has
    # no delete:add ratio at all, and reporting 0 would read as "nothing
    # was deleted" — the opposite of what happened.
    if (w_add > 0) printf "\"delete_add_pct\":%d,", int(w_del * 100.0 / w_add + 0.5)
    else printf "\"delete_add_pct\":null,"
    printf "\"by_bucket\":{"
    for (i = 1; i in BUCKETS; i++) {
        b = BUCKETS[i]
        printf "%s\"%s\":{\"add\":%d,\"del\":%d}", (i > 1 ? "," : ""), b, b_add[b] + 0, b_del[b] + 0
    }
    printf "}}}\n"
}
'

series_json() {
    local range log
    if [ -n "$SINCE" ]; then range="$SINCE..$REF"; else range="$REF"; fi
    log=$(git_answer "$NAME" 0 log --first-parent --numstat -M \
        --date=format-local:%Y-%m-%dT%H:%M:%SZ \
        --format="%x01%H%x09%ad%x09%s" "$range") || return $?
    local body
    body=$(printf '%s\n' "$log" | TZ=UTC awk "$AWK_LIB$AWK_SERIES") || return 1
    printf '%s' "$body" | jq \
        --arg ref "$REF" --arg head "$HEAD_SHA" --arg since "$SINCE" '
        {ref: $ref, head: $head,
         since: (if $since == "" then null else $since end),
         backfill: ($since == ""),
         landings: .landings, window: .window}'
}

# ---------------------------------------------------------------------
# THE SNAPSHOT — totals at one commit, read from that commit's tree
# rather than from the working copy, so the answer is a function of the
# sha and not of whatever the checkout happens to have dirty.
# ---------------------------------------------------------------------
#
# It takes the FILE LIST on stdin, one path per record, and opens each
# file itself rather than being handed them as arguments: an argument
# list long enough for xargs to split would run END twice and print two
# JSON objects, and the symptom of that is a parse error on a day the
# tree grew, which is the worst possible day to be debugging the
# measurement.
AWK_SNAPSHOT='
{
    path = $0
    p = substr(path, length(root) + 1)
    if (skip(p)) next
    b = bucket(p)
    rs = (b == "rust_prod" && p ~ /\.rs$/)
    in_t = 0; pending = 0; depth = 0
    rc = (getline ln < path)
    while (rc > 0) {
        if (!rs) { lines[b]++ }
        else if (in_t == 0 && pending == 0) {
            # The inline-test state machine. A `#[cfg(test)]` attribute is
            # itself test code, and so is the item it governs, from its
            # first brace to the one that closes it. An attribute on a
            # `use` line (no braces at all) attributes only itself, which
            # is correct.
            if (ln ~ /^[ \t]*#\[cfg\(test\)\]/) { lines["rust_test_inline"]++; attrs++; pending = 1 }
            else lines["rust_prod"]++
        } else {
            if (pending == 1) { in_t = 1; pending = 0; depth = 0 }
            lines["rust_test_inline"]++
            o = gsub(/\{/, "{", ln); c = gsub(/\}/, "}", ln)
            depth += o - c
            if (depth <= 0 && (o + c) > 0) in_t = 0
        }
        rc = (getline ln < path)
    }
    close(path)
    # A file that could not be read is a fact about the MACHINE. Counted,
    # and the caller refuses on it — totals over a tree this never
    # finished reading would be confidently short.
    if (rc < 0) unreadable++
}
END {
    total = 0
    printf "{\"by_bucket\":{"
    for (i = 1; i in BUCKETS; i++) {
        b = BUCKETS[i]
        printf "%s\"%s\":%d", (i > 1 ? "," : ""), b, lines[b] + 0
        total += lines[b] + 0
    }
    printf "},\"lines\":%d", total
    prod = lines["rust_prod"] + lines["web_prod"] + 0
    test = lines["rust_test"] + lines["rust_test_inline"] + lines["web_test"] + 0
    printf ",\"prod_lines\":%d,\"test_lines\":%d", prod, test
    if (prod > 0) printf ",\"test_prod_pct\":%d", int(test * 100.0 / prod + 0.5)
    else printf ",\"test_prod_pct\":null"
    printf ",\"inline_test_attributes\":%d", attrs + 0
    printf ",\"unreadable_files\":%d}\n", unreadable + 0
}
'

# ---------------------------------------------------------------------
# THE CODE-BRANCH HALF of CLAUDE.md §9, counted by `boss-leaked-policy`
# (crates/core/boss-testing/src/leaked_policy.rs — the rule is written out
# there, and every rung of it is pinned by a test whose fixture is copied
# from the tree it measures).
#
# WHY A SEPARATE BINARY AND NOT MORE AWK. The two gates the count rests on
# are "the scrutinee names a kind" and "an arm literal is a
# registry-declared kind", and both need Rust items: arms span lines and
# nest, an inline `#[cfg(test)] mod tests` has to be skipped by brace
# structure, and a `FromStr` whose vocabulary collides with a step-kind
# spelling has to be told apart from a branch on that kind. awk can do the
# line arithmetic this script is otherwise made of; it cannot do this.
#
# WHERE IT IS LOOKED FOR, in order, and WHY THE ABSENCE IS A SENTENCE
# RATHER THAN A ZERO. boss-gcp's converge deliberately does not build
# (`infra/gcp/boss-gcp-converge.sh`: "This does not build, stage binaries,
# converge the schema, or restart a service"), so the counter is present
# on a box only once somebody built it there. A machine without it must
# say "not measured here", because reporting 0 leaked branches from a box
# with no Rust toolchain is the same defect as a query against the wrong
# deployment answering `total: 0` — well-formed, confident and wrong.
leaked_policy_bin() {
    if [ -n "${BOSS_LEAKED_POLICY_BIN:-}" ]; then
        # Explicitly named and not runnable is a CONFIGURATION error, and
        # it gets said out loud rather than silently falling back to a
        # different binary than the operator asked for.
        if [ -x "$BOSS_LEAKED_POLICY_BIN" ]; then
            printf '%s' "$BOSS_LEAKED_POLICY_BIN"
            return 0
        fi
        return 1
    fi
    # `$CARGO_TARGET_DIR` before `$REPO/target`: a pod or dev box that
    # redirects cargo's output has no `target/` in the checkout at all,
    # and the first cut of this looked only at `$REPO/target` and so
    # reported "no counter on this machine" from a machine that had just
    # built one.
    local candidate
    for candidate in "${CARGO_TARGET_DIR:-}/release/boss-leaked-policy" \
        "${CARGO_TARGET_DIR:-}/debug/boss-leaked-policy" \
        "$REPO/target/release/boss-leaked-policy" \
        "$REPO/target/debug/boss-leaked-policy"; do
        # An unset CARGO_TARGET_DIR leaves `/release/boss-leaked-policy`,
        # an absolute path into the root filesystem. Skip it rather than
        # stat a path nobody meant.
        case "$candidate" in
            /release/* | /debug/*) continue ;;
        esac
        if [ -x "$candidate" ]; then
            printf '%s' "$candidate"
            return 0
        fi
    done
    command -v boss-leaked-policy 2>/dev/null && return 0
    return 1
}

# Writes the counter's JSON to `<scratch>/leaked.json` and, when there is
# no number, the sentence explaining that to `<scratch>/leaked.why`.
#
# FILES RATHER THAN VARIABLES, on purpose: the first cut echoed the JSON
# and set a global for the reason, and a caller's `x=$(fn …)` runs the
# function in a SUBSHELL, so the reason died with it. The symptom was a
# row carrying `code_branches_on_kind: null` beside
# `code_branches_not_counted_why: null` — a blank that does not even say
# it is a blank, which is the one outcome this field exists to prevent.
#
# Takes the EXTRACTED TREE, not the checkout: the count must belong to the
# same sha every other figure in the snapshot belongs to.
leaked_policy_measure() { # <extracted tree> <scratch dir>
    local tree="$1" scratch="$2" bin
    : >"$scratch/leaked.json"
    : >"$scratch/leaked.why"
    if ! bin=$(leaked_policy_bin); then
        if [ -n "${BOSS_LEAKED_POLICY_BIN:-}" ]; then
            printf '%s' "BOSS_LEAKED_POLICY_BIN=$BOSS_LEAKED_POLICY_BIN is not an executable, so the code-branch half was not counted. Named explicitly and not runnable is a configuration error, not a reason to silently count with something else." >"$scratch/leaked.why"
        else
            printf '%s' "No boss-leaked-policy on this machine (tried \$BOSS_LEAKED_POLICY_BIN, \$CARGO_TARGET_DIR, $REPO/target/release, $REPO/target/debug, \$PATH), so the code-branch half of CLAUDE.md §9 was NOT counted on this run. This is a fact about the machine, not about the codebase: the count is unmeasured here, which is not the same as zero. Build it with \`cargo build --release -p boss-testing --bin boss-leaked-policy\`." >"$scratch/leaked.why"
        fi
        return 0
    fi
    if "$bin" --repo "$tree" --scope crates/core \
        >"$scratch/leaked.json" 2>"$scratch/leaked.err"; then
        return 0
    fi
    # THE WHOLE REFUSAL, to the journal AND onto the row. A reduction
    # before the record is stored throws away the only copy, and this
    # file's own `unreadable_files` path makes the same argument.
    : >"$scratch/leaked.json"
    sed 's/^/    /' "$scratch/leaked.err" >&2
    printf '%s' "$bin refused rather than counting: $(tr '\n' ' ' <"$scratch/leaked.err"). Its full output is in this run's stderr." >"$scratch/leaked.why"
    return 0
}

snapshot_json() {
    local tmp
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/codebase-metrics.XXXXXX") || {
        echo "$NAME: $LINT_CANNOT_ANSWER_MARKER — no writable temp dir, so the tree could not be read." >&2
        return "$LINT_CANNOT_ANSWER"
    }
    # shellcheck disable=SC2064
    trap "rm -rf '$tmp'" RETURN
    mkdir -p "$tmp/tree"
    if ! git archive --format=tar "$REF" 2>"$tmp/err" | tar -x -C "$tmp/tree" 2>>"$tmp/err"; then
        {
            printf '%s: %s — `git archive %s` could not be extracted, so no file was counted.\n' \
                "$NAME" "$LINT_CANNOT_ANSWER_MARKER" "$REF"
            sed 's/^/    /' "$tmp/err"
        } >&2
        return "$LINT_CANNOT_ANSWER"
    fi

    local totals unreadable
    totals=$(find "$tmp/tree" -type f \
        | awk -v root="$tmp/tree/" "$AWK_LIB$AWK_SNAPSHOT") || return 1
    unreadable=$(printf '%s' "$totals" | jq '.unreadable_files')
    if [ "$unreadable" != "0" ]; then
        {
            printf '%s: %s — %s files in %s could not be read, so these totals\n' \
                "$NAME" "$LINT_CANNOT_ANSWER_MARKER" "$unreadable" "$REF"
            printf '  would be short by an unknown amount. An infrastructure refusal (exit %s),\n' \
                "$LINT_CANNOT_ANSWER"
            printf '  not a codebase that shrank.\n'
        } >&2
        return "$LINT_CANNOT_ANSWER"
    fi

    # COUNTS, from the commit's file list. `git ls-tree` answers 0 or
    # fails; there is no "no hits" status to confuse with a refusal.
    local tree
    tree=$(git_answer "$NAME" 0 ls-tree -r --name-only "$REF") || return $?
    count() { printf '%s\n' "$tree" | grep -cE "$1" || true; }
    local counts
    counts=$(jq -n \
        --argjson crates "$(count '^crates/[^/]+/[^/]+/Cargo\.toml$')" \
        --argjson core "$(count '^crates/core/[^/]+/Cargo\.toml$')" \
        --argjson modules "$(count '^crates/modules/[^/]+/Cargo\.toml$')" \
        --argjson orchestrators "$(count '^crates/orchestrators/[^/]+/Cargo\.toml$')" \
        --argjson tenants "$(count '^crates/tenants/[^/]+/Cargo\.toml$')" \
        --argjson lints "$(count '^infra/lint/[^/]+\.sh$')" \
        --argjson migrations "$(count '^infra/postgres/schema/.*\.sql$')" \
        --argjson rust_files "$(count '\.rs$')" \
        --argjson web_files "$(count '^(apps|libs)/.*\.(ts|svelte)$')" \
        '{crates: $crates,
          crates_by_tier: {core: $core, modules: $modules,
                           orchestrators: $orchestrators, tenants: $tenants},
          lints: $lints, migrations: $migrations,
          rust_files: $rust_files, web_files: $web_files}')

    # REGISTRY ROWS — the half of CLAUDE.md §9 that can be counted
    # exactly. One number per registry, and the total, so "is new
    # behaviour arriving as data" has an answer that is not a feeling.
    rows() { # <toml array header> <path...>
        local header="$1"; shift
        grep -rhc "^\\[\\[$header\\]\\]" "$@" 2>/dev/null \
            | awk '{s += $1} END {print s + 0}'
    }
    local t="$tmp/tree"
    local reg_rules reg_wf reg_tenant_wf reg_steptypes reg_plugins
    reg_rules=$(rows rule "$t/infra/dispatcher/rules")
    reg_wf=$(rows workflow "$t/infra/platform/workflows")
    reg_tenant_wf=$(rows workflow "$t"/examples/*/seeds/workflows.toml)
    reg_steptypes=$(rows step_type "$t/crates/core/boss-jobs/seeds/step_types.toml")
    reg_plugins=$(find "$t/infra/step-plugins" -name '*.js' 2>/dev/null | wc -l)
    # THE CODE-BRANCH HALF. Counted off the SAME extracted tree, so both
    # halves of the §9 measurement — rows and branches — belong to one sha.
    leaked_policy_measure "$t" "$tmp"
    local leaked leaked_why
    leaked=$(cat "$tmp/leaked.json")
    leaked_why=$(cat "$tmp/leaked.why")

    local registry
    registry=$(jq -n \
        --argjson rules "$reg_rules" --argjson workflows "$reg_wf" \
        --argjson tenant_workflows "$reg_tenant_wf" \
        --argjson step_types "$reg_steptypes" --argjson step_plugins "$reg_plugins" \
        --argjson leaked "${leaked:-null}" \
        --arg leaked_why "$leaked_why" \
        '{rows: ($rules + $workflows + $tenant_workflows + $step_types + $step_plugins),
          by_registry: {dispatcher_rules: $rules, platform_workflows: $workflows,
                        tenant_workflows: $tenant_workflows,
                        step_types: $step_types, step_plugins: $step_plugins},
          # Every key, always, null included — a stable row shape is what
          # makes the series queryable without the reader guessing which
          # keys a given day happened to have.
          code_branches_on_kind: ($leaked | if . == null then null else .leaked_policy end),
          code_branches_unclassified: ($leaked | if . == null then null else .unclassified end),
          code_branches_by_class: ($leaked | if . == null then null else .by_class end),
          # The audit trail: which file and line every leaked and every
          # undecided branch is at, so the integer can be checked by hand
          # without re-running the pass. That hand-check is the only
          # evidence a number is worth filing daily.
          code_branches_sites: ($leaked | if . == null then null else .sites end),
          code_branches_scanned: ($leaked | if . == null then null else
            {scopes: .scopes, files: .files_parsed, matches: .matches,
             vocabulary_kinds: .vocabulary.kinds,
             round_trip_over_registry_kinds: .round_trip_over_registry_kinds} end),
          code_branches_method: ($leaked | if . == null then null else .method end),
          # A blank that does not say it is a blank is the one outcome
          # this field exists to prevent, so no number AND no reason is
          # itself a reason.
          code_branches_not_counted_why:
            (if $leaked_why != "" then $leaked_why
             elif $leaked == null then
               "The counter produced neither a measurement nor a reason — a bug in codebase-metrics.sh itself, not a fact about the codebase. Re-run `codebase-metrics.sh snapshot` and read this run'"'"'s stderr."
             else null end)}')

    printf '%s' "$totals" | jq \
        --arg ref "$REF" --arg head "$HEAD_SHA" \
        --argjson counts "$counts" --argjson registry "$registry" \
        --argjson method "$(method_json)" \
        '{ref: $ref, head: $head, totals: ., counts: $counts,
          registry: $registry, method: $method}'
}

# The limits of the measurement, on the same row as the measurement.
method_json() {
    jq -n '{
      series: "One row per first-parent commit on the measured ref — every landing on main, trains and direct merges alike. `git log --first-parent --numstat -M`: a merge commits diff is against its first parent, so a rows add/del is what that landing brought to main, and -M means a pure file move is +0/-0 rather than a delete plus an add.",
      inline_test_attribution: "The SNAPSHOT attributes inline `#[cfg(test)]` code exactly, by brace-matching the item each attribute governs (see inline_test_attributes for how many it found). The per-landing SERIES attributes by PATH ONLY — a `.rs` file outside a tests/ directory counts as rust_prod even where the changed lines were inside a #[cfg(test)] module, because deciding otherwise needs the block structure of the file as it stood before each commit. Measured 2026-09-11, inline test code is 29% of the non-tests/ Rust lines, so treat the series prod/test split as an upper bound on prod and a lower bound on test; the snapshots split is the exact one.",
      skipped: "Lockfiles (Cargo.lock, bun.lock, package-lock.json) and binary assets are counted in neither bucket, in the series and the snapshot alike: a lockfile churns thousands of lines for a one-line dependency bump.",
      buckets: "rust_prod / rust_test (a tests/ or benches/ directory) / rust_test_inline (snapshot only) / web_prod / web_test (*.test.*, *.spec.*, apps/*/tests/) / schema / registry (dispatcher rules, platform workflows, step plugins, and every crate seeds/ directory) / lint / infra / seed (examples/) / docs / other. crates/core/boss-testing/src counts as rust_prod: it is a library other crates link, and reclassifying a whole crate on the strength of its name is a judgement a path rule should not make silently.",
      registry_rows: "`registry.rows` counts five NAMED registries — dispatcher rules, platform workflow rows, tenant workflow rows, step types, step plugin bundles — not every seed file in the tree. The registry LINE bucket is deliberately broader (any crate seeds/ directory), so the two answer different questions: rows is how much BEHAVIOUR is data, lines is how much of the tree that data occupies.",
      code_branches: "`registry.code_branches_on_kind` is the other half of CLAUDE.md §9, counted by `boss-leaked-policy` over a syn AST (never a regex) on the SAME extracted tree. `code_branches_method` on the same row states the classification rule; `code_branches_sites` names every leaked and every undecided site by file and line. TWO LIMITS: it counts `match` expressions only, so `if kind == \"sign-off\"` and `matches!(kind, \"…\")` are NOT in this number — `infra/lint/no-step-kind-match.sh` is the check that covers those shapes, and the two are complementary. And it is scoped to `crates/core`, because §9 is about core code; widening the scope would move the number for a reason that is not a change in the codebase, so the scope rides in `code_branches_scanned.scopes`. When the field is null, `code_branches_not_counted_why` says which machine fact made it null — never a zero.",
      snapshot_source: "The measured commits tree (git archive), not the working copy, so totals are a function of the sha."
    }'
}

row_json() {
    local s w m head_at
    s=$(snapshot_json) || return $?
    w=$(series_json) || return $?
    m=$(method_json) || return 1
    # WHEN THE MEASURED COMMIT LANDED, beside when it was measured. A
    # cadence reading a checkout somebody stopped converging would
    # otherwise file a current-looking row about a week-old tree, and the
    # flat trend that produces is indistinguishable from a quiet week.
    head_at=$(git_answer "$NAME" 0 log -1 \
        --date=format-local:%Y-%m-%dT%H:%M:%SZ --format=%ad "$HEAD_SHA") || return $?
    jq -n --argjson s "$s" --argjson w "$w" --argjson m "$m" \
        --arg head_at "$head_at" \
        --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
        {measured: {at: $at, ref: $w.ref, head: $w.head, head_at: $head_at,
                    since: $w.since,
                    backfill: $w.backfill,
                    window: $w.window, totals: $s.totals, counts: $s.counts,
                    registry: $s.registry, method: $m},
         landings: $w.landings}'
}

# ---------------------------------------------------------------------
# FILING — the packet is the record. The open packet was opened by this
# unit's ExecStartPre (boss-maintenance-wrap.sh); this writes the
# measurement onto it with PATCH /api/jobs/{id}/metadata, which MERGES
# top-level keys server-side. Never the full job PUT: that REPLACES, so
# every annotation would depend on reconstructing the whole envelope
# correctly, and a concurrent writer loses.
# ---------------------------------------------------------------------
API_CURL="$SELF_DIR/boss-api-curl.sh"
[ -x "$API_CURL" ] || API_CURL=boss-api-curl.sh
BOSS_USER='{"id":"automation:codebase-metrics","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

api() { # <method> <path> [body]
    local method="$1" path="$2" body="${3:-}"
    if [ -n "$body" ]; then
        "$API_CURL" -fsS -X "$method" -H "x-boss-user: $BOSS_USER" \
            -H "content-type: application/json" \
            ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
            -d "$body" "$BOSS_JOBS_URL$path"
    else
        "$API_CURL" -fsS -H "x-boss-user: $BOSS_USER" "$BOSS_JOBS_URL$path"
    fi
}

file_row() {
    if [ -z "${BOSS_JOBS_URL:-}" ]; then
        echo "$NAME: BOSS_JOBS_URL is not set, and there is no safe default — a measurement filed on the wrong instance is a measurement nobody reads (infra/boss-step.sh carries the argument)." >&2
        exit 78   # EX_CONFIG
    fi

    # WHERE THE WINDOW STARTS: the newest CLOSED packet's measured head.
    # Closed, not any — a re-run that reuses yesterdays still-open packet
    # must re-measure the same window, not an empty one.
    local filed since
    filed=$(api GET "/api/jobs?kind=$KIND&limit=50") || {
        echo "$NAME: could not read filed $KIND packets, so the window start is unknown — refusing to file a backfill over a series that already exists." >&2
        exit 1
    }
    since=$(printf '%s' "$filed" | jq -r '
        [(.data // [])[] | select(.status != "open")
         | select(.metadata.measured.head != null)]
        | sort_by(.created_at) | last | .metadata.measured.head // ""')
    [ "$since" = "null" ] && since=""
    SINCE="$since"

    local row
    row=$(row_json) || exit $?

    local open open_count job_id
    open=$(api GET "/api/jobs?kind=$KIND&status=open&limit=2") || {
        echo "$NAME: could not read the open $KIND packet; the measurement below was computed and NOT filed." >&2
        printf '%s\n' "$row" >&2
        exit 1
    }
    open_count=$(printf '%s' "$open" | jq '(.data // []) | length')
    if [ "$open_count" = "0" ]; then
        echo "$NAME: no open $KIND packet to file onto — ExecStartPre could not open one (the jobs API was unreachable then, most likely). The measurement is below; this run recorded nothing." >&2
        printf '%s\n' "$row" >&2
        exit 1
    fi
    if [ "$open_count" != "1" ]; then
        echo "$NAME: $open_count open $KIND packets — refusing to guess which one today's measurement belongs on." >&2
        exit 1
    fi
    job_id=$(printf '%s' "$open" | jq -r '.data[0].id')

    # The metadata patch: `measured` is the headline row, `landings` the
    # per-landing series. Two top-level keys, merged server-side.
    local body
    body=$(printf '%s' "$row" | jq '{measured: .measured, landings: .landings}')
    api PATCH "/api/jobs/$job_id/metadata" "$body" >/dev/null || {
        echo "$NAME: filing the measurement onto ${job_id:0:8} failed. It is below rather than lost." >&2
        printf '%s\n' "$row" >&2
        exit 1
    }

    # THE JOURNAL GETS THE HEADLINE, not a digest of it. A reduction
    # before the record is stored throws away the only copy (CLAUDE.md
    # §Diagnosis), so the full row is on the packet and the two ratios
    # are here, where a `systemctl status` shows them.
    printf '%s' "$row" | jq -r --arg id "${job_id:0:8}" '
        .measured as $m
        | "codebase-metrics: \($m.window.landings) landings " +
          (if $m.backfill then "(BACKFILL: the whole history)" else "since \($m.since[0:8])" end) +
          ": +\($m.window.adds)/-\($m.window.dels), net \($m.window.net), " +
          "delete:add " + (if $m.window.delete_add_pct == null then "n/a (nothing added)" else "\($m.window.delete_add_pct)%" end),
          "  totals at \($m.head[0:8]): \($m.totals.lines) lines — prod \($m.totals.prod_lines), tests \($m.totals.test_lines) (\($m.totals.test_prod_pct)% of prod)",
          "  registry rows \($m.registry.rows); code branches on kind: " +
            (if $m.registry.code_branches_on_kind == null
             then "NOT counted (the row says why)"
             else "\($m.registry.code_branches_on_kind) leaked" +
                  ", \($m.registry.code_branches_unclassified) unclassified" +
                  ", of \($m.registry.code_branches_scanned.matches) string-literal matches in \($m.registry.code_branches_scanned.scopes | join(", "))"
             end),
          "  filed on \($id)"'
}

case "$CMD" in
    snapshot) snapshot_json ;;
    series) series_json ;;
    row) row_json ;;
    file) file_row ;;
esac
