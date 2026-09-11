#!/usr/bin/env bash
# no-secrets — the "secrets never live in the tree" rule, as a gate.
#
# infra/cluster/manifests/README.md states the policy: manifests
# reference their secrets by name only; the Secret objects are created
# out-of-band; Talos machine configs, kubeconfig, and talosconfig stay
# in the operator's out-of-tree home precisely because they embed
# cluster PKI. Until now that policy was care, not a mechanism — and a
# credential that reaches git history stays reachable in every clone
# long after the line is deleted. This lint promotes the care into a
# check, the way the locomotive check promoted environment caution
# into one.
#
# What it scans — every tracked file (git ls-files), line by line:
#
#   private-key        PEM private-key block headers (RSA/EC/OPENSSH/
#                      DSA/PGP)
#   kubeconfig-data    client-key-data / client-certificate-data
#                      markers carrying a base64 payload
#   talos-cert-bundle  crt / key YAML keys carrying long base64 — the
#                      Talos machine-config PKI shape
#   wireguard-key      a WireGuard PrivateKey assignment with a real
#                      base64 value
#   token-assignment   token/secret/password/api_key/access_key
#                      assigned a 32+-char opaque value — skipped when
#                      the line is an obvious placeholder (a shell/env
#                      interpolation, an <angle-bracket> stand-in, or
#                      marker words like example / changeme / dummy)
#   url-token          a 40-hex Forgejo/Gitea token embedded in a URL
#                      as basic-auth
#   gcp-sa-json        GCP service-account JSON markers
#
# False positives are expected in fixtures and seeds. They go in
# infra/lint/no-secrets-allow.txt as `path:pattern-id`, each with a
# comment saying why it is not a secret — and every suppression is
# printed on every run, so the allow-list is visible policy, not a
# silencer.
#
# Findings print the offending line with the match replaced by
# [REDACTED:<pattern-id>], truncated: a lint that screams about a
# secret must not itself copy the secret into a CI log.
#
# Following the actor-stamp / locomotive precedent of checks that
# prove themselves, every run starts with a self-test: one planted
# fixture per pattern in a temp dir, each asserted caught; a
# placeholder asserted skipped; an allow-listed fixture asserted
# suppressed (and counted); the planted material asserted absent from
# every line of output. `--self-test` runs just that and stops.
#
# Usage: infra/lint/no-secrets.sh [--self-test]
# Exit:  0 clean / 1 findings or self-test failure / 3 git could not
#        list the tracked files, so NOTHING was scanned — an
#        infrastructure refusal, see lib/git-answer.sh

set -euo pipefail
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.." || exit 1
# shellcheck source=infra/lint/lib/git-answer.sh
. "$LINT_DIR/lib/git-answer.sh"

LINT=no-secrets
REPO_ALLOW_FILE="infra/lint/no-secrets-allow.txt"

PATTERN_IDS="private-key kubeconfig-data talos-cert-bundle wireguard-key token-assignment url-token gcp-sa-json"

# One ERE per pattern-id. Two constraints shape how these are written:
# they must run identically under grep -E and sed -E on both BSD and
# GNU userlands (so: [[:space:]] not \s, no back-references), and no
# regex may match its own source line here — this script is a tracked
# file and scans itself. That is why e.g. the private-key pattern is
# safe as a literal (its own text has "BEGIN (" where a real header
# has "BEGIN RSA") — checked by the self-test's scan shape and by the
# tree scan covering this file on every run.
regex_for() {
    case "$1" in
        private-key)
            printf '%s' '-----BEGIN (RSA|EC|OPENSSH|DSA|PGP) PRIVATE KEY' ;;
        kubeconfig-data)
            printf '%s' '(client-key-data|client-certificate-data):[[:space:]]*[A-Za-z0-9+/=]{24,}' ;;
        talos-cert-bundle)
            printf '%s' '^[[:space:]]*(crt|key):[[:space:]]*[A-Za-z0-9+/=]{40,}' ;;
        wireguard-key)
            printf '%s' '[Pp]rivate[Kk]ey[[:space:]]*=[[:space:]]*[A-Za-z0-9+/]{40,}={0,2}' ;;
        token-assignment)
            # Case-insensitivity is spelled out in classes because BSD
            # sed has no `I` flag and the same ERE drives redaction.
            printf '%s' "([tT][oO][kK][eE][nN]|[sS][eE][cC][rR][eE][tT]|[pP][aA][sS][sS][wW][oO][rR][dD]|[aA][pP][iI]_[kK][eE][yY]|[aA][cC][cC][eE][sS][sS]_[kK][eE][yY])[[:space:]]*[:=][[:space:]]*['\"]?[A-Za-z0-9+/_-]{32,}" ;;
        url-token)
            printf '%s' 'https?://[^/[:space:]:]+:[0-9a-f]{40}@' ;;
        gcp-sa-json)
            printf '%s' '"private_key_id"[[:space:]]*:|"private_key"[[:space:]]*:[[:space:]]*"?-----BEGIN' ;;
    esac
}

# A token-assignment hit whose line is self-evidently not a credential:
# shell/env interpolation, an <angle-bracket> stand-in, or a marker
# word. Everything else that is still a false positive (committed test
# fixtures, seeds) goes through the allow-list, visibly.
PLACEHOLDER_RE='(\$\{|<[^>]*>|xxx|example|changeme|change-me|change_me|dummy|placeholder|not-?a-?real|fake)'

# ---------------------------------------------------------------------
# Engine
# ---------------------------------------------------------------------

# redact_line <content> — print the line with every pattern's match
# replaced by [REDACTED:<id>], leading whitespace squeezed, truncated
# to 160 chars. All patterns are applied, not just the one that fired:
# a line dirty enough to trip one detector gets no benefit of the
# doubt on the rest of its content.
redact_line() {
    local s="$1" rid regex
    for rid in $PATTERN_IDS; do
        regex=$(regex_for "$rid")
        s=$(printf '%s\n' "$s" | sed -E $'s\001'"$regex"$'\001[REDACTED:'"$rid"$']\001g')
    done
    # Belt and suspenders: some patterns anchor on a marker, not the
    # value next to it (gcp-sa-json is the canonical case). Any long
    # opaque run still standing in a flagged line gets masked too —
    # a finding's snippet may lose cosmetic context, never a secret.
    s=$(printf '%s\n' "$s" | sed -E $'s\001[A-Za-z0-9+/=_-]{24,}\001[REDACTED:opaque]\001g')
    s=$(printf '%s\n' "$s" | sed -E 's/^[[:space:]]+//')
    printf '%.160s' "$s"
}

# allowed <file> <pattern-id> <allow-file> — 0 iff an allow entry
# covers this hit. Entry format: `path:pattern-id`; the path side may
# be a glob (matched against the path exactly as scanned). `#` starts
# a comment, full-line or trailing.
allowed() {
    local file="$1" id="$2" allow="$3" entry epath eid
    [ -f "$allow" ] || return 1
    while IFS= read -r entry || [ -n "$entry" ]; do
        entry=${entry%%#*}
        entry="${entry#"${entry%%[![:space:]]*}"}"
        entry="${entry%"${entry##*[![:space:]]}"}"
        [ -n "$entry" ] || continue
        eid=${entry##*:}
        epath=${entry%:*}
        [ "$eid" = "$id" ] || continue
        case "$file" in
            $epath) return 0 ;;
        esac
    done < "$allow"
    return 1
}

# scan_paths <allow-file> <violations-out> <suppressed-out> — reads a
# NUL-separated file list on stdin, appends redacted findings and
# suppression notes to the two out-files.
scan_paths() {
    local allow="$1" viol_out="$2" supp_out="$3"
    local files_tmp id regex hits hit file rest line content
    files_tmp=$(mktemp)
    cat > "$files_tmp"
    for id in $PATTERN_IDS; do
        regex=$(regex_for "$id")
        # /dev/null pins grep to multi-file mode so every hit carries
        # its filename even when xargs batches down to one file.
        hits=$(xargs -0 grep -nIE -e "$regex" /dev/null < "$files_tmp" 2>/dev/null || true)
        [ -n "$hits" ] || continue
        while IFS= read -r hit; do
            [ -n "$hit" ] || continue
            file=${hit%%:*}
            rest=${hit#*:}
            line=${rest%%:*}
            content=${rest#*:}
            if [ "$id" = "token-assignment" ] &&
                printf '%s\n' "$content" | grep -qiE "$PLACEHOLDER_RE"; then
                continue
            fi
            if allowed "$file" "$id" "$allow"; then
                printf '%s\n' "${file}:${line} [${id}]" >> "$supp_out"
            else
                printf '%s\n' "${file}:${line} [${id}] $(redact_line "$content")" >> "$viol_out"
            fi
        done <<< "$hits"
    done
    rm -f "$files_tmp"
}

# ---------------------------------------------------------------------
# Self-test — the lint proves its own detectors before it is allowed
# to say the tree is clean. Every fixture below is written through
# printf %s interpolation so that no matchable literal exists in this
# file; the fixture exists only in the temp dir, for milliseconds.
# ---------------------------------------------------------------------
self_test() {
    local tmp fails=0 viol supp viol2 supp2 allow b64 hex hex40 id
    tmp=$(mktemp -d)
    viol=$(mktemp); supp=$(mktemp); viol2=$(mktemp); supp2=$(mktemp)
    allow="$tmp/allow.txt"
    : > "$allow"

    # Inert stand-ins: the b64 decodes to a sentence saying what it is.
    b64='TG9uZ0Vub3VnaEJhc2U2NEZpeHR1cmVQYXlsb2FkRm9yU2VsZlRlc3RPbmx5MTIzNDU2Nzg='
    hex='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'
    hex40=${hex:0:40}   # Forgejo/Gitea tokens are exactly 40 hex chars

    printf -- '-----BEGIN %s PRIVATE KEY-----\n%s\n-----END %s PRIVATE KEY-----\n' \
        RSA "$b64" RSA > "$tmp/fx-private-key"
    printf 'users:\n- user:\n    client-certificate-data: %s\n    client-key-data: %s\n' \
        "$b64" "$b64" > "$tmp/fx-kubeconfig-data"
    printf 'ca:\n  crt: %s\n  key: %s\n' "$b64" "$b64" > "$tmp/fx-talos-cert-bundle"
    printf '[Interface]\nPrivateKey = %s\n' "$b64" > "$tmp/fx-wireguard-key"
    printf 'api_key: "%s"\n' "$hex" > "$tmp/fx-token-assignment"
    printf 'origin = https://oauth2:%s@git.internal.invalid/boss.git\n' \
        "$hex40" > "$tmp/fx-url-token"
    printf '{"type": "service_account", "private_key%s": "%s"}\n' \
        _id "$hex" > "$tmp/fx-gcp-sa-json"
    printf 'password = "%s"  # example value, must not be flagged\n' \
        "$hex" > "$tmp/fx-placeholder"

    # Run 1: empty allow-list — every pattern must catch its fixture,
    # the placeholder must not fire, and no output line may contain
    # the planted material.
    printf '%s\0' "$tmp"/fx-* | scan_paths "$allow" "$viol" "$supp"

    for id in $PATTERN_IDS; do
        if ! grep -q "fx-${id}:[0-9][0-9]* \[${id}\]" "$viol"; then
            echo "no-secrets self-test FAIL: pattern '${id}' missed its planted fixture" >&2
            fails=1
        fi
    done
    if grep -q 'fx-placeholder' "$viol"; then
        echo "no-secrets self-test FAIL: placeholder fixture was flagged" >&2
        fails=1
    fi
    # Grep for short PREFIXES of the planted material: a leak clipped
    # by the 160-char snippet truncation must still be caught.
    if grep -qF -e "${b64:0:16}" -e "${hex:0:16}" "$viol" "$supp"; then
        echo "no-secrets self-test FAIL: redaction leaked fixture material into the report" >&2
        fails=1
    fi

    # Run 2: the same WireGuard fixture, now allow-listed — it must be
    # suppressed AND the suppression must be visible.
    {
        echo '# self-test entry: planted fixture, not a credential'
        echo "$tmp/fx-wireguard-key:wireguard-key"
    } > "$allow"
    printf '%s\0' "$tmp/fx-wireguard-key" | scan_paths "$allow" "$viol2" "$supp2"
    if [ -s "$viol2" ]; then
        echo "no-secrets self-test FAIL: allow-listed fixture still reported as a finding" >&2
        fails=1
    fi
    if ! grep -q 'fx-wireguard-key:[0-9][0-9]* \[wireguard-key\]' "$supp2"; then
        echo "no-secrets self-test FAIL: allow-list suppression left no visible trace" >&2
        fails=1
    fi

    rm -rf "$tmp"
    rm -f "$viol" "$supp" "$viol2" "$supp2"
    if [ "$fails" -ne 0 ]; then
        echo "no-secrets: self-test FAILED — the detectors cannot be trusted, fix them first" >&2
        exit 1
    fi
    echo "no-secrets: self-test ok — 7/7 patterns caught, placeholder skipped, allow-list suppression visible, no fixture material leaked"
}

# ---------------------------------------------------------------------
# Tree scan
# ---------------------------------------------------------------------
main_scan() {
    local viol supp supp_count viol_count
    viol=$(mktemp); supp=$(mktemp)
    trap 'rm -f "'"$viol"'" "'"$supp"'"' EXIT

    # The ONE lint of the five that reached git and already failed rather
    # than certifying — `set -euo pipefail` carried git's 128 out. What it
    # did not do is SAY anything: the operator got git's fatal with no
    # lint name, no statement that zero files were scanned, and exit 128,
    # which no reader of this roster has a meaning for.
    #
    # The list lands in a FILE first rather than a pipe, because in
    # `git ls-files -z | scan_paths …` the status bash reports belongs to
    # the scanner. (A variable is not an option: -z separates paths with
    # NUL, which no shell variable can hold — and -z is why this lint can
    # read a path with a newline in it at all.)
    local paths ls_status=0
    paths=$(mktemp)
    git ls-files -z > "$paths" || ls_status=$?
    if [ "$ls_status" -ne 0 ]; then
        rm -f "$paths"
        echo "$LINT: CANNOT ANSWER — \`git ls-files -z\` exited $ls_status, so no file was scanned." >&2
        echo "  git's own message is above. An INFRASTRUCTURE refusal (exit $LINT_CANNOT_ANSWER), not" >&2
        echo "  a verdict on the branch: a scan of zero files is not a repository" >&2
        echo "  without secrets in it." >&2
        exit "$LINT_CANNOT_ANSWER"
    fi
    scan_paths "$REPO_ALLOW_FILE" "$viol" "$supp" < "$paths"
    rm -f "$paths"

    supp_count=$(wc -l < "$supp" | tr -d ' ')
    viol_count=$(wc -l < "$viol" | tr -d ' ')

    if [ "$supp_count" -gt 0 ]; then
        echo "no-secrets: allow-list suppressed ${supp_count} hit(s) (${REPO_ALLOW_FILE}):"
        sed 's/^/  allowed /' "$supp"
    fi

    if [ "$viol_count" -gt 0 ]; then
        echo "FAIL — credential-shaped material in tracked files (${viol_count} hit(s)):"
        sed 's/^/  /' "$viol"
        echo ""
        echo "If a hit is a fixture or placeholder: add 'path:pattern-id' to"
        echo "${REPO_ALLOW_FILE} with a comment saying why it is not a secret."
        echo "If a hit is real: it is compromised the moment it is pushed —"
        echo "rotate the credential first, then remove it; deleting the line"
        echo "does not delete it from history."
        exit 1
    fi
    echo "ok: no credential material in tracked files (${supp_count} allow-listed suppression(s))"
}

case "${1:-}" in
    --self-test)
        self_test ;;
    '')
        self_test
        main_scan ;;
    *)
        echo "usage: infra/lint/no-secrets.sh [--self-test]" >&2
        exit 2 ;;
esac
