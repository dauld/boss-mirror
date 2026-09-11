#!/usr/bin/env bash
#
# a-deleted-manifest-leaves-no-object — deleting a manifest must delete
# the thing it declared. The converge cannot do that, so this is what
# says so out loud.
#
# WHY THIS EXISTS
# ---------------
# The seam is stated once, in infra/cluster/undeclared-objects.sh's
# header: the converge's apply does not prune, so deleting a manifest
# removes the DECLARATION and leaves the OBJECT, and
# check-manifests-applied.sh cannot see it by construction. This script
# is the surface that REPORTS it to a human, and the one that also covers
# the objects a deleted file declared.
#
# CLAUDE.md records the mirror image: "an imperative cluster change has
# an expiry" — a `kubectl` change not in the manifests silently REVERTS
# at the next converge. This is the same seam read the other way: a
# manifest deletion silently does NOT take effect.
#
# Found concretely on 2026-09-10: `fix/the-corpus-index-is-deleted`
# deletes `boss-docs-internal.yaml`, and its builder had to note in prose
# that somebody must type `kubectl -n boss delete svc
# boss-docs-internal` by hand, because nothing in the pipeline would
# ever mention it again. That note is the defect — a hand-off that lives
# only in a commit message is not a mechanism (CLAUDE.md §9a).
#
# WHY NOT JUST TURN ON `--prune`
# ------------------------------
# Considered and deliberately refused; the reasons are worth keeping
# next to the check, because the next reader will ask.
#
#   1. `--prune` needs a scope it cannot derive correctly here. It
#      takes `-l <selector>` or `--all`, and these manifests carry no
#      common label. Labelling 21 files is a tree-wide edit whose
#      failure mode is a file that got MISSED: its object falls out of
#      the prune set, or — worse, with `--all` — everything the apply
#      did not see that run gets deleted.
#   2. The apply's view is already derived, not named. The runner copies
#      the directory to a temp dir and rewrites the image tag
#      (`manifests_with_image`). A file that fails to copy or render is
#      absent from the apply — and under prune, absent means DELETE.
#      "Roll back is a target, not a verb" (CLAUDE.md §Diagnosis) is the
#      same lesson: an irreversible operation must act on a named
#      target, never on a set something computed a moment ago.
#   3. The apply set contains the system of record's storage: the
#      `postgres` and `nats` StatefulSets and the PVCs under them
#      (`boss-auth`, `boss-backups`, `pgdata-postgres-0`,
#      `jsdata-nats-0`). A mis-scoped prune there is not an outage, it is
#      data loss on Longhorn volumes holding the audit log.
#   4. The work being automated is rare. A manifest deletion has
#      happened once in this repository's history. Automating a rare
#      irreversible operation to save one typed command is the wrong
#      trade; making the rare event LOUD is the right one.
#
# So the deletion stays a human step, and this check is what guarantees
# the human is told. The converge also now prints, at its apply step,
# that it does not prune — so the next reader does not assume semantics
# it does not have.
#
# THE TWO CHECKED PROPERTIES
# --------------------------
# A. NOTHING THE TREE DELETED IS STILL RUNNING. Every manifest path that
#    this branch's history (or its working tree) removed from
#    $DIR, whose objects are not declared anywhere in the tree any
#    more, must be absent from the cluster. This one is exact: it names
#    only objects the tree ITSELF once declared, so it cannot cry wolf,
#    and it covers the kinds property B leaves out of scope. A rename
#    is not a deletion — the object is still declared, by a different
#    file, so it never reaches this report.
#
# B. NOTHING LIVE IS UNDECLARED, in the narrow slice where that
#    question is well posed. This is the sweep that catches an orphan
#    whose deletion predates this check, or one deleted by a path git
#    cannot see — and IT IS NOT COMPUTED HERE.
#    `undeclared-objects.sh --list` is the one definition of "what is
#    running that the tree does not declare"; it states its own scope,
#    its excluded kinds, its exemptions and its UNVERIFIED accounting in
#    its own header, and this script asks it and reports what it says.
#
#    Until 2026-09-11 all of it lived here a second time — an
#    $EXCLUDED_KINDS, an $EXEMPT, an $EXEMPT_ANY_NS, an `is_exempt` and a
#    sweep of their own — and that second copy is what made backlog
#    19aa75e0 possible: the parse-error discard the packet named was in
#    THIS file, while the derivation had already been fixed in train
#    #308. Two definitions of one question differ in exactly the places
#    nobody compared (CLAUDE.md §9a), and at the far end of
#    `delete-orphan-object`, whose authority IS the derivation, a
#    divergence names a DECLARED object as deletable.
#
#    What survives here is the one thing the derivation does not ask
#    about itself: ITS EXEMPTIONS MUST NOT GO STALE — an exemption for
#    something the tree now declares, or for something the cluster no
#    longer has, would excuse a future object of that name without
#    anyone deciding so. The list is READ from `--exemptions`, never
#    held here.
#
# WHEN THE CLUSTER IS UNREACHABLE it SKIPS, loudly, and exits 0. This
# runs in the gate pre-flight roster, and a gate has no reach: the
# in-cluster gate Job runs with `automountServiceAccountToken: false`
# against a CI image whose tool manifest
# (infra/forge/boss-ci/required-tools.txt) carries no kubectl, and a
# probe run on the forge host as david finds none either
# (infra/forge/host-absent-tools.txt, measured 2026-09-09). A lint that
# reddened there would red every car.
#
# The CONVERGE is the run that has reach — measured, and measured in the
# derivation's header, where the forge host's two true statements about
# `kubectl` are written down once.
#
# What this must never do is read "could not look" as "nothing to report"
# — a wrong target answers instead of erroring (CLAUDE.md §Doors) — so
# anything it could not read is counted and NAMED as unverified, never as
# clean, and the coverage the run achieved is stated either way.
#
# Usage:  infra/lint/a-deleted-manifest-leaves-no-object.sh
#   KUBECONFIG  a credential that can read the managed namespaces; the
#               derivation's header says which part of them the dev pod's
#               narrow session credential reaches.
#
# EXIT. The converge reads any nonzero as a stop, so 1 carries both a
# finding and a refusal, and what tells them apart is what the run SAYS.
# Both shapes are guaranteed:
#   0  no orphan in what it could read (or it could read nothing and
#      said so)
#   1  A FINDING NAMES OBJECTS — an object whose manifest the tree
#      deleted is still running, the derivation named an undeclared one,
#      or one of its exemptions has gone stale. A REFUSAL NAMES A FILE OR
#      A CODE AND NO OBJECT, and claims nothing about the cluster: the
#      derivation could not derive the declared set or could not sweep,
#      and its CANNOT ANSWER (exit 4) is carried through with the code in
#      the message. A manifest that will not parse lands here — every
#      object it declares would otherwise read as an orphan, so there is
#      no answer to give. "Could not look" is never rounded to "nothing
#      to report", and never dressed up as a finding either.

set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

DIR="infra/cluster/manifests"

# WHICH KINDS ARE SWEPT and which live objects are tolerated are the
# derivation's to say — see the header. boss-testing's
# undeclared_objects_sh.rs refuses a second $EXCLUDED_KINDS, $EXEMPT,
# $EXEMPT_ANY_NS or `is_exempt` here: a list nobody reads is inert rather
# than wrong, so no run of this script could ever see one.

problems=0
fail() { echo "a-deleted-manifest-leaves-no-object: $*" >&2; problems=$((problems + 1)); }

# ---------------------------------------------------------------------------
# Static half — always runs, needs no network.
# ---------------------------------------------------------------------------
[ -d "$DIR" ] || { echo "a-deleted-manifest-leaves-no-object: $DIR does not exist" >&2; exit 1; }

shopt -s nullglob
# Counted here only, as the static half's own floor — it runs with no
# cluster, which is where most runs of this check stop. WHICH files make
# up the declared set is the derivation's question, and it is asked once,
# below. (Its answer also spans infra/gate-runner/*.yaml: the gate runner
# applies its own PVC and Job, so an object they declare is DECLARED,
# just not converged — which is what keeps `gate-runner-disk` from
# reading as an orphan.)
MANIFESTS=("$DIR"/*.yaml)
shopt -u nullglob

if [ ${#MANIFESTS[@]} -lt 10 ]; then
    echo "a-deleted-manifest-leaves-no-object: found only ${#MANIFESTS[@]} manifest(s) in $DIR — the scrape broke." >&2
    echo "  Refusing rather than reporting every live object as an orphan." >&2
    exit 1
fi

command -v python3 >/dev/null 2>&1 || {
    echo "a-deleted-manifest-leaves-no-object: python3 is not on this box — cannot parse manifests." >&2
    exit 1
}

# kind<TAB>ns<TAB>name for every object in the manifests fed on stdin as
# kubectl's own JSON. Using kubectl's parser rather than growing a YAML
# implementation, exactly as check-manifests-applied.sh does.
#
# A StatefulSet also declares the PVCs its volumeClaimTemplates will
# create — `<template>-<set>-<ordinal>` — and those PVCs carry no
# ownerReference, so without this they look exactly like orphans. This
# is the "compare like with like" clause: the tree DOES declare
# pgdata-postgres-0, it just spells it as a template.
#
# The JSON arrives in a FILE, not on stdin: the python body itself is
# this function's stdin (`python3 - <<PY`), so a script that also read
# stdin would read an empty string and report a clean cluster. It did,
# the first time this ran.
objects_from_json() { # json-file
    python3 - "$1" <<'PY'
import json, sys

dec = json.JSONDecoder()
s = open(sys.argv[1]).read()
docs, i, n = [], 0, len(s)
while i < n:
    while i < n and s[i] in " \n\r\t":
        i += 1
    if i >= n:
        break
    obj, i = dec.raw_decode(s, i)
    docs.append(obj)

for d in docs:
    if not isinstance(d, dict):
        continue
    kind = d.get("kind")
    md = d.get("metadata") or {}
    name, ns = md.get("name"), md.get("namespace") or ""
    # No name means a generateName template (the gate Job). It declares
    # no specific object, so it can neither be found nor be missed.
    if not kind or not name:
        continue
    print(f"{kind}\t{ns}\t{name}")
    if kind == "StatefulSet":
        spec = d.get("spec") or {}
        try:
            replicas = 1 if spec.get("replicas") is None else int(spec["replicas"])
        except (TypeError, ValueError):
            replicas = 1
        for tmpl in spec.get("volumeClaimTemplates") or []:
            tn = ((tmpl.get("metadata") or {}).get("name"))
            if not tn:
                continue
            for ordinal in range(replicas):
                print(f"PersistentVolumeClaim\t{ns}\t{tn}-{name}-{ordinal}")
PY
}

# ---------------------------------------------------------------------------
# Self-test — the one part whose breakage would make this check LIE.
# ---------------------------------------------------------------------------
# Following invariant-register.sh and gate.sh's scope self-test: a check
# that cannot demonstrate itself is a check nobody can trust. This runs
# every time, cluster or no cluster, because it is pure string work and a
# rule that only self-tests when asked is a rule that stops working
# quietly.
#
# What it pins is the volumeClaimTemplates clause. Since property B
# became the derivation's, this parser reads only DELETED manifests, so a
# clause that silently stopped deriving `pgdata-postgres-0` would no
# longer cry wolf — it would do the quieter thing and UNDER-report:
# delete `postgres.yaml` and the PVC holding the audit log is never asked
# about. A finding that goes missing inside a passing run is the defect
# CLAUDE.md §Diagnosis names under "a check nobody reads".
self_test() {
    local fixture want got
    fixture=$(mktemp) || exit 1
    cat > "$fixture" <<'JSON'
{"kind":"StatefulSet","metadata":{"name":"postgres","namespace":"boss"},
 "spec":{"volumeClaimTemplates":[{"metadata":{"name":"pgdata"}}]}}
{"kind":"StatefulSet","metadata":{"name":"pair","namespace":"boss"},
 "spec":{"replicas":2,"volumeClaimTemplates":[{"metadata":{"name":"d"}}]}}
{"kind":"Job","metadata":{"generateName":"gate-","namespace":"boss-dev"}}
{"kind":"Service","metadata":{"name":"nats","namespace":"boss"}}
{"kind":"Namespace","metadata":{"name":"boss"}}
JSON
    # A StatefulSet with no `replicas` means one replica, so exactly one
    # PVC; an explicit 2 means two. A generateName template declares no
    # specific object and must produce no line — it can neither be found
    # nor be missed. A cluster-scoped object keeps an empty namespace
    # field rather than dropping it.
    want='PersistentVolumeClaim	boss	d-pair-0
PersistentVolumeClaim	boss	d-pair-1
PersistentVolumeClaim	boss	pgdata-postgres-0
Namespace		boss
Service	boss	nats
StatefulSet	boss	pair
StatefulSet	boss	postgres'
    got=$(objects_from_json "$fixture" | LC_ALL=C sort)
    rm -f "$fixture"
    if [ "$got" != "$(printf '%s\n' "$want" | LC_ALL=C sort)" ]; then
        echo "a-deleted-manifest-leaves-no-object: SELF-TEST FAILED — the manifest parser does not read what it claims to." >&2
        echo "  got:" >&2
        printf '    %s\n' "$got" >&2
        echo "  Refusing to compare the cluster against a parse this broken:" >&2
        echo "  a missed volumeClaimTemplate reports the audit log's PVC as an orphan." >&2
        exit 1
    fi
    echo "a-deleted-manifest-leaves-no-object: self-test ok — volumeClaimTemplate PVCs are derived, generateName templates declare nothing"
}
self_test

# ---------------------------------------------------------------------------
# Reach. Everything below needs the cluster; without it, skip loudly.
# ---------------------------------------------------------------------------
skip() {
    echo "a-deleted-manifest-leaves-no-object: SKIPPED the live comparison — $1" >&2
    echo "  Point KUBECONFIG at a credential that can read the namespaces" >&2
    echo "  $DIR declares. A gate cannot: the CI image carries no kubectl" >&2
    echo "  and the gate Job mounts no service-account token, so a skip" >&2
    echo "  here is the expected result of a gate, not a finding. The" >&2
    echo "  converge runs this with the admin kubeconfig." >&2
    echo "  Nothing is claimed about what the cluster is running." >&2
    [ "$problems" -eq 0 ] || exit 1
    exit 0
}

command -v kubectl >/dev/null 2>&1 || skip "kubectl is not on this box"
kubectl version -o json --request-timeout=10s >/dev/null 2>&1 \
    || skip "cannot reach the cluster API"

JSONBUF=$(mktemp) || exit 1
PARSEERR=$(mktemp) || exit 1
LISTERR=$(mktemp) || exit 1
GETERR=$(mktemp) || exit 1
trap 'rm -f "$JSONBUF" "$PARSEERR" "$LISTERR" "$GETERR"' EXIT

# The text of the last failed `kubectl get`, as one line. A reason not
# read out at the call site is a reason nobody will ever read: $GETERR is
# ONE file and the next probe overwrites it.
err_line() {
    [ -s "$GETERR" ] || { printf 'no output from kubectl'; return 0; }
    LC_ALL=C tr '\n\t' '  ' < "$GETERR" | LC_ALL=C sed 's/  */ /g; s/^ //; s/ $//'
}

# Is one named object live? 0 = yes, 1 = the server said it is not there,
# 2 = this credential could not tell. The three are never collapsed:
# reporting "could not read it" as "it is not there" states a fact about
# the cluster from evidence about the credential, and only NotFound is
# the server saying so. The words land in $GETERR for err_line.
object_is_live() { # kind name [ns]
    local args=()
    [ -n "${3:-}" ] && args=(-n "$3")
    if kubectl get "$1" "$2" "${args[@]}" --request-timeout=10s \
            >/dev/null 2>"$GETERR"; then
        return 0
    fi
    LC_ALL=C grep -qiE 'notfound|not found' "$GETERR" && return 1
    return 2
}

# --- what the tree declares -------------------------------------------------
# ASKED, NOT RECOMPUTED — the header says why, and backlog 19aa75e0 is the
# measurement: a local copy of this derivation ran `kubectl create
# --dry-run=client` per file with `2>/dev/null`, so an unparseable
# manifest was skipped, its objects fell out of the declared set, and
# `Service/svc-a` was reported as an orphan under advice to delete it
# (fixture in crates/core/boss-testing/tests/undeclared_objects_sh.rs).
# The derivation refuses with the filename instead.
DERIVE="infra/cluster/undeclared-objects.sh"
[ -x "$DERIVE" ] || {
    echo "a-deleted-manifest-leaves-no-object: $DERIVE is missing — it is where both the declared set and the orphan set are defined." >&2
    exit 1
}
# kind<TAB>ns<TAB>name<TAB>file for EVERY manifest in the tree, converged
# or not.
# `|| {` and NOT `if ! …; then`: inside the `then` of an `if !`, `$?` is
# the status the `!` produced, which is always 0 — so the first draft of
# this reported every refusal as "exit 0", a verdict that names nothing
# (CLAUDE.md §Diagnosis). Measured with `if ! (exit 4); then echo $?`.
declared_rows=$("$DERIVE" --declared 2>"$PARSEERR") || {
    rc=$?
    echo "a-deleted-manifest-leaves-no-object: CANNOT ANSWER — $DERIVE could not derive the declared set (exit $rc):" >&2
    sed 's/^/    /' "$PARSEERR" >&2
    echo "  A declared set missing a file is not a smaller declared set; every object that" >&2
    echo "  file declares would read as an orphan. Nothing is claimed about the cluster." >&2
    exit 1
}
# Anything it said while still answering is the reader's too: a warning
# swallowed on the success path is the same defect one notch quieter.
sed 's/^/  /' "$PARSEERR" >&2

# Property A compares identities, so the file column is dropped. NO FLOOR
# ON THE COUNT HERE: the derivation refuses below 20 converged objects
# before printing anything, and a second copy of that number could only
# drift from it (CLAUDE.md §9a). What this checks is that the rows
# PARSED — an answer this script cannot read is not a declared set, and an
# empty one would make every renamed object read as deleted-and-live.
declared_tree=$(printf '%s\n' "$declared_rows" \
    | LC_ALL=C awk -F'\t' 'NF >= 3 { print $1 "\t" $2 "\t" $3 }' \
    | grep -v '^[[:space:]]*$' | LC_ALL=C sort -u)
if [ -z "$declared_tree" ]; then
    fail "CANNOT ANSWER — $DERIVE answered, but no row of it parsed as kind<TAB>ns<TAB>name"
    echo "  Its output format has changed under this reader. Nothing is claimed about the cluster." >&2
    exit 1
fi

# A `Kind<TAB>ns<TAB>name` line the tree declares?
is_declared() { printf '%s\n' "$declared_tree" | LC_ALL=C grep -qxF "$1"; }

unreadable=0
unreadable_names=()

# ---------------------------------------------------------------------------
# Property A — nothing the tree DELETED is still running.
# ---------------------------------------------------------------------------
# The deleted paths: every manifest this branch's history removed from
# $DIR, plus any removed in the working tree or the index but not yet
# committed. `-m --first-parent` so a deletion that landed inside a
# merge is still seen; trains squash today, but a check that depends on
# that is a check that breaks the day they stop.
deleted_paths=$(
    {
        while IFS= read -r sha; do
            [ -n "$sha" ] || continue
            git show -m --first-parent --diff-filter=D --name-only --format= \
                "$sha" -- "$DIR" 2>/dev/null
        done <<EOF
$(git log -m --first-parent --diff-filter=D --format=%H -- "$DIR" 2>/dev/null)
EOF
        git diff --diff-filter=D --name-only HEAD -- "$DIR" 2>/dev/null
        git diff --cached --diff-filter=D --name-only -- "$DIR" 2>/dev/null
    } | grep -E '\.yaml$' | LC_ALL=C sort -u
)

deleted_objects=""
while IFS= read -r path; do
    [ -n "$path" ] || continue
    # Re-created since, or renamed back: not a deletion any more.
    [ -f "$path" ] && continue
    # The content as of the last commit that had it. For a working-tree
    # deletion that is HEAD; for a historical one it is the parent of
    # the commit that removed it.
    blob=""
    if git cat-file -e "HEAD:$path" 2>/dev/null; then
        blob=$(git show "HEAD:$path" 2>/dev/null)
    else
        sha=$(git log -m --first-parent --diff-filter=D --format=%H -n 1 -- "$path" 2>/dev/null)
        [ -n "$sha" ] && blob=$(git show "$sha^:$path" 2>/dev/null)
    fi
    [ -n "$blob" ] || continue
    # THE PARSE, WITH ITS ERRORS KEPT. The derivation cannot do this one
    # — it reads files in the tree, and this content exists only in git
    # history — so the parse stays here, and so does the obligation. With
    # `2>/dev/null` a deleted manifest that will not parse declared
    # NOTHING as far as this loop could tell, and property A then passed
    # it silently: the one direction of drift this check exists for,
    # reported clean because the evidence was discarded.
    if ! printf '%s\n' "$blob" | kubectl create --dry-run=client -o json -f - \
            > "$JSONBUF" 2> "$PARSEERR"; then
        fail "cannot parse the DELETED manifest $path as of the commit that removed it:"
        sed 's/^/    /' "$PARSEERR" >&2
        echo "  So nothing here knows what it declared, and a pass would mean 'could not look'." >&2
        continue
    fi
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        # Declared again by another file — a rename, a split, a move.
        is_declared "$line" && continue
        # NAMESPACE GOES LAST, and the file in the middle. Tab is IFS
        # WHITESPACE, so `read` collapses the two consecutive tabs a
        # cluster-scoped object's empty namespace produces and every
        # field after it shifts left — a deleted ClusterRole would be
        # looked up as `kubectl get ClusterRole <the file path>`. The
        # same trap, and the same fix, as the inventory in
        # check-manifests-applied.sh: a TRAILING empty field is
        # harmless, a middle one is not.
        kind="${line%%	*}"; rest="${line#*	}"
        ns="${rest%%	*}"; name="${rest#*	}"
        deleted_objects="${deleted_objects}${kind}	${name}	${path}	${ns}
"
    done <<EOF
$(objects_from_json "$JSONBUF")
EOF
done <<EOF
$deleted_paths
EOF

deleted_still_live=()
# The same objects as `Kind/ns/name`, for the de-duplication against
# property B below — matched on identity rather than by clipping a
# prefix off a sentence.
deleted_still_live_ids=()
while IFS=$'\t' read -r kind name path ns; do
    [ -n "${kind:-}" ] || continue
    # Trailing IFS whitespace is stripped, so a cluster-scoped object's
    # empty namespace leaves `ns` UNSET rather than empty.
    ns="${ns:-}"
    object_is_live "$kind" "$name" "$ns"
    case "$?" in
        0)
            deleted_still_live+=("$kind/$name${ns:+ -n $ns} (was declared in $path)")
            deleted_still_live_ids+=("$kind/$ns/$name")
            ;;
        1) ;; # the server says it is gone, which is the whole point
        *)
            unreadable=$((unreadable + 1))
            unreadable_names+=("$kind/$name${ns:+ (ns $ns)} — deleted in $path, not readable by this credential: $(err_line)")
            ;;
    esac
done <<EOF
$deleted_objects
EOF

if [ ${#deleted_still_live[@]} -gt 0 ]; then
    fail "${#deleted_still_live[@]} object(s) whose manifest the tree DELETED are still running:"
    printf '    %s\n' "${deleted_still_live[@]}" >&2
    echo "" >&2
    echo "  \`kubectl apply\` does not prune, so removing the file removed the" >&2
    echo "  DECLARATION and left the OBJECT. Nothing in the tree accounts for" >&2
    echo "  it now, and the next converge will not mention it." >&2
    echo "" >&2
    echo "  Delete each one by hand, naming it — this is the step the converge" >&2
    echo "  deliberately does not automate (see the header of this script for" >&2
    echo "  why \`--prune\` is refused):" >&2
    for o in "${deleted_still_live[@]}"; do
        echo "    kubectl delete ${o%% (was declared*}" >&2
    done
    echo "" >&2
    echo "  If it is meant to keep running, it needs a declaration: restore the" >&2
    echo "  manifest, or move the object into one that stays." >&2
fi

# ---------------------------------------------------------------------------
# Property B — nothing live is undeclared. ASKED, NOT RECOMPUTED.
# ---------------------------------------------------------------------------
# The whole question, answered by the one script that defines it: scope,
# excluded kinds, exemptions, the ownerReference rule and the UNVERIFIED
# accounting, none of them repeated here. stdout is `kind<TAB>ns<TAB>name`
# per undeclared object and EMPTY when there are none — so "clean" and
# "orphans found" share exit 0, and "I could not look" having a code of
# its own (CANNOT ANSWER = 4) is what makes reading this safe: without it
# `-ne 0` cannot tell a refusal from a finding, and `-eq 0` reads a
# refusal as a clean cluster.
#
# This is the SECOND call to the derivation in one run; `--declared`
# above parses the same manifests. Two passes is the price of one
# definition, and the converge is the only place that pays it.
orphan_rows=$("$DERIVE" --list 2>"$LISTERR")
list_rc=$?
# Its coverage line and its UNVERIFIED list are part of the answer on
# EVERY path, this one's failure included: a warning swallowed on the
# success path is the same defect one notch quieter, and a reader looking
# at a refusal is exactly the reader who needs to know how much of the
# scope was ever in reach.
sed 's/^/  /' "$LISTERR" >&2

if [ "$list_rc" -ne 0 ]; then
    # NOT A FINDING, and it must not read as one: no object is named, and
    # nothing is claimed about what is running. 4 is the derivation's own
    # CANNOT ANSWER; any other nonzero code is a fault in it, and is
    # reported the same way rather than guessed at.
    fail "CANNOT ANSWER — $DERIVE could not sweep the cluster (exit $list_rc; 4 is its CANNOT ANSWER):"
    echo "    its reason is quoted above." >&2
    echo "  No object has been shown to be undeclared and nothing is claimed about the cluster." >&2
else
    # Anything property A already reported is dropped, so one orphan
    # reads as one finding — A's message is the better one, because it
    # knows which file declared it.
    #
    # Split by hand, not with `IFS=$'\t' read`: tab is IFS WHITESPACE, so
    # `read` collapses the two consecutive tabs an empty namespace field
    # produces and shifts the name into `ns`. The derivation's `--list`
    # emits no such row today — its pairs require a namespace — but this
    # is the trap property A above is annotated for, and a reader must not
    # have to prove it cannot happen here.
    orphans=()
    while IFS= read -r row; do
        [ -n "$row" ] || continue
        kind="${row%%	*}"; rest="${row#*	}"
        ns="${rest%%	*}"; name="${rest#*	}"
        dup=0
        for d in ${deleted_still_live_ids+"${deleted_still_live_ids[@]}"}; do
            [ "$d" = "$kind/$ns/$name" ] && dup=1
        done
        [ "$dup" -eq 0 ] && orphans+=("$kind/$name (ns $ns)")
    done <<EOF
$orphan_rows
EOF

    if [ ${#orphans[@]} -gt 0 ]; then
        fail "${#orphans[@]} live object(s) that no manifest declares:"
        printf '    %s\n' "${orphans[@]}" >&2
        echo "" >&2
        echo "  Each is in a (kind, namespace) the tree manages and carries no" >&2
        echo "  ownerReference, so no controller made it from a declared parent." >&2
        echo "  One of three things is true, and the diff a reviewer reads should" >&2
        echo "  say which:" >&2
        echo "    1. its manifest was deleted and the object outlived it —" >&2
        echo "       \`git log --diff-filter=D -- $DIR\` and delete the object;" >&2
        echo "    2. it was applied by hand and never written down — land a" >&2
        echo "       manifest for it (hand-applied state is drift: $DIR/README.md);" >&2
        echo "    3. it is generated from sources already in the tree, like the" >&2
        echo "       step-plugins ConfigMap — add it to EXEMPT in $DERIVE with" >&2
        echo "       the reason, which is a decision and belongs in the diff." >&2
    fi
fi

# ---------------------------------------------------------------------------
# And the derivation's exemptions must not have gone stale.
# ---------------------------------------------------------------------------
# The one thing the sweep above cannot report, because an exemption is
# exactly what keeps an object out of it. Two directions, each of which
# leaves the exemption standing to excuse a future object of that name:
# the tree now DECLARES it, or the cluster does not HAVE it any more.
# `Kind/name` entries (no namespace) are the control plane's own objects
# in every namespace — kube-root-ca.crt, the default ServiceAccount —
# and neither direction is well posed for them: there is no namespace to
# ask about, and their ABSENCE would be the anomaly. So they are counted
# and stated rather than silently passed over.
exemptions=$("$DERIVE" --exemptions 2>"$PARSEERR") || {
    rc=$?
    fail "CANNOT ANSWER — $DERIVE --exemptions failed (exit $rc):"
    sed 's/^/    /' "$PARSEERR" >&2
}
any_ns_exemptions=0
while IFS= read -r e; do
    [ -n "$e" ] || continue
    case "$e" in
        */*/*) ;;
        */*) any_ns_exemptions=$((any_ns_exemptions + 1)); continue ;;
        *) fail "$DERIVE --exemptions printed \`$e\`, which is neither Kind/name nor Kind/ns/name"
           continue ;;
    esac
    kind="${e%%/*}"; rest="${e#*/}"; ns="${rest%%/*}"; name="${rest#*/}"
    stale=""
    if is_declared "$(printf '%s\t%s\t%s' "$kind" "$ns" "$name")"; then
        stale="the tree now declares it"
    else
        object_is_live "$kind" "$name" "$ns"
        case "$?" in
            0) ;;
            1) stale="the cluster does not have it" ;;
            *) unreadable=$((unreadable + 1))
               unreadable_names+=("$e — exempt in $DERIVE, and not readable by this credential: $(err_line)") ;;
        esac
    fi
    [ -n "$stale" ] || continue
    fail "the exemption for \`$e\` is stale — $stale"
    echo "  Drop it from EXEMPT in $DERIVE." >&2
done <<EOF
$exemptions
EOF

# ---------------------------------------------------------------------------
# The report. Partial coverage is stated, never rounded to clean — AND it
# is stated on the failing path too.
# ---------------------------------------------------------------------------
# A first draft printed the unverified list only on success, so a run
# that found one orphan dropped "and here are the six things I could not
# look at" — the record reduced before the reader saw it, which is the
# defect CLAUDE.md §Diagnosis describes under "quiet is not free". The
# person reading a finding is exactly the person who needs to know the
# finding might not be the only one.
report_unverified() { # stream
    [ "$unreadable" -gt 0 ] || return 0
    echo "  UNVERIFIED — $unreadable thing(s) this credential could not read, so they are not claimed clean:" >&"$1"
    printf '    %s\n' "${unreadable_names[@]}" >&"$1"
}

if [ "$problems" -ne 0 ]; then
    echo "" >&2
    report_unverified 2
    exit 1
fi

echo "a-deleted-manifest-leaves-no-object: OK — nothing the tree deleted is still running, $DERIVE found no undeclared object, and none of its exemptions has gone stale"
echo "  Coverage is the derivation's line above, and so are the scope, the excluded kinds and the exemptions (see its header, and this script's)."
[ "$any_ns_exemptions" -eq 0 ] \
    || echo "  not asserted: $any_ns_exemptions any-namespace exemption(s) — control-plane objects whose ABSENCE would be the anomaly, so staleness is not a question they answer"
report_unverified 1
exit 0
