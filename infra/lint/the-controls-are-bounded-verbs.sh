#!/usr/bin/env bash
# the-controls-are-bounded-verbs.sh — the ad hoc controls are ops verbs
# with the reclaim-disk shape: a script in the tree, params with
# patterns that admit no whitespace and no leading dash, a required
# sha for the rollback; the hold file round-trips; the runner's hold
# check reads it.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; repo="$(cd "$here/../.." && pwd)"
fail() { echo "FAIL: $*" >&2; exit 1; }
# The allowlist is the directory infra/ops/verbs/, assembled by the one
# script the runner itself uses (5086842d) — read here the same way.
allowlist="$(sh "$repo/infra/ops/verbs-allowlist.sh" "$repo/infra/ops/verbs")" \
    || fail "infra/ops/verbs-allowlist.sh could not assemble infra/ops/verbs/ (see above)"
python3 - "$repo" "$allowlist" <<'PY' || exit 1
import json,re,sys,os
repo=sys.argv[1]; v=json.loads(sys.argv[2])["verbs"]
# THE ROSTER IS DERIVED, not listed here. It used to be four names typed
# into this loop, which meant every mutating verb added after them —
# reclaim-disk, converge, mirror-base-images, delete-orphan-object — was
# outside the only check that says a mutating verb is bounded and
# authorized. A roster that has to be edited in two places is the §9a
# defect; a verb that declares itself MUTATING is the one definition.
mutating=sorted(n for n,s in v.items() if "MUTATING" in s.get("about",""))
len(mutating) >= 8 or sys.exit(f"FAIL: only {len(mutating)} verb(s) declare MUTATING — the roster derivation broke: {mutating}")
for name in mutating:
    spec=v[name]
    # argv[0] is repo-relative (66077f9c); the runner resolves it against
    # its own checkout, and so does this lint — one rule, no substitution.
    argv0=spec["argv"][0]
    script=argv0 if argv0.startswith("/") else os.path.join(repo, argv0)
    os.path.isfile(script) or sys.exit(f"FAIL: {name} points at a script not in the tree: {spec['argv'][0]}")
    os.access(script, os.X_OK) or sys.exit(f"FAIL: {name}'s script is not executable")
    for p in spec["params"]:
        if "one_of" in p:
            # A literal list is exact-match only, and each word is
            # reviewed file content: whitespace-free, so the runner's
            # newline-split argv stays exact. A leading dash is allowed
            # HERE precisely because the packet cannot supply the word.
            "pattern" in p and sys.exit(f"FAIL: {name}.{p['name']} mixes one_of with a pattern — a literal list is equality only")
            words=p["one_of"]
            (isinstance(words,list) and words and all(isinstance(w,str) and w and not re.search(r"\s",w) for w in words)) \
                or sys.exit(f"FAIL: {name}.{p['name']}.one_of must be a non-empty list of whitespace-free words")
            continue
        pat=p["pattern"]
        for bad in (" ", "\t", "\n"):
            re.fullmatch(pat, "a"+bad+"b") and sys.exit(f"FAIL: {name}.{p['name']} admits whitespace")
        re.fullmatch(pat, "-x") and sys.exit(f"FAIL: {name}.{p['name']} admits a leading dash")
    "MUTATING" in spec["about"] or sys.exit(f"FAIL: {name} does not say it is MUTATING")
    "David" in spec["about"] or sys.exit(f"FAIL: {name} names no authorization")
rb=v["rollback-to"]["params"][0]
"default" in rb and sys.exit("FAIL: rollback-to's sha has a default — a rollback must name its target")
re.fullmatch(rb["pattern"],"2683908") or sys.exit("FAIL: a 7-char sha is refused")
re.fullmatch(rb["pattern"],"b2814ef") or sys.exit("FAIL: a real short sha is refused")
re.fullmatch(rb["pattern"],"latest") and sys.exit("FAIL: 'latest' passes as a sha")
# publish-github-pr takes NO packet-supplied TEXT (fixed repos, a dated
# branch): its one param is a literal list the packet can only select
# from, and the only literal is --check — the verb's own no-network
# input check (6964f9e8), so the verb is exercisable through the runner
# without the real run; optional, because the real run passes no arg.
# It declares its own timeout, because a first push of the whole tree
# exceeds the runner's 30s default — and the runner must actually read
# that field, or the number is decoration.
pub=v["publish-github-pr"]
free=[p["name"] for p in pub["params"] if "one_of" not in p]
free and sys.exit(f"FAIL: publish-github-pr must take no packet-supplied text (pattern params: {free})")
lits=sorted(w for p in pub["params"] for w in p["one_of"])
lits==["--check"] or sys.exit(f"FAIL: publish-github-pr must admit exactly the literal --check, got {lits}")
all(p.get("optional") is True for p in pub["params"]) or sys.exit("FAIL: publish-github-pr's --check must be optional — the real run passes no arg")
isinstance(pub.get("timeout"), int) and pub["timeout"] >= 120 or sys.exit("FAIL: publish-github-pr must declare a timeout of at least 120s")
# delete-orphan-object: the one verb whose authority is DERIVED rather
# than granted. There is deliberately no general `kubectl delete` verb,
# so the properties that keep this one bounded are the properties that
# keep the allowlist worth having: one object named in full, the kind
# floor, and the derivation it re-runs at call time.
dob=v["delete-orphan-object"]
pats=[p for p in dob["params"] if "one_of" not in p]
[p["name"] for p in pats]==["object"] or sys.exit(f"FAIL: delete-orphan-object must take exactly one pattern param, `object`, got {[p['name'] for p in pats]}")
lits=sorted(w for p in dob["params"] if "one_of" in p for w in p["one_of"])
lits==["--dry-run"] or sys.exit(f"FAIL: delete-orphan-object must admit exactly the literal --dry-run, got {lits}")
all(p.get("optional") is True for p in dob["params"] if "one_of" in p) or sys.exit("FAIL: --dry-run must be optional — the real run passes no second arg")
op=pats[0]["pattern"]
"default" in pats[0] and sys.exit("FAIL: delete-orphan-object's object has a default — a delete must name its target")
for ok in ("Service/boss/boss-docs-internal","ConfigMap/boss-dev/gate-runner-script"):
    re.fullmatch(op, ok) or sys.exit(f"FAIL: the object pattern refuses {ok}")
# A delete whose target is not fully named is a delete with a scope, and
# a scope is what this verb must never accept.
for bad in ("services/boss/x","Service/boss","boss-docs-internal","Service/boss/x/y","Service//x","*/boss/x","Service/boss/*"):
    re.fullmatch(op, bad) and sys.exit(f"FAIL: the object pattern admits {bad}")
isinstance(dob.get("timeout"), int) and dob["timeout"] >= 120 or sys.exit("FAIL: delete-orphan-object must declare a timeout — the derivation parses every manifest")
for phrase in ("undeclared-objects.sh","DERIVED","--dry-run"):
    phrase in dob["about"] or sys.exit(f"FAIL: delete-orphan-object's about does not say {phrase}")
for n,s in v.items():
    re.search(r"\bkubectl\b[^.\n]*\bdelete\b", " ".join(s["argv"])) and sys.exit(f"FAIL: verb {n} hands kubectl a delete directly — the allowlist must not carry an unbounded delete")
# The derivation is the authority, so it has to be there, and the verb
# has to be the thing that calls it.
derive=f"{repo}/infra/cluster/undeclared-objects.sh"
os.path.isfile(derive) and os.access(derive, os.X_OK) or sys.exit("FAIL: infra/cluster/undeclared-objects.sh is missing or not executable")
dobsh=open(f"{repo}/infra/forge/delete-orphan-object.sh").read()
"undeclared-objects.sh" in dobsh or sys.exit("FAIL: delete-orphan-object.sh does not call the derivation")
re.search(r'delete "\$KIND" "\$NAME" -n "\$NS"', dobsh) or sys.exit("FAIL: delete-orphan-object.sh's delete does not use the derivation's own fields")
'"$TARGET"' in dobsh.split("--- the delete")[-1] and sys.exit("FAIL: delete-orphan-object.sh's delete reads the packet's string instead of the derivation's answer")
floor=re.search(r"^DELETABLE_KINDS=\(([^)]*)\)", dobsh, re.M) or sys.exit("FAIL: delete-orphan-object.sh declares no DELETABLE_KINDS floor")
kinds=floor.group(1).split()
# Bytes, credentials and privileges stay a named human step. A widening
# here is a reviewed change to that file, and this is the review.
for withheld in ("PersistentVolumeClaim","StatefulSet","Secret","ServiceAccount","Role","RoleBinding","Namespace","ClusterRole","ClusterRoleBinding"):
    withheld in kinds and sys.exit(f"FAIL: the kind floor admits {withheld} — bytes, credentials and privileges stay a human step")
kinds or sys.exit("FAIL: the kind floor is empty")
print(f"verbs: the kind floor is {' '.join(kinds)}; the object pattern names one object in full")

runner=open(f"{repo}/infra/ops/ops-runner.sh").read()
"$spec.timeout" in runner and "verb_timeout" in runner or sys.exit("FAIL: ops-runner.sh does not honour a verb's declared timeout")
"one_of" in runner and "optional" in runner or sys.exit("FAIL: ops-runner.sh does not read one_of/optional params — the literal allowlist is decoration")
print("verbs: " + ", ".join(mutating) + " are bounded and authorized")
PY
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
export BOSS_CONVERGE_HOLD="$tmp/hold"
# Under the ops runner a verb runs as root with NO HOME — the scripts
# must not need one (2026-09-05: "HOME: unbound variable").
env -i PATH="$PATH" BOSS_CONVERGE_HOLD="$tmp/hold" bash "$repo/infra/forge/converge-hold.sh" hold no-home-here >/dev/null || fail "converge-hold.sh needs HOME (the ops runner has none)"
env -i PATH="$PATH" BOSS_CONVERGE_HOLD="$tmp/hold" bash "$repo/infra/forge/converge-hold.sh" release >/dev/null || fail "release needs HOME"
env -i PATH="$PATH" bash -n "$repo/infra/forge/rollback-to.sh" || fail "rollback-to.sh does not parse"
# Code lines only — a comment may name $HOME to say why it is not used.
grep -vE '^\s*#' "$repo/infra/forge/rollback-to.sh" | grep -qE '\$HOME' && fail "rollback-to.sh still reads \$HOME"
grep -vE '^\s*#' "$repo/infra/forge/converge-hold.sh" | grep -qE '\$HOME' && fail "converge-hold.sh still reads \$HOME"
bash "$repo/infra/forge/converge-hold.sh" hold learning-the-new-runner >/dev/null || fail "hold failed"
[[ "$(<"$tmp/hold")" == "learning-the-new-runner" ]] || fail "the hold file does not carry the reason"
# shellcheck source=/dev/null
. "$repo/infra/forge/cluster-deploy-lib.sh"
reason=$(converge_held "$tmp/hold") || fail "the runner's hold check did not see the hold"
[[ "$reason" == "learning-the-new-runner" ]] || fail "the hold check returned '$reason'"
bash "$repo/infra/forge/converge-hold.sh" release >/dev/null || fail "release failed"
converge_held "$tmp/hold" >/dev/null && fail "a released hold still holds"
bash "$repo/infra/forge/converge-hold.sh" hold 2>/dev/null && fail "a hold with no reason was accepted"

# publish-github-pr: its --check validates inputs with no network, under
# the runner's environment (no HOME), and a missing token is a refusal
# that NAMES THE PATH — never a silent skip, never the value.
pub="$repo/infra/forge/publish-github-pr.sh"
env -i PATH="$PATH" bash -n "$pub" || fail "publish-github-pr.sh does not parse"
grep -vE '^\s*#' "$pub" | grep -qE '\$HOME' && fail "publish-github-pr.sh reads \$HOME (the ops runner has none)"
grep -vE '^\s*#' "$pub" | grep -qE '^\s*set .*-x|set -x' && fail "publish-github-pr.sh traces (set -x) — a trace would print the token's environment"
mkdir -p "$tmp/bin" "$tmp/state" "$tmp/etc"
# --check only asks that gh/jq/curl EXIST (this box may lack jq; the
# forge and the gate image have it), so stubs stand in for all three.
for t in gh jq curl; do printf '#!/bin/sh\nexit 0\n' > "$tmp/bin/$t"; chmod +x "$tmp/bin/$t"; done
# The forge stand-in carries a `main` commit: since 2026-09-11 --check
# FETCHES refs/heads/main from it, because `-c safe.directory=<src>`
# cannot exempt a fetch SOURCE and a check that only READ the directory
# passed twice while the publish failed (ops-request c258d3b7).
git init -q --bare "$tmp/forge.git"
fixture_git() { env -i PATH="$PATH" GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=f@example.invalid \
    GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=f@example.invalid git -C "$tmp/forge.git" "$@"; }
seed_tree=$(fixture_git hash-object -t tree -w --stdin </dev/null) \
    || fail "could not write the fixture's empty tree"
seed_commit=$(fixture_git commit-tree "$seed_tree" -m seed) \
    || fail "could not write the fixture's seed commit"
fixture_git update-ref refs/heads/main "$seed_commit" \
    || fail "could not point the fixture's main at $seed_commit"
printf 'not-a-real-token\n' > "$tmp/etc/github.token"; chmod 600 "$tmp/etc/github.token"
checkenv=(env -i PATH="$tmp/bin:$PATH" BOSS_PUBLISH_STATE_DIR="$tmp/state" BOSS_FORGE_REPO_PATH="$tmp/forge.git")
out=$("${checkenv[@]}" BOSS_GITHUB_TOKEN_FILE="$tmp/etc/github.token" bash "$pub" --check 2>&1) \
    || fail "publish-github-pr.sh --check refused a complete input set: $out"
grep -q -- '--check ok' <<<"$out" || fail "--check did not report ok: $out"
grep -q 'not-a-real-token' <<<"$out" && fail "--check printed the token"
out=$("${checkenv[@]}" BOSS_GITHUB_TOKEN_FILE="$tmp/etc/absent.token" bash "$pub" --check 2>&1) \
    && fail "--check passed with no token file"
grep -q "$tmp/etc/absent.token" <<<"$out" || fail "the refusal does not name the token path: $out"
grep -qi 'token admin' <<<"$out" || fail "the refusal does not say who provisions the token: $out"
chmod 644 "$tmp/etc/github.token"
"${checkenv[@]}" BOSS_GITHUB_TOKEN_FILE="$tmp/etc/github.token" bash "$pub" --check >/dev/null 2>&1 \
    && fail "--check accepted a world-readable token file"
chmod 600 "$tmp/etc/github.token"
# A run (no --check) with no system of record refuses before touching
# anything — the ops-runner rule, and the reason nothing here needs a
# network to prove.
"${checkenv[@]}" BOSS_GITHUB_TOKEN_FILE="$tmp/etc/github.token" bash "$pub" >/dev/null 2>&1 \
    && fail "a run without BOSS_JOBS_URL did not refuse"
# THROUGH THE RUNNER: the allowed literal is exercised the way a packet
# would — ops-runner.sh against a stubbed system of record (a GET serves
# one open packet, a PUT records the completion) and the real allowlist
# with its script path rewritten to this tree. `--check` must be
# ANSWERED (the verb's own check ran and said ok); a word outside the
# literal list must be REFUSED with the reason on the step, named,
# and identical to the journal line (6964f9e8: the reason lived only
# in the forge journal). The runner is sh + jq; this box may lack jq.
runner_line="runner path not exercised here (no jq on this box)"
if command -v jq >/dev/null 2>&1; then
    mkdir -p "$tmp/rbin" "$tmp/rstate"
    printf '#!/bin/sh\nexit 0\n' > "$tmp/rbin/gh"; chmod +x "$tmp/rbin/gh"
    cat > "$tmp/rbin/curl" <<'EOF'
#!/bin/sh
# The system of record, stubbed: a PUT records its payload; a GET serves the fixture.
for a in "$@"; do case "$a" in @*) cp "${a#@}" "$STUB_PUT"; exit 0;; esac; done
cat "$STUB_JOBS"
EOF
    chmod +x "$tmp/rbin/curl"
    # The allowlist is used VERBATIM — the directory of verb files copied
    # as-is: its scripts are repo-relative and the runner resolves them
    # against OPS_REPO_ROOT (66077f9c).
    mkdir -p "$tmp/verbs" && cp "$repo"/infra/ops/verbs/*.json "$tmp/verbs/"
    packet() { # $1 = args JSON array
        printf '{"data":[{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{"host":"forge","verb":"publish-github-pr","args":%s},"steps":[{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{"authority_role":"platform-admin"}}]}]}' "$1" > "$tmp/jobs.json"
    }
    run_runner() {
        env -i PATH="$tmp/rbin:$PATH" HOST_ID=forge BOSS_JOBS_URL=http://sor.invalid \
            OPS_VERBS_DIR="$tmp/verbs" STUB_JOBS="$tmp/jobs.json" STUB_PUT="$tmp/put.json" \
            BOSS_PUBLISH_STATE_DIR="$tmp/rstate" BOSS_FORGE_REPO_PATH="$tmp/forge.git" \
            BOSS_GITHUB_TOKEN_FILE="$tmp/etc/github.token" \
            sh "$repo/infra/ops/ops-runner.sh" 2>&1
    }
    rm -f "$tmp/put.json"; packet '["--check"]'
    out=$(run_runner) || fail "the runner failed on publish-github-pr --check: $out"
    [[ -f "$tmp/put.json" ]] || fail "the runner completed no step for --check: $out"
    [[ "$(jq -r .metadata.disposition "$tmp/put.json")" == answered ]] || fail "--check was not answered through the runner: $(cat "$tmp/put.json") / $out"
    jq -r .metadata.output "$tmp/put.json" | grep -q -- '--check ok' || fail "--check through the runner did not report ok: $(cat "$tmp/put.json")"
    rm -f "$tmp/put.json"; packet '["--force"]'
    out=$(run_runner) || fail "the runner failed refusing --force: $out"
    [[ "$(jq -r .metadata.disposition "$tmp/put.json")" == refused ]] || fail "--force was not refused: $(cat "$tmp/put.json")"
    reason=$(jq -r '.metadata.reason // empty' "$tmp/put.json")
    [[ -n "$reason" ]] || fail "the refusal wrote no reason on the step: $(cat "$tmp/put.json")"
    [[ "$reason" == *"not one of --check"* ]] || fail "the reason does not name the literal list: $reason"
    [[ "$reason" == "$(jq -r .metadata.output "$tmp/put.json")" ]] || fail "reason and output differ on a refusal"
    grep -qF -- "refused aaaaaaaa — $reason" <<<"$out" || fail "the journal line does not carry the same reason: $out"
    runner_line="through the runner, publish-github-pr --check is answered and --force is refused with the reason on the step"
fi
# delete-orphan-object: the bounds that need no cluster, exercised. The
# derivation's own behaviour against a stubbed kubectl is
# boss-testing/tests/delete_orphan_object_sh.rs; here we pin only that
# the argument bound refuses BEFORE anything looks at a cluster, under
# the ops-runner's environment (no HOME).
dob="$repo/infra/forge/delete-orphan-object.sh"
der="$repo/infra/cluster/undeclared-objects.sh"
env -i PATH="$PATH" bash -n "$dob" || fail "delete-orphan-object.sh does not parse"
env -i PATH="$PATH" bash -n "$der" || fail "undeclared-objects.sh does not parse"
grep -vE '^\s*#' "$dob" | grep -qE '\$HOME' && fail "delete-orphan-object.sh reads \$HOME (the ops runner has none)"
grep -vE '^\s*#' "$der" | grep -qE '\$HOME' && fail "undeclared-objects.sh reads \$HOME (the ops runner has none)"
# EVERY git CALL IN THE VERB GOES THROUGH THE OWNER.
#
# The ops-runner executes verbs as root and the forge checkout belongs to a
# user, and git refuses to READ across that boundary ("dubious ownership",
# 2.35.2+) as firmly as a root WRITE would leave root-owned objects behind.
# A bare `git -C "$TREE"` is therefore a command that fails on every real
# invocation while passing every test here, because a fixture is owned by
# whoever runs the gate — measured, on ops-request c9877f75, 2026-09-10.
# Structural, for the same reason boss-gcp-converges-itself.sh §2b is: the
# runuser branch cannot be exercised without a second account.
bare=$(grep -nE '(^|[^_"])git -C "\$TREE"' "$dob" || true)
[ -z "$bare" ] || fail "delete-orphan-object.sh calls git outside as_owner:
$bare
    Root cannot even READ a checkout it does not own; wrap it:
      as_owner \"git -C '\$TREE' <args>\""
grep -q 'as_owner()' "$dob" || fail "delete-orphan-object.sh has no as_owner — its git reads cannot be running as the checkout's owner"
grep -q 'stat -c %U' "$dob" || fail "delete-orphan-object.sh does not read the owner off the directory (hardcoding an account silently corrupts a host that moves the checkout)"
grep -q 'UNKNOWN' "$dob" || fail "delete-orphan-object.sh does not handle stat's UNKNOWN (no passwd entry for the owning uid), so it would fall back to reading git as the caller"
# And the derivation needs none of this, which is only true while it makes
# no git call and writes nothing under the checkout. Pin both, because the
# day either changes it acquires the same defect silently.
der_git=$(grep -vE '^\s*#' "$der" | grep -nE '(^|[^-[:alnum:]_.])git[[:space:]]' || true)
[ -z "$der_git" ] || fail "undeclared-objects.sh now calls git:
$der_git
    It runs as root under the ops-runner against a user-owned checkout, so a
    git call there needs the same as_owner drop delete-orphan-object.sh uses."
der_writes=$(grep -nE '>[[:space:]]*"?\$(TREE|DIR)' "$der" || true)
[ -z "$der_writes" ] || fail "undeclared-objects.sh writes under the checkout:
$der_writes
    Root writing in a user-owned clone is what breaks the owner's later pulls.
    Keep its scratch in mktemp."

for bad in "boss-docs-internal" "service/boss/x" "Service/boss"; do
    out=$(env -i PATH="$PATH" bash "$dob" "$bad" 2>&1) \
        && fail "delete-orphan-object.sh accepted the malformed target '$bad'"
    grep -q '<Kind>/<namespace>/<name>' <<<"$out" || fail "the refusal of '$bad' does not name the shape: $out"
done
out=$(env -i PATH="$PATH" bash "$dob" "Service/boss/x" --force 2>&1) \
    && fail "delete-orphan-object.sh accepted a second argument other than --dry-run"
grep -q -- '--dry-run' <<<"$out" || fail "the refusal does not name the only allowed mode: $out"

echo "the-controls-are-bounded-verbs: self-test ok — every MUTATING ops verb is bounded and authorized; the hold round-trips through the file the runner reads, with no HOME in the environment; a hold needs a reason; a rollback needs a sha; publish-github-pr --check passes on complete inputs, refuses by path without the token, refuses a world-readable token, and a run refuses without a system of record; $runner_line"
exit 0
