#!/usr/bin/env bash
# Is what's in the tree what's running in the cluster?
#
# WHY THIS EXISTS. `boss-dev.yaml` was merged on train 36 (2026-08-15)
# and applied by hand from a laptop. Nothing recorded that it had been
# applied, and nothing would have said if it hadn't. A day later a
# design doc asserted — in writing, to David — that the dev pod had
# never run, while it was sitting there with 25 hours of uptime and a
# bound 40 Gi volume. The reasoning was "no script applies it and no
# reachable host has kubectl, therefore nobody has one", which was
# sound and wrong.
#
# `deploy-services.sh` owns systemd units on boss-gcp. Nothing owned
# `infra/cluster/manifests/`, so "merged" and "running" were different
# states with no observer. This is the observer.
#
# WHAT IT CHECKS. Every named object in every manifest exists in the
# cluster — and for the kinds where a present-but-WRONG object is the
# realistic failure, that its contents match too.
#
# EXISTENCE WAS NOT ENOUGH, measured (95f6aba5). The dev-session Role
# was granted batch/jobs create,get,list,watch by hand on 2026-08-28 to
# close a car, and proven by launching a real gate. The grant was never
# written into boss-dev-access.yaml, so the next converge removed it and
# `boss gate` started failing with `jobs.batch is forbidden`. The Role
# EXISTED throughout; only its rules differed, so this check was green
# across the whole window in which the capability it guards silently
# went away.
#
# Note the direction, because it is the interesting part: the cluster
# had MORE than the tree, and the convergence was CORRECT — it removed
# an undeclared grant. The defect was that nothing could see a live
# capability resting on undeclared state, so a `proven in prod` claim
# was true and load-bearing on something with no owner.
#
# So RBAC objects are compared by content: `rules` for Roles, and
# `roleRef` + `subjects` for bindings. A missing rule is invisible and a
# spurious one is a privilege the tree never granted; both now fail.
# Other kinds stay existence-only — a full drift diff is `kubectl diff`
# and needs write-shaped permission this credential does not have.
#
# EXIT CODES
#   0  every object present, and every RBAC object matches the tree
#   1  something in the tree is not in the cluster, or differs from it
#   2  cannot reach the cluster (no credential, no kubectl) — NOT
#      confused with "nothing is applied", because reporting a missing
#      credential as missing infrastructure is the same class of error
#      this script was written about.
#
# RUN IT with a credential that can read the namespaces in question.
# The boss-dev session credential is namespace-scoped and cannot read
# cluster-scoped objects (Namespace, StorageClass) — those are
# reported as skipped rather than passed, so a narrow credential
# cannot produce a falsely green run.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1
DIR="infra/cluster/manifests"

command -v kubectl >/dev/null 2>&1 || {
    echo "check-manifests-applied: kubectl not found — cannot verify." >&2
    echo "  This is 'unknown', not 'clean'. Install kubectl and point" >&2
    echo "  KUBECONFIG at a credential that can read the boss namespaces." >&2
    exit 2
}
if ! kubectl version -o json --request-timeout=10s >/dev/null 2>&1; then
    echo "check-manifests-applied: cannot reach the cluster API — cannot verify." >&2
    exit 2
fi

# kind/name/namespace for every document, via kubectl's own parser so
# this does not grow a YAML implementation.
inventory=$(
    for f in "$DIR"/*.yaml; do
        [ -f "$f" ] || continue
        # No {range .items[*]}: kubectl emits one JSON document per
        # object, not a List, so the template applies per document.
        # The source file rides along so the content check can recover
        # what the tree DECLARES for this object.
        # THE SOURCE FILE GOES FIRST, and namespace stays LAST, because
        # tab is IFS whitespace: bash collapses the two consecutive tabs
        # of a cluster-scoped object's empty namespace, and every field
        # after it shifts left. Appending the filename put it in `ns` for
        # every Namespace and StorageClass in the tree — caught by
        # running this, which reported `(ns infra/cluster/manifests/
        # boss-dev.yaml)`. A trailing empty field is harmless; a middle
        # one is not.
        kubectl create --dry-run=client -o \
            'jsonpath={.kind}{"\t"}{.metadata.name}{"\t"}{.metadata.namespace}{"\n"}' \
            -f "$f" 2>/dev/null | sed "s|^|$f\t|"
    done | grep -v '^[[:space:]]*$' | sort -u
)

# Compare the fields that carry the privilege, canonically.
#
# Order is not meaning here: kubectl returns rules and subjects in
# whatever order the API server holds them, and a list reordered by a
# round-trip is not drift. Both sides are normalised — inner lists
# sorted, then the outer list sorted by its serialisation — so only a
# real difference in what is GRANTED can fail this.
rbac_drift() { # kind name ns file  -> prints a diff summary, or nothing
    python3 - "$1" "$2" "${3:-}" "$4" <<'PY'
import json, subprocess, sys
kind, name, ns, path = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
FIELDS = ["rules"] if kind.endswith("Role") else ["roleRef", "subjects"]

def norm(v):
    if isinstance(v, dict):
        return {k: norm(x) for k, x in sorted(v.items()) if x not in (None, [], {})}
    if isinstance(v, list):
        return sorted((norm(x) for x in v), key=lambda y: json.dumps(y, sort_keys=True))
    return v

def run(args):
    p = subprocess.run(args, capture_output=True, text=True)
    return p.stdout if p.returncode == 0 else None

nsargs = ["-n", ns] if ns else []
live = run(["kubectl", "get", kind, name, *nsargs, "-o", "json", "--request-timeout=10s"])
want = run(["kubectl", "create", "--dry-run=client", "-o", "json", "-f", path])
if live is None or want is None:
    sys.exit(0)  # unreadable is handled by the existence pass, not here
live = json.loads(live)
# The file may hold several documents; kubectl prints them concatenated.
docs, dec = [], json.JSONDecoder()
i, s = 0, want.strip()
while i < len(s):
    obj, end = dec.raw_decode(s, i)
    docs.append(obj)
    i = end
    while i < len(s) and s[i] in " \n\r\t":
        i += 1
want = next(
    (d for d in docs
     if d.get("kind") == kind and (d.get("metadata") or {}).get("name") == name),
    None,
)
if want is None:
    sys.exit(0)
for f in FIELDS:
    if norm(live.get(f)) != norm(want.get(f)):
        print(f"{f} differs")
PY
}

total=$(printf '%s\n' "$inventory" | grep -c . || true)
if [ "$total" -lt 5 ]; then
    echo "check-manifests-applied: only parsed $total object(s) from $DIR —" >&2
    echo "  the scrape broke, so a green result would mean nothing." >&2
    exit 2
fi

missing=0; skipped=0; present=0; drifted=0
while IFS=$'\t' read -r file kind name ns; do
    [ -n "$kind" ] || continue
    if [ -n "$ns" ]; then
        args=(-n "$ns")
    else
        args=()
    fi
    out=$(kubectl get "$kind" "$name" "${args[@]}" --request-timeout=10s 2>&1)
    rc=$?
    if [ "$rc" -eq 0 ]; then
        present=$((present + 1))
        case "$kind" in
            Role|ClusterRole|RoleBinding|ClusterRoleBinding)
                why=$(rbac_drift "$kind" "$name" "$ns" "$file")
                if [ -n "$why" ]; then
                    echo "  DRIFT   $kind/$name${ns:+ (ns $ns)} — $why from $file" >&2
                    drifted=$((drifted + 1))
                fi
                ;;
        esac
    elif printf '%s' "$out" | grep -qiE 'forbidden|cannot list|cannot get'; then
        # Not visible to THIS credential. Say so; never count it green.
        echo "  skip    $kind/$name${ns:+ (ns $ns)} — not readable by this credential"
        skipped=$((skipped + 1))
    else
        echo "  MISSING $kind/$name${ns:+ (ns $ns)}" >&2
        missing=$((missing + 1))
    fi
done <<< "$inventory"

echo "check-manifests-applied: $present present, $missing missing, $drifted drifted, $skipped unreadable (of $total)"
if [ "$missing" -gt 0 ]; then
    echo "  A manifest in the tree is not in the cluster. Apply it, or delete it —" >&2
    echo "  a file that describes nothing running is worse than no file, because" >&2
    echo "  it reads as infrastructure that exists." >&2
    exit 1
fi
if [ "$drifted" -gt 0 ]; then
    echo "  An RBAC object in the cluster does not grant what the tree declares." >&2
    echo "  Either direction is a defect: a rule the cluster is MISSING is a" >&2
    echo "  capability about to vanish at the next converge, and a rule it has" >&2
    echo "  EXTRA is a privilege nobody declared and nobody owns." >&2
    exit 1
fi
# UNREADABLE IS NOT CLEAN. The namespace-scoped session credential can
# see 2 of these 24 objects, and an exit 0 on that run would report
# "the cluster matches the tree" having checked 8% of it. That is the
# same false comfort this whole script was written against — a green
# result must mean verified, so a partial view exits 2 (unknown) and
# names the number.
if [ "$skipped" -gt 0 ]; then
    echo "  $skipped of $total objects were not readable by this credential, so this" >&2
    echo "  run verified $present. That is 'unknown', not 'clean' — rerun with a" >&2
    echo "  credential that can read them before believing the cluster matches." >&2
    exit 2
fi
exit 0
