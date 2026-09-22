# jq.sh — the question `jq -e` cannot answer: was there a document?
#
# Source it, then ask before you judge:
#
#   . "$(dirname "$0")/../lib/jq.sh"
#   jq_doc_file "$body" && jq -e 'type == "object"' "$body" >/dev/null 2>&1 \
#       || fail "the endpoint answered nothing parseable"
#
#   jq_doc_text "$out" && printf '%s' "$out" | jq -e '.rows | type == "array"' >/dev/null
#
# WHY (2026-09-21, backlog d96e38ab). On jq-1.6 — the version on the
# pod, the forge and boss-gcp — `jq -e <any filter>` over an input
# carrying NO document exits **0**. Over malformed content it exits 4,
# correctly. So the one input that means "I got no answer at all" is
# the one input every `jq -e` guard in this tree read as a pass.
# Measured four ways on this pod:
#
#   jq -e . < /dev/null                      -> 0
#   jq -e 'type == "object"' < /dev/null     -> 0
#   jq -e 'type == "object"' < "   \n"       -> 0
#   jq -e . < "not json at all"              -> 4
#   printf '{"a":1}' | jq -e 'empty'         -> 4
#
# The last two lines are the mechanism: jq's exit code is RIGHT
# whenever it read a document — `empty` output is a 4 — and jq-1.6
# simply never sets it when there was no document to run the filter
# over. So the missing question is not "is the filter true", which jq
# answers; it is "was there anything to ask it about".
#
# IT ALREADY COST A DAILY CHECK. `infra/forge/publish-github-pr.sh`
# verified its drift write by jq-ing the PATCH response body, and that
# door answers 204 with no body. On this pod the check PASSED while
# verifying nothing, for its whole life; on the forge's jq it reported
# FAILED on every successful run (b88a13d5, ops-request 9340fd6e). One
# line, two opposite wrong answers, neither of them a verification.
#
# WHY NOT `[ -s "$f" ]`, which was the first repair reached for: a
# whitespace-only file has size > 0, so `-s` passes and `jq -e` still
# exits 0 on it. `-s` also cannot be asked of a value already in a
# shell variable, which is half the call sites. The one thing true of
# every silent input — zero-byte, whitespace-only, malformed, absent,
# and jq itself missing — is that jq PRODUCES NO OUTPUT, so that is
# what these read. They use jq's OUTPUT and never its exit status,
# because the exit status is the broken half.
#
# WHY NOT A WRAPPER AROUND `jq -e`: the call sites carry multi-line
# filters, `--arg`, `--argjson` and `-r`, and one writes jq's stdout to
# a file. A wrapper would have to re-order every one of those, and `-r`
# would defeat an output test. These answer the one question jq gets
# wrong and leave each site's own `jq -e` verbatim beside them
# (CLAUDE.md §9a — one definition, not sixteen correct copies).
#
# THEY FAIL CLOSED. An absent file, an unreadable one, and a host with
# no jq all answer "no document" rather than erroring, because a guard
# that cannot read is a guard that refuses (CLAUDE.md §Doors — a wrong
# target answers instead of erroring). `null` IS a document: the site's
# own `jq -e .` then correctly exits 1 on it, and keeping the two
# questions apart is what lets a reader tell "the server said null"
# from "the server said nothing".
#
# POSIX sh, no bashisms: `infra/ops/verbs-allowlist.sh` is `#!/bin/sh`,
# which is dash on the forge and boss-gcp.

# jq_doc_file FILE — 0 when FILE holds at least one JSON document.
jq_doc_file() {
    [ -n "$(jq -c 'true' "${1-}" 2>/dev/null)" ]
}

# jq_doc_text TEXT — the same question for a value already in a
# variable: a curl body, a command substitution, a caller's argument.
jq_doc_text() {
    [ -n "$(printf '%s' "${1-}" | jq -c 'true' 2>/dev/null)" ]
}
