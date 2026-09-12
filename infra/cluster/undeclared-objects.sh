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
#   * $DIR declares at least one object of that kind in it; and
#   * the kind is not in $EXCLUDED_KINDS below.
#
# OUT OF SCOPE, each for a stated reason — never silently skipped:
#   * CLUSTER-SCOPED KINDS (ClusterRole, ClusterRoleBinding, Namespace,
#     StorageClass). The cluster holds hundreds that belong to Talos,
#     Cilium, cert-manager and Longhorn; the tree declares four.
#     Sweeping them would report the cluster's own furniture as BOSS's
#     orphans.
#   * NAMESPACES THE TREE DOES NOT OWN. `boss-tls.yaml` puts two
#     one-shot Jobs into `cert-manager`, which cert-manager owns and
#     fills with its own work. "The tree declares something here" is
#     not "the tree manages this namespace".
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
#       the exemption entries, as `Kind/ns/name` or `Kind/name`.
#   undeclared-objects.sh --kubectl
#       the resolved kubectl argv, so a caller needing its own kubectl
#       call uses the same one rather than a second resolution.
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
objects_from_json() { # json-file source-file
    python3 - "$1" "$2" <<'PY'
import json, sys

dec = json.JSONDecoder()
src = sys.argv[2]
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
    # No name means a generateName template (the gate Job): it declares
    # no specific object, so it can neither be found nor be missed.
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
    --list|--declared|--exemptions|--kubectl) MODE="$1" ;;
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
        say "usage: $ME --list | --check <Kind>/<ns>/<name> | --declared | --exemptions | --kubectl | --objects-of <json>"
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
    objects_from_json "$TMP/parse.json" "$rel"
}

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
    return 1
}

is_excluded_kind() { # kind
    local k
    for k in ${EXCLUDED_KINDS+"${EXCLUDED_KINDS[@]}"}; do
        [ "$k" = "$1" ] && return 0
    done
    return 1
}

# --- the namespaces the tree owns ------------------------------------------
managed_ns=$(LC_ALL=C awk -F'\t' '$1 == "Namespace" { print $3 }' "$declared_converged" | LC_ALL=C sort -u)
if [ -z "$managed_ns" ]; then
    cannot_answer "$DIR declares no Namespace object — cannot tell which namespaces the tree owns"
fi
owns_ns() { printf '%s\n' "$managed_ns" | LC_ALL=C grep -qxF "$1"; }

# (kind, namespace) pairs in scope: a kind the tree declares in a
# namespace the tree owns.
pairs=$(LC_ALL=C awk -F'\t' -v mns="$managed_ns" '
    BEGIN { n = split(mns, a, "\n"); for (i = 1; i <= n; i++) if (a[i] != "") own[a[i]] = 1 }
    $2 != "" && ($2 in own) { print $1 "\t" $2 }
' "$declared_converged" | LC_ALL=C sort -u)
declares_pair() { printf '%s\n' "$pairs" | LC_ALL=C grep -qxF "$(printf '%s\t%s' "$1" "$2")"; }

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
        say "  so nothing here can be called undeclared. Owned: $(printf '%s ' $managed_ns)"
        exit 3
    fi
    if is_excluded_kind "$kind"; then
        say "REFUSED $kind/$ns/$name — $kind is excluded from this derivation by design (see EXCLUDED_KINDS in $ME):"
        say "  the tree's declaration set for it is incomplete on purpose, so 'undeclared' means nothing here."
        exit 3
    fi
    if ! declares_pair "$kind" "$ns"; then
        say "REFUSED $kind/$ns/$name — the tree declares no $kind in \`$ns\`, so there is no declared set to compare against."
        exit 3
    fi
    if file=$(declaring_file "$kind" "$ns" "$name"); [ -n "$file" ]; then
        say "REFUSED $kind/$ns/$name — the tree DECLARES it, in $file."
        exit 3
    fi
    if is_exempt "$kind" "$ns" "$name"; then
        say "REFUSED $kind/$ns/$name — it is EXEMPT in $ME (generated from sources already in the tree)."
        exit 3
    fi
    if ! names=$(live_names "$kind" "$ns"); then
        say "CANNOT ANSWER for $kind/$ns/$name — this credential cannot list $kind in \`$ns\`:"
        sed 's/^/    /' "$TMP/get.err" >&2
        say "  'could not look' is not 'undeclared'. Point KUBECONFIG at a credential that can read it."
        exit "$CANNOT_ANSWER"
    fi
    if ! printf '%s\n' "$names" | LC_ALL=C grep -qxF "$name"; then
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
        if ! printf '%s' "$owner_err" | LC_ALL=C grep -qiE 'notfound|not found'; then
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
    say "$kind/$ns/$name is live in a namespace the tree owns, of a kind the tree declares there, carries no"
    say "  ownerReference, and NO manifest in the tree declares it. Derived from $declared_count declared object(s)."
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
    pairs_checked=$((pairs_checked + 1))
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        [ -n "$(declaring_file "$kind" "$ns" "$name")" ] && continue
        is_exempt "$kind" "$ns" "$name" && continue
        orphans+=("$(printf '%s\t%s\t%s' "$kind" "$ns" "$name")")
    done <<EOF
$names
EOF
done <<EOF
$pairs
EOF

[ "${#orphans[@]}" -eq 0 ] || printf '%s\n' "${orphans[@]}"

say "${#orphans[@]} undeclared object(s) in $pairs_checked of $pairs_total in-scope (kind, namespace) pair(s) across $(printf '%s ' $managed_ns)"
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
