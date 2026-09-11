#!/usr/bin/env bash
# a-signature-follows-its-decision.sh — in sign-off.js's decision flow
# the decision lands (metadata PATCH) BEFORE the signature is taken, the
# completion (PUT) comes after both, and nothing writes metadata after
# the signature.
#
# A stamp attests the step's shape, metadata included; a metadata write
# after the stamp makes the stamp stale by design. On 2026-09-05 the
# flow signed and then re-saved the decision, and the operator's own
# signature was refused as stale by the completion two seconds later.
# This reads the source order of the decision handler and refuses the
# inversion. Self-tested on planted fixtures.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# order FILE — prints "ok" or the violation, reading the decide handler:
# from the first `patch.decision =` to the `finally {` that ends it.
order() {
    python3 - "$1" <<'PY'
import sys,re
s=open(sys.argv[1]).read()
a=s.find('patch.decision =')
b=s.find('} finally {', a)
if a<0 or b<0: print('no decision flow found'); sys.exit(0)
body=s[a:b]
patch=body.find("method: 'PATCH'"); sign=body.find('await sign('); put=body.find("method: 'PUT'")
if patch<0: print('no metadata PATCH in the decision flow'); sys.exit(0)
if sign<0: print('the decision flow never signs'); sys.exit(0)
if put<0: print('the decision flow never completes'); sys.exit(0)
if not (patch<sign<put): print(f'order wrong: PATCH@{patch} sign@{sign} PUT@{put}'); sys.exit(0)
if "method: 'PATCH'" in body[sign:]: print('a metadata PATCH after the signature'); sys.exit(0)
print('ok')
PY
}
self_test() {
    local fx; fx="$(mktemp -d)"; trap 'rm -rf "$fx"' RETURN
    printf "patch.decision = d;\nfetch(x, { method: 'PATCH' });\nconst signed = await sign(left[0]);\nfetch(y, { method: 'PUT' });\n} finally {\n" >"$fx/good.js"
    [[ "$(order "$fx/good.js")" == "ok" ]] || { echo "self-test FAILED: the good order was refused" >&2; return 1; }
    printf "patch.decision = d;\nconst signed = await sign(left[0]);\nfetch(x, { method: 'PATCH' });\nfetch(y, { method: 'PUT' });\n} finally {\n" >"$fx/bad.js"
    [[ "$(order "$fx/bad.js")" != "ok" ]] || { echo "self-test FAILED: a PATCH after the signature passed" >&2; return 1; }
    printf "patch.decision = d;\nfetch(x, { method: 'PATCH' });\nfetch(y, { method: 'PUT' });\n} finally {\n" >"$fx/nosign.js"
    [[ "$(order "$fx/nosign.js")" == "the decision flow never signs" ]] || { echo "self-test FAILED: a flow that never signs passed" >&2; return 1; }
    echo "a-signature-follows-its-decision: self-test ok — decide→sign→complete accepted; a PATCH after the signature and a flow that never signs refused"
}
if [[ "${1:-}" == "--self-test" ]]; then self_test; exit $?; fi
self_test || exit 1
f="$here/../step-plugins/sign-off.js"
r="$(order "$f")"
[[ "$r" == "ok" ]] || { echo "a-signature-follows-its-decision: FAIL — sign-off.js decision flow: $r" >&2; exit 1; }
echo "a-signature-follows-its-decision: sign-off.js decides, then signs, then completes — no metadata write after the signature"
exit 0
