#!/usr/bin/env bash
#
# undeclared-objects.sh — WHAT IS RUNNING THAT THE TREE DOES NOT DECLARE.
# One definition of that question, for every reader of it.
#
# WHY IT IS ITS OWN SCRIPT
# -----------------------
# The converge runs `kubectl apply -f infra/cluster/manifests` with NO
# `--prune`. Apply is additive: it creates and updates what the files
# name and has no opinion about anything else. So deleting a manifest
# removes the DECLARATION and leaves the OBJECT running, forever, with
# nothing in the tree accounting for it. `check-manifests-applied.sh`
# is the observer for the other direction — "is everything declared
# present?" — a question a deleted file is not in.
#
# Two programs want the answer to this one:
#
#   * the orphan LINT, which reports it so a human is told; and
#   * the `delete-orphan-object` ops verb
#     (infra/forge/delete-orphan-object.sh), which DERIVES ITS
#     AUTHORITY from it — it may delete a named object only if this
#     computation names that object.
#
# Two programs computing "what is orphaned" is exactly the defect
# CLAUDE.md §9a describes, and here it would be worse than drift: the
# verb's bound IS the computation, so a second copy of it is a second,
# unreviewed bound. Hence one script, three ways to ask it.
#
# THE SCOPE, STATED RATHER THAN ASSUMED
# -------------------------------------
# "Undeclared" is only well posed where the tree's declaration set is
# complete enough to compare against, so a (kind, namespace) pair is IN
# SCOPE when
#   * the namespace is one the tree OWNS — $DIR declares a `Namespace`
#     object for it (today: boss, boss-dev); and
#   * $DIR declares at least one object of that kind in it — or a
#     generateName TEMPLATE of that kind names it (the gate Job), in
#     which case a live object carrying the template's literal labels
#     is declared by it (THE TEMPLATE RULE, below; backlog 4438217e); and
#   * the kind is not in $EXCLUDED_KINDS below.
#
# AND ONE OBJECT AT A TIME, BY LABEL: an object carrying the tree's own
# mark, `app.kubernetes.io/part-of=boss`, of a kind the tree declares
# SOMEWHERE in an owned namespace, is in scope in every owned namespace
# — whether or not that namespace still declares the kind. This is the
# case the pair rule alone cannot see: THE LAST MANIFEST OF A KIND IN A
# NAMESPACE. Measured 2026-09-12: the seed-dir car deleted boss-dev's
# only CronJob (#341); the object stayed (apply does not prune), failed
# every minute against PodSecurity, and this derivation REFUSED to name
# it — "the tree declares no CronJob in boss-dev" — while `--list` did
# not list it. The pair rule exists so Pods, ReplicaSets and Events —
# kinds the tree declares nowhere — are never findings, and it keeps
# doing that: a labelled Pod is still out (Pod is declared nowhere), an
# UNlabelled CronJob in boss-dev is still out (nothing says it is ours).
# The label is the tree's own claim on the object, so acting on it is
# acting on a declaration, not a guess.
#
# OUT OF SCOPE, each for a stated reason — never silently skipped:
#   * CLUSTER-SCOPED KINDS (ClusterRole, ClusterRoleBinding, Namespace,
#     StorageClass). The cluster holds hundreds that belong to Talos,
#     Cilium, cert-manager and Longhorn; the tree declares four.
#     Sweeping them would report the cluster's own furniture as BOSS's
#     orphans.
#   * NAMESPACES THE TREE DOES NOT OWN. Until 2026-09-17 (21c17ebc)
#     `boss-tls.yaml` put two one-shot Jobs into `cert-manager`, which
#     cert-manager owns and fills with its own work. "The tree declares
#     something here" is not "the tree manages this namespace" — and
#     those two completed Jobs are exactly what this sweep will NOT
#     name now that their manifest is gone; property A of
#     a-deleted-manifest-leaves-no-object (the tree's own deletions,
#     read from git) is what does.
#   * KINDS THE TREE DECLARES NOTHING OF in that namespace — Pod,
#     ReplicaSet, Endpoints, EndpointSlice, ControllerRevision, Event.
#     There is no declared set to compare them against, so every one of
#     them would be a finding.
#   * OBJECTS WITH AN ownerReference. A controller made them from a
#     declared parent (Deployment -> ReplicaSet -> Pod, CronJob -> Job);
#     deleting the parent's manifest is the declared change and the
#     children follow.
#   * $EXCLUDED_KINDS and $EXEMPT below, each entry carrying its reason.
#
# A PARTIAL SCRAPE IS AN ERROR, NOT A SMALLER ANSWER. If a manifest
# fails to parse, every object it declares falls out of the declared set
# and looks undeclared — and for the verb that would mean deleting a
# DECLARED object. So a file `kubectl` cannot parse, a directory with
# implausibly few manifests, or a credential that cannot list a pair
# makes this script say "cannot answer" and exit 4. "Could not look" is
# never rounded to "nothing to report" (CLAUDE.md §Doors: a wrong
# target answers instead of erroring).
#
# CANNOT ANSWER HAS AN EXIT CODE OF ITS OWN (4), and that is the whole
# point of it. In `--list` a clean cluster and a cluster full of orphans
# BOTH exit 0 — the answer is on stdout, not in the status — so a
# consumer that reads only `-ne 0` cannot tell a refusal from a finding,
# and one that reads only `-eq 0` reads a refusal as "clean". Neither
# mistake is available once the refusal has its own number. 1 stays what
# it was for: this run asked nothing well posed (a bad mode, a missing
# argument). Every code other than 0 and 3 was already CANNOT ANSWER to
# the ops verb, so this sharpens the contract rather than breaking it.
#
# USAGE
#   undeclared-objects.sh --list
#       kind<TAB>ns<TAB>name for every in-scope live object no manifest
#       declares. Coverage notes on stderr. This is the orphan set.
#       exit 0 — answered; stdout IS the set, empty when there are none
#       exit 4 — cannot answer; stdout is empty and the reason is named
#   undeclared-objects.sh --check <Kind>/<namespace>/<name>
#       exit 0 — undeclared; prints `undeclared<TAB>kind<TAB>ns<TAB>name`
#       exit 3 — NO, with the test that refused it named on stderr
#       exit 4 — cannot answer, with the reason named
#   undeclared-objects.sh --declared
#       kind<TAB>ns<TAB>name<TAB>file for everything the tree declares.
#       exit 4 when any manifest would not parse: a declared set missing
#       a file is not a smaller declared set, it is no answer.
#   undeclared-objects.sh --exemptions
#   undeclared-objects.sh --exemptions-derived
#       the generated per-instance objects (ConfigMap/<ns>/boss-tenant
#       for every tenant_repo instance in instances.toml, and
#       ConfigMap/<ns>/boss-site for every one that declares a site) —
#       exempt when present, never stale when absent
#       the exemption entries, as `Kind/ns/name` or `Kind/name`.
#   undeclared-objects.sh --kubectl
#       the resolved kubectl argv, so a caller needing its own kubectl
#       call uses the same one rather than a second resolution.
#   undeclared-objects.sh --namespaces
#       the namespaces the tree OWNS, one per line, sorted — the first
#       clause of THE SCOPE above, and nothing else. For a caller whose
#       bound is "a namespace the tree declares" (reap-terminated-pods,
#       backlog 85889a52), so that bound is this derivation rather than
#       a second copy of it or a hand list. exit 4 when the manifests
#       will not parse or declare no Namespace: no owned set is not an
#       empty one.
#   undeclared-objects.sh --objects-of <kubectl-json-file>
#       kind<TAB>ns<TAB>name for every object ONE parsed manifest declares
#       (the JSON `kubectl create --dry-run=client -o json` prints for it),
#       volumeClaimTemplate PVCs included. No cluster, no tree: python3
#       only. This is the ONE definition of "what does a manifest declare"
#       — the deleted-manifest lint parses blobs that exist only in git
#       history, which this script cannot reach, and used to carry its own
#       copy of this parser for that reason (582cefe9).
#   exit 1 in any mode — a usage error. Nothing was computed.
#
# ENV
#   BOSS_CLUSTER_TREE  the tree to read manifests from (default: the
#                      repository this script lives in). A test seam,
#                      and the reason the fixtures in
#                      boss-testing/tests/delete_orphan_object_sh.rs can
#                      exercise this without a cluster. A packet cannot
#                      set it: the ops-runner passes no environment from
#                      the packet, only an argv built from the
#                      allowlist.
#   BOSS_KUBECTL       the kubectl command, whitespace-separated words
#                      (default: `kubectl`, else a docker wrapper —
#                      see resolve_kubectl).
#   KUBECONFIG         a credential that can read the managed
#                      namespaces. The converge's admin kubeconfig can;
#                      the dev pod's session credential reads only part
#                      of it and the unreadable part is NAMED.

set -uo pipefail

ME="undeclared-objects"
say() { echo "$ME: $*" >&2; }

# "I could not look" — never an answer, and never the same number as one.
# See the header: in --list both answers are exit 0, so a refusal that
# shares a code with either is a refusal a consumer cannot read.
CANNOT_ANSWER=4
cannot_answer() { # reason-lines...
    local line
    for line in "$@"; do say "$line"; done
    exit "$CANNOT_ANSWER"
}

TREE="${BOSS_CLUSTER_TREE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
DIR="$TREE/infra/cluster/manifests"

# Kinds out of the sweep, with the reason.
#
# Secret — `$DIR/README.md` makes it a rule that Secret OBJECTS are
# created out of band and stay out of tree; every manifest references
# them by name only. So the tree's declaration set for Secrets is
# incomplete BY DESIGN, and a sweep against it would call every
# legitimately out-of-tree secret an orphan.
EXCLUDED_KINDS=(Secret)

# Objects the control plane creates in EVERY namespace, as `Kind/name`.
# No manifest will ever declare them and their absence would be the
# anomaly.
EXEMPT_ANY_NS=(
    "ConfigMap/kube-root-ca.crt"
    "ServiceAccount/default"
)

# In-scope live objects no manifest declares, as `Kind/ns/name`, each
# with the reason it is tolerated. A NAMED SET, not a count, so adding
# one never edits a shared tail line (CLAUDE.md §9a).
#
# Both entries are the same shape: a ConfigMap GENERATED from sources
# already in the tree, where committing the derived artifact would be
# the second copy that drifts. Neither is an orphan; both are declared,
# just not as YAML.
EXEMPT=(
    # 72KB of JS built from infra/step-plugins/*.js. $DIR/README.md
    # names it under "what's deliberately not here".
    "ConfigMap/boss/step-plugins"
    # Built from infra/gate-runner/run.sh by
    # infra/gate-runner/apply-script-configmap.sh, which exists because
    # the gate Job cannot run run.sh out of the clone it is about to make.
    "ConfigMap/boss-dev/gate-runner-script"
)

# DERIVED EXEMPTIONS — the objects the converge GENERATES per instance,
# read off the instance list rather than typed here. A repo-sourced
# instance (`tenant_repo` in infra/cluster/instances.toml, f4f5c387)
# gets its tenant delivered as ConfigMap/<namespace>/boss-tenant; no
# manifest declares it, and it is absent whenever the converge skipped
# that instance (its source unreadable), so unlike $EXEMPT above it is
# exempt WHEN PRESENT and never stale when absent. Measured 2026-09-16
# 23:55Z: the first converge after the prod flip applied and rolled prod,
# then failed its own orphan check on ConfigMap/boss/boss-tenant
# (d7d23650) — a hand entry could not be added earlier because the lint
# refuses an exemption for an object the cluster does not hold yet. Read
# with awk the way render-instance.sh reads the file (one key per line).
# An instance that also declares a `site` (design b64c4377) gets its
# site delivered the same way, as ConfigMap/<namespace>/boss-site.
derived_exemptions() {
    local f="$TREE/infra/cluster/instances.toml"
    [ -f "$f" ] || return 0
    awk '
        function flush() {
            if (ns != "" && repo != "") {
                print "ConfigMap/" ns "/boss-tenant"
                if (site != "") print "ConfigMap/" ns "/boss-site"
            }
            ns = ""; repo = ""; site = ""
        }
        /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
        /^\[/ { flush(); next }
        $1 == "namespace"   { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/"/, ""); ns = $0; next }
        $1 == "tenant_repo" { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/"/, ""); repo = $0; next }
        $1 == "site"        { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/"/, ""); site = $0; next }
        END { flush() }
    ' "$f"
}

# --- the kubectl this run uses ---------------------------------------------
# ONE resolution, shared with callers through `--kubectl`, because a
# second resolution is a second set of assumptions about a host.
#
# The forge host is the measured case and it is not simple: the converge
# unit runs a bare `kubectl` with an admin kubeconfig at
# /home/david/kc.yaml (journal, 2026-09-10: `check-manifests-applied: 49
# present`), while a probe run as david through a minimal environment
# finds none (infra/forge/host-absent-tools.txt, 2026-09-09) and the
# converge's own apply goes through `docker run alpine/k8s`. Two true
# statements about one host — so try the binary, then the container, and
# say which when neither is there.
resolve_kubectl() {
    if [ -n "${BOSS_KUBECTL:-}" ]; then
        printf '%s\n' "$BOSS_KUBECTL"
        return 0
    fi
    local kc="${KUBECONFIG:-${BOSS_FORGE_KUBECONFIG:-/home/david/kc.yaml}}"
    case "$kc" in
        *[[:space:]]*)
            say "the kubeconfig path \`$kc\` contains whitespace; name one without it"
            return 1
            ;;
    esac
    if command -v kubectl >/dev/null 2>&1; then
        # KUBECONFIG already set governs on its own; otherwise name the
        # file on the command line, because the ops-runner hands a verb
        # no HOME and therefore no ~/.kube/config.
        if [ -n "${KUBECONFIG:-}" ] || [ ! -f "$kc" ]; then
            printf 'kubectl\n'
        else
            printf 'kubectl --kubeconfig=%s\n' "$kc"
        fi
        return 0
    fi
    if command -v docker >/dev/null 2>&1 && [ -f "$kc" ]; then
        # The shape the converge uses, plus the tree mounted at its own
        # path so `-f <file>` means the same thing inside and out.
        printf 'docker run --rm --network host -v %s:/kc:ro -v %s:%s:ro -w %s alpine/k8s:1.33.3 kubectl --kubeconfig=/kc\n' \
            "$kc" "$TREE" "$TREE" "$TREE"
        return 0
    fi
    say "no kubectl on PATH and no docker+kubeconfig to run one in (looked for $kc)"
    say "  This is 'cannot answer', not 'nothing to report'."
    return 1
}

MODE=""
TARGET=""
# kind<TAB>ns<TAB>name<TAB>file for every object in one manifest's JSON.
#
# A StatefulSet also declares the PVCs its volumeClaimTemplates create —
# `<template>-<set>-<ordinal>` — and those PVCs carry no ownerReference,
# so without this clause they look exactly like orphans. The tree DOES
# declare pgdata-postgres-0; it just spells it as a template. Getting
# this wrong reports the PVC holding the audit log as an orphan.
#
# The JSON arrives in a FILE, not on stdin: the python body itself is
# this function's stdin, so a script that also read stdin would read an
# empty string and report a clean cluster.
#
# With a third argument `templates` it prints the OTHER half instead:
# kind<TAB>ns<TAB>selector<TAB>file for every generateName template —
# see THE TEMPLATE RULE below.
objects_from_json() { # json-file source-file [templates]
    python3 - "$1" "$2" "${3:-}" <<'PY'
import json, sys

dec = json.JSONDecoder()
src = sys.argv[2]
templates = sys.argv[3] == "templates"
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
    if templates:
        # A generateName template declares no specific object, but it
        # does declare its kind here and the labels its objects carry.
        # Only LITERAL labels can select what it stamped; a `$PLACEHOLDER`
        # is filled per launch. No literal label, no template.
        if kind and not name and md.get("generateName"):
            labels = md.get("labels") or {}
            sel = ",".join(f"{k}={v}" for k, v in sorted(labels.items())
                           if "$" not in k and "$" not in str(v))
            if sel:
                print(f"{kind}\t{ns}\t{sel}\t{src}")
        continue
    # No name means a generateName template (the gate Job): it declares
    # no specific object, so it can neither be found nor be missed — its
    # kind and labels are read by the `templates` pass instead.
    if not kind or not name:
        continue
    print(f"{kind}\t{ns}\t{name}\t{src}")
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
                print(f"PersistentVolumeClaim\t{ns}\t{tn}-{name}-{ordinal}\t{src} (volumeClaimTemplate)")
PY
}

case "${1:-}" in
    --list|--declared|--exemptions|--exemptions-derived|--kubectl|--namespaces) MODE="$1" ;;
    --objects-of)
        MODE="--objects-of"
        TARGET="${2:-}"
        [ -n "$TARGET" ] || { say "--objects-of needs <kubectl-json-file>"; exit 1; }
        [ -f "$TARGET" ] || { say "--objects-of: no such file: $TARGET"; exit 1; }
        ;;
    --check)
        MODE="--check"
        TARGET="${2:-}"
        [ -n "$TARGET" ] || { say "--check needs <Kind>/<namespace>/<name>"; exit 1; }
        ;;
    *)
        say "usage: $ME --list | --check <Kind>/<ns>/<name> | --declared | --exemptions | --exemptions-derived | --kubectl | --namespaces | --objects-of <json>"
        exit 1
        ;;
esac

if [ "$MODE" = "--objects-of" ]; then
    # Three columns, not four: the caller names the source, this only
    # says what the JSON declares. Needs python3 and nothing else — no
    # tree, no cluster — so it runs from a copy of this file anywhere.
    command -v python3 >/dev/null 2>&1 \
        || cannot_answer "python3 is not on this box — cannot read the manifest"
    objects_from_json "$TARGET" "" | cut -f1-3
    exit 0
fi

if [ "$MODE" = "--exemptions" ]; then
    printf '%s\n' "${EXEMPT_ANY_NS[@]}" "${EXEMPT[@]}"
    exit 0
fi
if [ "$MODE" = "--exemptions-derived" ]; then
    derived_exemptions
    exit 0
fi

KUBECTL_LINE=$(resolve_kubectl) || exit "$CANNOT_ANSWER"
read -r -a KUBECTL <<<"$KUBECTL_LINE"
if [ "$MODE" = "--kubectl" ]; then
    printf '%s\n' "$KUBECTL_LINE"
    exit 0
fi

[ -d "$DIR" ] || cannot_answer "$DIR does not exist"
command -v python3 >/dev/null 2>&1 \
    || cannot_answer "python3 is not on this box — cannot read the manifests"

TMP=$(mktemp -d) || cannot_answer "cannot make a scratch directory to parse into"
trap 'rm -rf "$TMP"' EXIT

shopt -s nullglob
MANIFESTS=("$DIR"/*.yaml)
# Every other Kubernetes manifest in the tree. These are applied by
# something other than the converge (the gate runner applies its own PVC
# and Job), so an object they declare IS declared — just not converged.
# Reading them is what keeps `gate-runner-disk` from reading as an
# orphan, and it collapses two more exemption lines.
OTHER_MANIFESTS=("$TREE"/infra/gate-runner/*.yaml)
shopt -u nullglob

if [ "${#MANIFESTS[@]}" -lt 10 ]; then
    cannot_answer \
        "found only ${#MANIFESTS[@]} manifest(s) in $DIR — the scrape broke" \
        "  Refusing rather than reporting every live object as undeclared."
fi


# One manifest file's objects, via kubectl's own parser rather than a
# YAML implementation grown here — the same choice
# check-manifests-applied.sh makes. A file kubectl cannot parse is a
# HARD failure: its objects would silently drop out of the declared set.
objects_in_file() { # path
    local f="$1" rel
    rel="${f#"$TREE"/}"
    if ! "${KUBECTL[@]}" create --dry-run=client -o json -f "$f" \
            > "$TMP/parse.json" 2> "$TMP/parse.err"; then
        say "cannot parse $rel — refusing to derive a declaration set that is missing it:"
        sed 's/^/    /' "$TMP/parse.err" >&2
        return 1
    fi
    objects_from_json "$TMP/parse.json" "$rel" || return 1
    objects_from_json "$TMP/parse.json" "$rel" templates >> "$TMP/templates"
}
: > "$TMP/templates"

# --- what the tree declares ------------------------------------------------
declared_converged="$TMP/declared-converged"
declared_all="$TMP/declared-all"
: > "$declared_converged"
for f in "${MANIFESTS[@]}"; do
    # The refusal is `objects_in_file`'s, already printed with the
    # offending file and kubectl's own words; this only carries it out
    # as CANNOT ANSWER rather than letting a short declared set stand.
    objects_in_file "$f" >> "$declared_converged" || exit "$CANNOT_ANSWER"
done
cp "$declared_converged" "$declared_all"
for f in ${OTHER_MANIFESTS+"${OTHER_MANIFESTS[@]}"}; do
    objects_in_file "$f" >> "$declared_all" || exit "$CANNOT_ANSWER"
done

declared_count=$(grep -c . "$declared_converged" || true)
if [ "$declared_count" -lt 20 ]; then
    cannot_answer \
        "parsed only $declared_count object(s) from $DIR — the scrape broke, so any answer would mean nothing"
fi

if [ "$MODE" = "--declared" ]; then
    LC_ALL=C sort -u "$declared_all"
    exit 0
fi

# The file that declares `Kind<TAB>ns<TAB>name`, or empty.
declaring_file() { # kind ns name
    LC_ALL=C awk -F'\t' -v k="$1" -v n="$2" -v m="$3" \
        '$1 == k && $2 == n && $3 == m { print $4; exit }' "$declared_all"
}

is_exempt() { # kind ns name
    local e
    for e in ${EXEMPT_ANY_NS+"${EXEMPT_ANY_NS[@]}"}; do
        [ "$e" = "$1/$3" ] && return 0
    done
    for e in ${EXEMPT+"${EXEMPT[@]}"}; do
        [ "$e" = "$1/$2/$3" ] && return 0
    done
    for e in $DERIVED_EXEMPT; do
        [ "$e" = "$1/$2/$3" ] && return 0
    done
    return 1
}
DERIVED_EXEMPT=$(derived_exemptions)

is_excluded_kind() { # kind
    local k
    for k in ${EXCLUDED_KINDS+"${EXCLUDED_KINDS[@]}"}; do
        [ "$k" = "$1" ] && return 0
    done
    return 1
}

# --- the namespaces the tree owns ------------------------------------------
# EACH SET BELOW IS ONE ARRAY, AND ITS MEMBERSHIP TEST IS ONE LOOP OVER
# THAT ARRAY. Until 2026-09-19 each was a newline-joined string tested
# with `printf '%s\n' "$set" | grep -qxF` — and bash's printf leaves a
# multi-line variable in several write() calls, `grep -q` exits at its
# match, the next write is SIGPIPE, and under pipefail the pipeline
# answers "not a member" for a member that IS there. `boss` sorts first
# of the two owned namespaces, so the match came on the first write and
# the second was the one killed: `--check Service/boss/…` was REFUSED as
# "the tree does not own namespace boss" in the same line that listed
# "Owned: boss boss-dev" (backlog 0f2ecbda; once in five full-suite runs
# under load). Nothing here pipes a set into a reader any more.
in_set() { # needle member...
    local needle="$1" e
    shift
    for e in "$@"; do
        [ "$e" = "$needle" ] && return 0
    done
    return 1
}
mapfile -t managed_ns < <(LC_ALL=C awk -F'\t' '$1 == "Namespace" { print $3 }' "$declared_converged" | LC_ALL=C sort -u)
if [ "${#managed_ns[@]}" -eq 0 ]; then
    cannot_answer "$DIR declares no Namespace object — cannot tell which namespaces the tree owns"
fi
owns_ns() { in_set "$1" "${managed_ns[@]}"; }
if [ "$MODE" = "--namespaces" ]; then
    printf '%s\n' "${managed_ns[@]}"
    exit 0
fi

# (kind, namespace) pairs in scope: a kind the tree declares in a
# namespace the tree owns.
mapfile -t pairs < <(LC_ALL=C awk -F'\t' -v mns="$(printf '%s\n' "${managed_ns[@]}")" '
    BEGIN { n = split(mns, a, "\n"); for (i = 1; i <= n; i++) if (a[i] != "") own[a[i]] = 1 }
    $2 != "" && ($2 in own) { print $1 "\t" $2 }
' "$declared_converged" | LC_ALL=C sort -u)

# THE TEMPLATE RULE (backlog 4438217e). A generateName template — the
# gate Job, infra/gate-runner/gate-runner.yaml — declares no specific
# object, so until 2026-09-26 it put no pair in scope, and the tree held
# no other Job in boss-dev: a hand-made `Job/boss-dev/seed-dir-probe-2`
# (2026-09-12, no owner, no ttlSecondsAfterFinished) was REFUSED by
# `--check` as "the tree declares no Job in boss-dev", so neither the
# lint nor `delete-orphan-object` could ever name it. A template DOES
# declare its kind in its namespace, and the objects it stamps out are
# the ones carrying its LITERAL labels (`app=gate-runner`; the per-launch
# `$PLACEHOLDER` labels select nothing). So its (kind, ns) pair is in
# scope, and a live object matching its selector is declared by it — 285
# gate Jobs on 2026-09-26, none a finding. It declares its kind in ITS
# namespace only: it adds nothing to the label rule's declared-somewhere
# kinds, because a template is a claim on what it stamped, not on the
# kind everywhere. Templates are read from every manifest this script
# reads, converged or not, like the objects in $declared_all.
mapfile -t template_pairs < <(LC_ALL=C awk -F'\t' -v mns="$(printf '%s\n' "${managed_ns[@]}")" '
    BEGIN { n = split(mns, a, "\n"); for (i = 1; i <= n; i++) if (a[i] != "") own[a[i]] = 1 }
    $2 != "" && ($2 in own) { print $1 "\t" $2 }
' "$TMP/templates" | LC_ALL=C sort -u | LC_ALL=C comm -23 - <(printf '%s\n' ${pairs[@]+"${pairs[@]}"} | LC_ALL=C sort -u))
declares_pair() {
    in_set "$(printf '%s\t%s' "$1" "$2")" ${pairs[@]+"${pairs[@]}"} ${template_pairs[@]+"${template_pairs[@]}"}
}
# selector<TAB>file for every template of one (kind, ns).
templates_of() { # kind ns
    LC_ALL=C awk -F'\t' -v k="$1" -v n="$2" '$1 == k && $2 == n { print $3 "\t" $4 }' "$TMP/templates"
}

# The tree's own mark on what it creates; every manifest under $DIR
# carries it. An object without it in an undeclared pair is not ours to
# call undeclared.
BOSS_LABEL="app.kubernetes.io/part-of=boss"
# Kinds the tree declares in SOME owned namespace — the kinds that can
# be in scope by label where a namespace no longer declares them.
mapfile -t declared_kinds < <(printf '%s\n' ${pairs[@]+"${pairs[@]}"} | LC_ALL=C awk -F'\t' '$1 != "" { print $1 }' | LC_ALL=C sort -u)
declares_kind_somewhere() { in_set "$1" ${declared_kinds[@]+"${declared_kinds[@]}"}; }
# (kind, namespace) pairs in scope BY LABEL ONLY: a declared-somewhere
# kind in an owned namespace that does not itself declare the kind.
label_pairs=$(for k in ${declared_kinds[@]+"${declared_kinds[@]}"}; do for ns in "${managed_ns[@]}"; do
    printf '%s\t%s\n' "$k" "$ns"
done; done | LC_ALL=C sort -u | LC_ALL=C comm -23 - <(printf '%s\n' ${pairs[@]+"${pairs[@]}"} | LC_ALL=C sort -u))

# --- the cluster ----------------------------------------------------------
# Live names of one kind in one namespace, excluding anything a
# controller owns. Prints nothing and returns 1 when this credential
# cannot look — which is an error, never an empty answer.
live_names() { # kind ns
    local out
    if ! out=$("${KUBECTL[@]}" get "$1" -n "$2" \
            -o 'jsonpath={range .items[*]}{.metadata.name}{"\t"}{.metadata.ownerReferences[0].kind}{"\n"}{end}' \
            --request-timeout=10s 2>"$TMP/get.err"); then
        return 1
    fi
    printf '%s\n' "$out" | LC_ALL=C awk -F'\t' 'NF && $1 != "" && $2 == "" { print $1 }'
}
# The same, restricted to objects carrying the tree's label — the read
# for a pair in scope by label only.
live_labelled_names() { # kind ns
    local out
    if ! out=$("${KUBECTL[@]}" get "$1" -n "$2" -l "$BOSS_LABEL" \
            -o 'jsonpath={range .items[*]}{.metadata.name}{"\t"}{.metadata.ownerReferences[0].kind}{"\n"}{end}' \
            --request-timeout=10s 2>"$TMP/get.err"); then
        return 1
    fi
    printf '%s\n' "$out" | LC_ALL=C awk -F'\t' 'NF && $1 != "" && $2 == "" { print $1 }'
}
# Live names matching one label selector, owner or not — what a template
# stamped. Returns 1 when this credential cannot look.
live_selected_names() { # kind ns selector
    local out
    if ! out=$("${KUBECTL[@]}" get "$1" -n "$2" -l "$3" \
            -o 'jsonpath={range .items[*]}{.metadata.name}{"\n"}{end}' \
            --request-timeout=10s 2>"$TMP/get.err"); then
        return 1
    fi
    printf '%s\n' "$out" | LC_ALL=C awk -F'\t' 'NF && $1 != "" { print $1 }'
}
# The names a template of this (kind, ns) declares, one per line, and
# the selector<TAB>file that declares each as `name<TAB>selector<TAB>file`.
# Returns 1 when any selector cannot be read: a template's objects that
# could not be looked up are not undeclared, they are unknown.
template_names() { # kind ns
    local sel file names
    while IFS=$'\t' read -r sel file; do
        [ -n "$sel" ] || continue
        names=$(live_selected_names "$1" "$2" "$sel") || return 1
        [ -n "$names" ] || continue
        printf '%s\n' "$names" | LC_ALL=C awk -v s="$sel" -v f="$file" '{ print $0 "\t" s "\t" f }'
    done <<EOF
$(templates_of "$1" "$2")
EOF
}

# One object's part-of label, or empty. Returns 1 when the read failed.
label_of() { # kind ns name
    "${KUBECTL[@]}" get "$1" "$3" -n "$2" \
        -o 'jsonpath={.metadata.labels.app\.kubernetes\.io/part-of}' --request-timeout=10s 2>"$TMP/label.err"
}

# The text of the last failed read, as one line. $TMP/get.err is ONE
# file and the next pair overwrites it, so a reason not copied out at the
# call site is a reason nobody will ever read — the reduction-before-
# storing failure CLAUDE.md §Diagnosis names, applied to the only
# evidence that an answer is incomplete.
read_error() { # err-file
    [ -s "$1" ] || { printf 'no output from kubectl\n'; return 0; }
    LC_ALL=C tr '\n\t' '  ' < "$1" | LC_ALL=C sed 's/  */ /g; s/^ //; s/ $//'
}

# The ownerReference kind of one object, or empty. Returns 1 if the read
# failed — which is "not there" ONLY when the server said so; its own
# error file, because the caller compares its text against NotFound and
# $TMP/get.err belongs to the listing that ran before it.
owner_of() { # kind ns name
    "${KUBECTL[@]}" get "$1" "$3" -n "$2" \
        -o 'jsonpath={.metadata.ownerReferences[0].kind}' --request-timeout=10s 2>"$TMP/owner.err"
}

# ---------------------------------------------------------------------------
# --check: one object, and the name of whatever refuses it.
# ---------------------------------------------------------------------------
if [ "$MODE" = "--check" ]; then
    if ! [[ "$TARGET" =~ ^[A-Z][A-Za-z0-9]{0,62}/[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?/[a-z0-9]([a-z0-9.-]{0,251}[a-z0-9])?$ ]]; then
        say "REFUSED \`$TARGET\` — it is not <Kind>/<namespace>/<name> (Kind capitalised, the other two DNS labels)"
        exit 3
    fi
    kind="${TARGET%%/*}"; rest="${TARGET#*/}"; ns="${rest%%/*}"; name="${rest#*/}"

    if ! owns_ns "$ns"; then
        say "REFUSED $kind/$ns/$name — the tree does not own namespace \`$ns\` (no Namespace manifest in ${DIR#"$TREE"/}),"
        say "  so nothing here can be called undeclared. Owned: ${managed_ns[*]}"
        exit 3
    fi
    if is_excluded_kind "$kind"; then
        say "REFUSED $kind/$ns/$name — $kind is excluded from this derivation by design (see EXCLUDED_KINDS in $ME):"
        say "  the tree's declaration set for it is incomplete on purpose, so 'undeclared' means nothing here."
        exit 3
    fi
    in_scope_by_label=""
    if ! declares_pair "$kind" "$ns"; then
        if ! declares_kind_somewhere "$kind"; then
            say "REFUSED $kind/$ns/$name — the tree declares no $kind in \`$ns\`, nor anywhere it owns, so there is no declared set to compare against."
            exit 3
        fi
        # The pair is gone; the object may still be ours by label.
        if ! label=$(label_of "$kind" "$ns" "$name"); then
            if LC_ALL=C grep -q 'NotFound' "$TMP/label.err"; then
                say "$kind/$ns/$name is NOT LIVE — the tree declares no $kind in \`$ns\` and the cluster has no such object."
                exit 3
            fi
            say "CANNOT ANSWER for $kind/$ns/$name — this credential cannot read it to see whether it carries $BOSS_LABEL:"
            sed 's/^/    /' "$TMP/label.err" >&2
            exit "$CANNOT_ANSWER"
        fi
        if [ "$label" != "${BOSS_LABEL#*=}" ]; then
            say "REFUSED $kind/$ns/$name — the tree declares no $kind in \`$ns\`, so there is no declared set to compare against,"
            say "  and the object does not carry $BOSS_LABEL (it has part-of='${label:-<none>}'), so nothing says it is ours."
            exit 3
        fi
        in_scope_by_label="yes"
    fi
    if file=$(declaring_file "$kind" "$ns" "$name"); [ -n "$file" ]; then
        say "REFUSED $kind/$ns/$name — the tree DECLARES it, in $file."
        exit 3
    fi
    if is_exempt "$kind" "$ns" "$name"; then
        say "REFUSED $kind/$ns/$name — it is EXEMPT in $ME (generated from sources already in the tree)."
        exit 3
    fi
    if [ -n "$(templates_of "$kind" "$ns")" ]; then
        if ! stamped=$(template_names "$kind" "$ns"); then
            say "CANNOT ANSWER for $kind/$ns/$name — this credential cannot list the $kind a template in the tree stamps in \`$ns\`:"
            sed 's/^/    /' "$TMP/get.err" >&2
            exit "$CANNOT_ANSWER"
        fi
        hit=$(LC_ALL=C awk -F'\t' -v n="$name" '$1 == n { print $2 " (" $3 ")"; exit }' <<<"$stamped")
        if [ -n "$hit" ]; then
            say "REFUSED $kind/$ns/$name — the tree DECLARES it by template: it carries $hit,"
            say "  the literal labels of a generateName $kind template."
            exit 3
        fi
    fi
    if ! names=$(live_names "$kind" "$ns"); then
        say "CANNOT ANSWER for $kind/$ns/$name — this credential cannot list $kind in \`$ns\`:"
        sed 's/^/    /' "$TMP/get.err" >&2
        say "  'could not look' is not 'undeclared'. Point KUBECONFIG at a credential that can read it."
        exit "$CANNOT_ANSWER"
    fi
    if ! LC_ALL=C grep -qxF "$name" <<<"$names"; then
        if owner=$(owner_of "$kind" "$ns" "$name"); then
            if [ -n "$owner" ]; then
                say "REFUSED $kind/$ns/$name — a controller owns it (ownerReferences[0].kind=$owner)."
                say "  Deleting the parent's manifest is the declared change; the child follows."
                exit 3
            fi
            say "CANNOT ANSWER for $kind/$ns/$name — it exists but did not appear in the listing of $kind in \`$ns\`."
            exit "$CANNOT_ANSWER"
        fi
        # A FAILED READ IS NOT AN ABSENT OBJECT. Only the server saying
        # NotFound means "not there"; Forbidden, a timeout or a TLS error
        # mean this credential cannot tell, and reporting those as "it is
        # not live" states a fact about the cluster from evidence about
        # the credential.
        owner_err=$(read_error "$TMP/owner.err")
        if ! LC_ALL=C grep -qiE 'notfound|not found' <<<"$owner_err"; then
            say "CANNOT ANSWER for $kind/$ns/$name — it was not in the listing and it cannot be read either:"
            say "    $owner_err"
            say "  'could not read it' is not 'it is not there'. Point KUBECONFIG at a credential that can."
            exit "$CANNOT_ANSWER"
        fi
        say "REFUSED $kind/$ns/$name — it is not live: no $kind \`$name\` in \`$ns\` ($owner_err)."
        say "  'Not found' is not 'orphaned', and a delete of it would be a no-op reported as work."
        exit 3
    fi
    printf 'undeclared\t%s\t%s\t%s\n' "$kind" "$ns" "$name"
    if [ -n "$in_scope_by_label" ]; then
        say "$kind/$ns/$name is live in a namespace the tree owns, in scope by label ($BOSS_LABEL — the tree"
        say "  declares $kind elsewhere but no longer in \`$ns\`), carries no ownerReference, and NO manifest in the"
        say "  tree declares it. Derived from $declared_count declared object(s)."
    else
        say "$kind/$ns/$name is live in a namespace the tree owns, of a kind the tree declares there, carries no"
        say "  ownerReference, and NO manifest in the tree declares it. Derived from $declared_count declared object(s)."
    fi
    exit 0
fi

# ---------------------------------------------------------------------------
# --list: the whole orphan set.
# ---------------------------------------------------------------------------
unreadable=0
unreadable_names=()
pairs_checked=0
pairs_total=0
excluded_pairs=()
orphans=()
while IFS=$'\t' read -r kind ns; do
    [ -n "${kind:-}" ] || continue
    if is_excluded_kind "$kind"; then
        excluded_pairs+=("$kind in $ns")
        continue
    fi
    pairs_total=$((pairs_total + 1))
    if ! names=$(live_names "$kind" "$ns"); then
        unreadable=$((unreadable + 1))
        # The server's OWN WORDS, copied out now. A count of pairs is a
        # symptom; "Forbidden" and "connection refused" are different
        # problems with different fixes, and the reader of the UNVERIFIED
        # line is the one who has to tell them apart.
        unreadable_names+=("$kind in $ns — not listable by this credential: $(read_error "$TMP/get.err")")
        continue
    fi
    stamped=""
    if [ -n "$(templates_of "$kind" "$ns")" ] && ! stamped=$(template_names "$kind" "$ns"); then
        unreadable=$((unreadable + 1))
        unreadable_names+=("$kind in $ns (by template) — not listable by this credential: $(read_error "$TMP/get.err")")
        continue
    fi
    pairs_checked=$((pairs_checked + 1))
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        [ -n "$(declaring_file "$kind" "$ns" "$name")" ] && continue
        is_exempt "$kind" "$ns" "$name" && continue
        [ -n "$(LC_ALL=C awk -F'\t' -v n="$name" '$1 == n { print; exit }' <<<"$stamped")" ] && continue
        orphans+=("$(printf '%s\t%s\t%s' "$kind" "$ns" "$name")")
    done <<EOF
$names
EOF
done < <(printf '%s\n' ${pairs[@]+"${pairs[@]}"} ${template_pairs[@]+"${template_pairs[@]}"})

# Pairs in scope BY LABEL ONLY: the tree declares the kind elsewhere,
# not here, and only objects carrying its own label are its business.
label_pairs_checked=0
label_pairs_total=0
while IFS=$'\t' read -r kind ns; do
    [ -n "${kind:-}" ] || continue
    is_excluded_kind "$kind" && continue
    label_pairs_total=$((label_pairs_total + 1))
    if ! names=$(live_labelled_names "$kind" "$ns"); then
        unreadable=$((unreadable + 1))
        unreadable_names+=("$kind in $ns (by label) — not listable by this credential: $(read_error "$TMP/get.err")")
        continue
    fi
    label_pairs_checked=$((label_pairs_checked + 1))
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        [ -n "$(declaring_file "$kind" "$ns" "$name")" ] && continue
        is_exempt "$kind" "$ns" "$name" && continue
        orphans+=("$(printf '%s\t%s\t%s' "$kind" "$ns" "$name")")
    done <<EOF
$names
EOF
done <<EOF
$label_pairs
EOF

[ "${#orphans[@]}" -eq 0 ] || printf '%s\n' "${orphans[@]}"

say "${#orphans[@]} undeclared object(s) in $pairs_checked of $pairs_total in-scope (kind, namespace) pair(s) across ${managed_ns[*]}"
say "  plus $label_pairs_checked of $label_pairs_total pair(s) in scope by label only ($BOSS_LABEL on a kind the tree declares elsewhere)"
[ "${#excluded_pairs[@]}" -eq 0 ] || say "  excluded by kind: $(printf '%s; ' "${excluded_pairs[@]}")"
if [ "$unreadable" -gt 0 ]; then
    # Stated on every path. A first draft of the lint this was extracted
    # from printed the unverified list only on success, so a run that
    # found one orphan dropped "and here are the six things I could not
    # look at" — the record reduced before the reader saw it.
    say "  UNVERIFIED — $unreadable pair(s) this credential could not read, so they are not claimed clean:"
    printf '    %s\n' "${unreadable_names[@]}" >&2
fi
exit 0
