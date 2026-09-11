#!/usr/bin/env bash
#
# a-deleted-manifest-leaves-no-object — deleting a manifest must delete
# the thing it declared. The converge cannot do that, so this is what
# says so out loud.
#
# WHY THIS EXISTS
# ---------------
# The converge runs `kubectl apply -f infra/cluster/manifests` with NO
# `--prune` (infra/forge/cluster-deploy-runner.sh, "apply manifests").
# Apply is additive: it creates and updates what the files name and has
# no opinion about anything else. So deleting a manifest removes the
# DECLARATION and leaves the OBJECT running, forever, with nothing in
# the tree accounting for it.
#
# `check-manifests-applied.sh` cannot see this by construction. It walks
# the tree and asks "is each declared object present?" — a question a
# deleted file is not in, and a question an orphan answers correctly by
# being absent from it. It is the observer for one direction of drift;
# this is the observer for the other.
#
# CLAUDE.md already records the mirror image: "an imperative cluster
# change has an expiry" — a `kubectl` change not in the manifests
# silently REVERTS at the next converge. This is the same seam read the
# other way: a manifest deletion silently does NOT take effect. Both
# leave the estate holding something the tree cannot account for, which
# is the exact drift the converge exists to prevent.
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
# B. NOTHING LIVE IS UNDECLARED, in the narrow slice where that question
#    is well posed. This is the sweep that catches an orphan whose
#    deletion predates this check, or one deleted by a path git cannot
#    see. It is only honest if it compares like with like, so the scope
#    is stated rather than assumed:
#
#    IN SCOPE — a (kind, namespace) pair is checked when
#      * the namespace is one the tree OWNS, meaning $DIR declares a
#        `Namespace` object for it (today: boss, boss-dev), and
#      * $DIR declares at least one object of that kind in it, and
#      * the kind is not in $EXCLUDED_KINDS below.
#
#    OUT OF SCOPE, each for a stated reason — never silently skipped:
#      * CLUSTER-SCOPED KINDS (ClusterRole, ClusterRoleBinding,
#        Namespace, StorageClass). The cluster holds hundreds of these
#        that belong to Talos, Cilium, cert-manager and Longhorn; the
#        tree declares four. Sweeping them would report the cluster's
#        own furniture as BOSS's orphans. Property A still covers a
#        cluster-scoped object the tree DELETED, which is the case that
#        matters.
#      * NAMESPACES THE TREE DOES NOT OWN. `boss-tls.yaml` puts two
#        one-shot Jobs into `cert-manager`, a namespace cert-manager
#        owns and fills with its own work. "The tree declares something
#        here" is not "the tree manages this namespace".
#      * KINDS THE TREE DECLARES NOTHING OF in that namespace — Pod,
#        ReplicaSet, Endpoints, EndpointSlice, ControllerRevision, Event
#        and the rest. There is no declared set to compare them against,
#        so every one of them would be a finding.
#      * OBJECTS WITH AN ownerReference. A controller made them from a
#        declared parent: CronJob -> Job -> Pod, Deployment ->
#        ReplicaSet. Deleting the parent's manifest is the declared
#        change; the children follow.
#      * $EXCLUDED_KINDS and the two exemption lists, below, each entry
#        carrying its reason.
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
# The CONVERGE is the run that has reach, and it is measured, not
# assumed: cluster-deploy-runner.sh's verify step reported
# `check-manifests-applied: 50 present, 0 missing, 0 drifted, 0
# unreadable (of 50)` in the journal on 2026-09-10, so bare `kubectl`
# plus the admin kubeconfig works in that unit's environment even
# though the probe path on the same host has none. Two true statements
# about one host; do not read either for the other.
#
# What it must never do is read "could not look" as "nothing to report"
# — a wrong target answers instead of erroring (CLAUDE.md §Doors) — so a
# pair it could not list is counted and NAMED as unverified, never as
# clean, and the summary says how much of the scope the run covered.
#
# Usage:  infra/lint/a-deleted-manifest-leaves-no-object.sh
#   KUBECONFIG  a credential that can read the managed namespaces. The
#               dev pod's session credential is narrow (it reads
#               Services, Deployments, CronJobs and StatefulSets in
#               `boss`, and ConfigMaps/Deployments/PVCs in `boss-dev`);
#               the converge's admin kubeconfig sees all of it.
#
# EXIT
#   0  no orphan in what it could read (or it could read nothing and
#      said so)
#   1  an object the tree does not account for is running, an exemption
#      here has gone stale, or the DECLARED SET could not be derived —
#      which is not a finding about the cluster and says so. A manifest
#      that will not parse lands here: every object it declares would
#      otherwise read as an orphan, so there is no answer to give.

set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

DIR="infra/cluster/manifests"

# Kinds excluded from property B's sweep, with the reason.
#
# Secret — `$DIR/README.md` makes it a rule that Secret OBJECTS are
# created out-of-band and stay out of tree; every manifest references
# them by name only. So the tree's declaration set for Secrets is
# incomplete BY DESIGN, and a sweep against it would report every
# legitimately out-of-tree secret as an orphan. The one Secret the tree
# does declare (`dev-session-token`) is still covered in the other
# direction by check-manifests-applied.sh, and by property A if it is
# ever deleted.
EXCLUDED_KINDS=(Secret)

# Objects the control plane creates in EVERY namespace, as `Kind/name`.
# No manifest will ever declare them and their absence would be the
# anomaly.
EXEMPT_ANY_NS=(
    "ConfigMap/kube-root-ca.crt"
    "ServiceAccount/default"
)

# Live objects in scope that no manifest declares, as `Kind/ns/name`,
# each with the reason it is tolerated. A NAMED SET, not a count, so
# adding one never edits a shared tail line (CLAUDE.md §9a, the
# BASELINE=<n> lesson).
#
# Both entries are the same shape: a ConfigMap GENERATED from sources
# that are already in the tree, where committing the derived artifact
# would be the second copy that drifts. Neither is an orphan; both are
# declared, just not as YAML.
EXEMPT=(
    # 72KB of JS built from infra/step-plugins/*.js. $DIR/README.md
    # names it under "what's deliberately not here".
    "ConfigMap/boss/step-plugins"
    # Built from infra/gate-runner/run.sh by
    # infra/gate-runner/apply-script-configmap.sh, which exists because
    # the gate Job cannot run run.sh out of the clone it is about to make.
    "ConfigMap/boss-dev/gate-runner-script"
)

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
# What it pins is the volumeClaimTemplates clause. If that silently stops
# deriving `pgdata-postgres-0`, this check reports the PVC holding the
# audit log as an undeclared orphan — a wolf cry on the most alarming
# object in the cluster, which is how a check gets demoted and then
# ignored (CLAUDE.md §Diagnosis).
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
READERR=$(mktemp) || exit 1
trap 'rm -f "$JSONBUF" "$PARSEERR" "$READERR"' EXIT

# --- what the tree declares -------------------------------------------------
# ASKED, NOT RECOMPUTED. This used to be a local `objects_in_files` that
# ran `kubectl create --dry-run=client` per file with `2>/dev/null` — so a
# manifest that would not parse was silently skipped, its objects fell out
# of the declared set, and the object it DECLARES was reported here as an
# orphan under advice to delete it. Measured (backlog 19aa75e0, fixture in
# crates/core/boss-testing/tests/undeclared_objects_sh.rs): one
# unparseable file made this print "1 live object(s) ... that no manifest
# declares: Service/svc-a" and exit 1, naming a declared object and never
# mentioning the file.
#
# `undeclared-objects.sh --declared` is the one definition of this
# question, and it refuses with the filename instead (exit 4, CANNOT
# ANSWER). Two copies of one derivation differ in exactly the places
# nobody compared (CLAUDE.md §9a), and this was that place.
DERIVE="infra/cluster/undeclared-objects.sh"
[ -x "$DERIVE" ] || {
    echo "a-deleted-manifest-leaves-no-object: $DERIVE is missing — it is where the declared set is defined." >&2
    exit 1
}
# kind<TAB>ns<TAB>name<TAB>file for EVERY manifest in the tree, converged
# or not; the file column is dropped here because this check compares
# identities.
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

declared_tree=$(printf '%s\n' "$declared_rows" \
    | LC_ALL=C awk -F'\t' 'NF >= 3 { print $1 "\t" $2 "\t" $3 }' \
    | grep -v '^[[:space:]]*$' | LC_ALL=C sort -u)
# Converged-only: the subset declared by a file under $DIR. `managed_ns`
# below must come from this and not from the gate-runner manifests, which
# declare objects into a namespace they do not own.
declared_converged=$(printf '%s\n' "$declared_rows" \
    | LC_ALL=C awk -F'\t' -v d="$DIR/" 'NF >= 4 && index($4, d) == 1 { print $1 "\t" $2 "\t" $3 }' \
    | grep -v '^[[:space:]]*$' | LC_ALL=C sort -u)

declared_count=$(printf '%s\n' "$declared_converged" | grep -c . || true)
if [ "$declared_count" -lt 20 ]; then
    fail "parsed only $declared_count object(s) from $DIR — the scrape broke, so a green result would mean nothing"
    exit 1
fi

# A `Kind<TAB>ns<TAB>name` line the tree declares?
is_declared() { printf '%s\n' "$declared_tree" | LC_ALL=C grep -qxF "$1"; }

# Exempted, by either list?
is_exempt() { # kind ns name
    local e
    for e in ${EXEMPT_ANY_NS+"${EXEMPT_ANY_NS[@]}"}; do
        [ "$e" = "$1/$3" ] && return 0
    done
    for e in ${EXEMPT+"${EXEMPT[@]}"}; do
        [ "$e" = "$1/$2/$3" ] && return 0
    done
    return 1
}

# An exemption for an object the tree now DECLARES is stale, and left
# standing it would excuse a future live object of that name without
# anyone deciding so — the same refusal gate.sh applies to a
# PREFLIGHT_EXCLUDES entry naming a lint that no longer exists, and
# the-live-rules-are-the-authored-rules.sh to a stale EXEMPT rule.
for e in ${EXEMPT+"${EXEMPT[@]}"}; do
    kind="${e%%/*}"; rest="${e#*/}"; ns="${rest%%/*}"; name="${rest#*/}"
    if is_declared "$(printf '%s\t%s\t%s' "$kind" "$ns" "$name")"; then
        fail "the exemption for \`$e\` is stale — the tree now declares it"
        echo "  Drop it from EXEMPT in this script." >&2
    fi
done

# --- the namespaces the tree OWNS ------------------------------------------
managed_ns=$(printf '%s\n' "$declared_converged" \
    | LC_ALL=C awk -F'\t' '$1 == "Namespace" { print $3 }' | LC_ALL=C sort -u)
if [ -z "$managed_ns" ]; then
    fail "$DIR declares no Namespace object — cannot tell which namespaces the tree owns"
    exit 1
fi

unreadable=0
unreadable_names=()
orphans=()
readable_pairs=""
live_seen=""

# List the live object names of one kind in one namespace, excluding
# anything a controller owns. Prints nothing and returns 1 when this
# credential cannot look — leaving the server's words in $READERR,
# because "Forbidden" and "connection refused" are different problems
# with different fixes and the reader of the UNVERIFIED line below is the
# one who has to tell them apart.
#
# A FILE, not a variable. The caller runs this inside `$(...)`, so an
# assignment here happens in a subshell and is gone by the time anything
# could read it — which is how the first attempt at keeping this reason
# produced an empty one.
live_names() { # kind ns
    local out rc
    out=$(kubectl get "$1" -n "$2" \
        -o 'jsonpath={range .items[*]}{.metadata.name}{"\t"}{.metadata.ownerReferences[0].kind}{"\n"}{end}' \
        --request-timeout=10s 2>&1)
    rc=$?
    if [ "$rc" -ne 0 ]; then
        printf '%s' "$out" | LC_ALL=C tr '\n\t' '  ' | LC_ALL=C sed 's/  */ /g; s/^ //; s/ $//' > "$READERR"
        [ -s "$READERR" ] || printf 'no output from kubectl (exit %s)' "$rc" > "$READERR"
        return 1
    fi
    printf '%s\n' "$out" | LC_ALL=C awk -F'\t' 'NF && $1 != "" && $2 == "" { print $1 }'
}

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
while IFS=$'\t' read -r kind name path ns; do
    [ -n "${kind:-}" ] || continue
    # Trailing IFS whitespace is stripped, so a cluster-scoped object's
    # empty namespace leaves `ns` UNSET rather than empty.
    ns="${ns:-}"
    args=()
    [ -n "$ns" ] && args=(-n "$ns")
    out=$(kubectl get "$kind" "$name" "${args[@]}" --request-timeout=10s 2>&1)
    rc=$?
    if [ "$rc" -eq 0 ]; then
        deleted_still_live+=("$kind/$name${ns:+ -n $ns} (was declared in $path)")
    elif printf '%s' "$out" | grep -qiE 'forbidden|cannot get|cannot list'; then
        unreadable=$((unreadable + 1))
        unreadable_names+=("$kind/$name${ns:+ (ns $ns)} — deleted in $path, not readable by this credential")
    fi
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
# Property B — nothing live is undeclared, in the scoped slice.
# ---------------------------------------------------------------------------
# The pairs: a kind the tree declares in a namespace the tree owns.
pairs=$(printf '%s\n' "$declared_converged" | LC_ALL=C awk -F'\t' -v mns="$managed_ns" '
    BEGIN { n = split(mns, a, "\n"); for (i = 1; i <= n; i++) if (a[i] != "") own[a[i]] = 1 }
    $2 != "" && ($2 in own) { print $1 "\t" $2 }
' | LC_ALL=C sort -u)

pairs_checked=0
pairs_total=0
out_of_scope_kinds=()
while IFS=$'\t' read -r kind ns; do
    [ -n "${kind:-}" ] || continue
    excluded=0
    for k in ${EXCLUDED_KINDS+"${EXCLUDED_KINDS[@]}"}; do
        [ "$k" = "$kind" ] && excluded=1
    done
    if [ "$excluded" -eq 1 ]; then
        out_of_scope_kinds+=("$kind in $ns")
        continue
    fi
    pairs_total=$((pairs_total + 1))
    if ! names=$(live_names "$kind" "$ns"); then
        unreadable=$((unreadable + 1))
        unreadable_names+=("$kind in $ns — not listable by this credential: $(cat "$READERR")")
        continue
    fi
    pairs_checked=$((pairs_checked + 1))
    readable_pairs="${readable_pairs}${kind}/${ns}
"
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        live_seen="${live_seen}${kind}/${ns}/${name}
"
        is_declared "$(printf '%s\t%s\t%s' "$kind" "$ns" "$name")" && continue
        is_exempt "$kind" "$ns" "$name" && continue
        orphans+=("$kind/$name (ns $ns)")
    done <<EOF
$names
EOF
done <<EOF
$pairs
EOF

# An exemption in the other direction: named here, and the object is not
# there. Same hazard as the stale exemption above — it would excuse the
# next object that happens to take the name. Only asserted for a pair
# this run could actually LIST, because "not seen" and "not looked at"
# are different answers and only one of them is a finding.
for e in ${EXEMPT+"${EXEMPT[@]}"}; do
    kind="${e%%/*}"; rest="${e#*/}"; ns="${rest%%/*}"
    printf '%s\n' "$readable_pairs" | LC_ALL=C grep -qxF "$kind/$ns" || continue
    printf '%s\n' "$live_seen" | LC_ALL=C grep -qxF "$e" && continue
    fail "the exemption for \`$e\` is stale — the cluster does not have it"
    echo "  Drop it from EXEMPT in this script." >&2
done

# Already reported by property A with a better message (it knows which
# file declared it); drop the duplicate so one orphan reads as one
# finding.
filtered_orphans=()
for o in ${orphans+"${orphans[@]}"}; do
    dup=0
    for d in ${deleted_still_live+"${deleted_still_live[@]}"}; do
        # "Kind/name (ns X)" vs "Kind/name -n X (was declared in ...)"
        [ "${d%% *}" = "${o%% *}" ] && dup=1
    done
    [ "$dup" -eq 0 ] && filtered_orphans+=("$o")
done

if [ ${#filtered_orphans[@]} -gt 0 ]; then
    fail "${#filtered_orphans[@]} live object(s) in the managed namespaces that no manifest declares:"
    printf '    %s\n' "${filtered_orphans[@]}" >&2
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
    echo "       step-plugins ConfigMap — add it to EXEMPT in this script with" >&2
    echo "       the reason, which is a decision and belongs in the diff." >&2
fi

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

echo "a-deleted-manifest-leaves-no-object: OK — no orphan in $pairs_checked of $pairs_total (kind, namespace) pair(s) across $(printf '%s\n' "$managed_ns" | tr '\n' ' ')"
echo "  out of scope by design: cluster-scoped kinds, namespaces the tree does not own, kinds the tree declares nothing of, controller-owned objects${EXCLUDED_KINDS+, $(printf '%s ' "${EXCLUDED_KINDS[@]}")}(see this script's header)"
report_unverified 1
exit 0
