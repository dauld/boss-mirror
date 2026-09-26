#!/usr/bin/env bash
#
# forge-auth-header — protect-main's credential, read off the forge
# checkout's own `forgejo` remote URL (backlog 164f38c7).
#
#   forge-auth-header.sh HEADER_FILE READER [ARGS...]
#
# READER prints the remote's URL on stdout. forge-converge.sh runs it as
# the checkout's owner:
#   runuser -l "$OWNER" -c "git -C '$REPO' remote get-url forgejo"
# The URL's userinfo, url-decoded, becomes `Authorization: Basic <base64>`
# in HEADER_FILE (truncated first; created 0600 if absent). A token-only
# userinfo gains a trailing colon: Forgejo reads `user:token` as the
# token-as-password and `token:` as the token-as-username, so Basic
# carries both shapes and protect-main.sh needs no change (it hands curl
# any header with `-H @file`).
#
# Exit 0: header written. Exit 3: no credential (READER failed, or its URL
# is not http(s) or carries no userinfo) — HEADER_FILE is left EMPTY and
# protect-main's own exit 4 is the verdict on the packet. Exit 2: usage.
#
# WHY IT EXISTS
# -------------
# Car 4 of design d812f1b7 filled the header from `git credential fill`
# run as the owner. The owner has NO credential helper (measured
# 2026-09-19 on ops-request 3d9d5f58, recorded in publish-github-pr.sh):
# the converge's fetch authenticates because the remote URL itself
# carries the credential, which is how publish-github-pr.sh's
# checkout_remote_url and cluster-deploy-lib.sh already read it. So the
# fill read nothing, every converge since #694 closed FAILED on
# protect-main's exit 4 (runs 954387f5, b4bcb5af, 868483c1), and git's
# prompt-disabled error — `could not read Password for
# 'http://<userinfo>@host'` — went to the converge's journal unredacted
# (backlog 37497977).
#
# THE VALUE STAYS IN THE FILE. It is held in shell variables and moved
# only by builtins (printf -v, parameter expansion) or on a pipe's stdin
# (base64), so it is on no argv `ps` can read; this script prints only
# lengths; READER's stderr is printed with every `scheme://userinfo@` and
# every literal copy of the userinfo replaced by <redacted>. Never
# `set -x` here.
set -uo pipefail
umask 077

if [ $# -lt 2 ]; then
    echo "usage: forge-auth-header.sh HEADER_FILE READER [ARGS...]" >&2
    exit 2
fi
hdr="$1"
shift

# Empty BEFORE anything can fail, so no path out of here leaves a
# previous run's credential for protect-main to use.
: >"$hdr" || exit 2

err_file="$(mktemp -t forge-auth-err.XXXXXX)" || exit 2
trap 'rm -f "$err_file"' EXIT

rc=0
out="$("$@" 2>"$err_file")" || rc=$?
url="${out%%$'\n'*}"

# The userinfo: http(s) only, the authority is everything up to the first
# `/` after the scheme, and the userinfo ends at the authority's LAST `@`
# (a raw `@` in a password is not legal, but a lenient cut costs nothing).
# An `@` further along is part of the path, not a credential.
userinfo=""
case "$url" in
    http://* | https://*)
        rest="${url#*://}"
        authority="${rest%%/*}"
        case "$authority" in
            *@*) userinfo="${authority%@*}" ;;
        esac
        ;;
esac

# url-decode: escape any literal backslash first, so `%b` expands only
# the `\xHH` this makes out of each `%HH`.
decoded="${userinfo//\\/\\\\}"
decoded="${decoded//%/\\x}"
printf -v decoded '%b' "$decoded"

# READER's own words, kept (quiet is a loan against the next diagnosis)
# but never with the credential in them.
if [ -s "$err_file" ]; then
    err="$(<"$err_file")"
    for secret in "$userinfo" "${userinfo#*:}" "$decoded" "${decoded#*:}"; do
        [ -n "$secret" ] && err="${err//"$secret"/<redacted>}"
    done
    printf '%s\n' "$err" |
        sed -E -e 's#://[^/@[:space:]]+@#://<redacted>@#g' -e 's/^/forge-auth-header: reader: /' >&2
fi

if [ "$rc" -ne 0 ]; then
    echo "forge-auth-header: no credential: the remote URL could not be read (rc $rc); the header file is left empty" >&2
    exit 3
fi
if [ -z "$userinfo" ]; then
    echo "forge-auth-header: no credential: the remote URL is not http(s) or carries no userinfo; the header file is left empty" >&2
    exit 3
fi

case "$decoded" in
    *:*) ;;
    *) decoded="$decoded:" ;;
esac
encoded="$(printf '%s' "$decoded" | base64 | tr -d '\n')"
if [ -z "$encoded" ]; then
    echo "forge-auth-header: no credential: base64 produced nothing; the header file is left empty" >&2
    exit 3
fi
printf 'Authorization: Basic %s\n' "$encoded" >"$hdr"
echo "forge-auth-header: Basic header written from the forgejo remote's userinfo (${#decoded} bytes decoded)"
