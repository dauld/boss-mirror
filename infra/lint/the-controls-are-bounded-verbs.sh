#!/usr/bin/env bash
# the-controls-are-bounded-verbs.sh — the ad hoc controls are ops verbs
# with the reclaim-disk shape: a script in the tree, params with
# patterns that admit no whitespace and no leading dash, a required
# sha for the rollback; the hold file round-trips; the runner's hold
# check reads it.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; repo="$(cd "$here/../.." && pwd)"
fail() { echo "FAIL: $*" >&2; exit 1; }
python3 - "$repo" <<'PY' || exit 1
import json,re,sys,os
repo=sys.argv[1]; v=json.load(open(f"{repo}/infra/ops/verbs.json"))["verbs"]
for name in ("rollback-to","hold-converge","release-converge","publish-github-pr"):
    spec=v.get(name) or sys.exit(f"FAIL: verb {name} missing")
    script=spec["argv"][0].replace("/home/david/boss/", f"{repo}/")
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
runner=open(f"{repo}/infra/ops/ops-runner.sh").read()
"$spec.timeout" in runner and "verb_timeout" in runner or sys.exit("FAIL: ops-runner.sh does not honour a verb's declared timeout")
"one_of" in runner and "optional" in runner or sys.exit("FAIL: ops-runner.sh does not read one_of/optional params — the literal allowlist is decoration")
print("verbs: rollback-to, hold-converge, release-converge, publish-github-pr are bounded and authorized")
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
git init -q --bare "$tmp/forge.git"
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
    sed "s#/home/david/boss/#$repo/#g" "$repo/infra/ops/verbs.json" > "$tmp/verbs.json"
    packet() { # $1 = args JSON array
        printf '{"data":[{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{"host":"forge","verb":"publish-github-pr","args":%s},"steps":[{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{"authority_role":"platform-admin"}}]}]}' "$1" > "$tmp/jobs.json"
    }
    run_runner() {
        env -i PATH="$tmp/rbin:$PATH" HOST_ID=forge BOSS_JOBS_URL=http://sor.invalid \
            OPS_VERBS_FILE="$tmp/verbs.json" STUB_JOBS="$tmp/jobs.json" STUB_PUT="$tmp/put.json" \
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
echo "the-controls-are-bounded-verbs: self-test ok — four bounded, authorized ops verbs; the hold round-trips through the file the runner reads, with no HOME in the environment; a hold needs a reason; a rollback needs a sha; publish-github-pr --check passes on complete inputs, refuses by path without the token, refuses a world-readable token, and a run refuses without a system of record; $runner_line"
exit 0
