#!/usr/bin/env bash
# a-sh-script-parses-under-sh.sh — a script that says `#!/bin/sh` is
# parsed by sh, not by the bash that happened to source it in a test.
#
# WHY THIS EXISTS (2026-09-18, estate alarms 356e1885 and 2d5e26fd).
# Train #439 converted `printf '%s' "$1" | sed … | head -n 1` in
# infra/estate/observe-lib.sh to a here-string — the repair the
# producer-coin pin names — and the gate stayed green: the lint that
# exercises that library sources it into BASH, where `<<<` is a word.
# The library's shebang is `#!/bin/sh`, and on the forge and boss-gcp
# /bin/sh is dash:
#
#     observe-lib.sh: 69: Syntax error: redirection unexpected
#
# The forge's host observer died at its next firing (04:16Z), the estate
# alarm's silence sweep filed "forge unobserved" at 05:06Z, and
# boss-gcp's daily observer failed at 10:25Z with exit 2. One bashism,
# two hosts blind, six hours — and the tree had said `#!/bin/sh` the
# whole time. A shebang is a contract about WHICH parser runs the file;
# this lint holds the file to it.
#
# THE RULE. Every tracked `*.sh` whose first line is `#!/bin/sh` (or
# `#!/usr/bin/env sh`) must:
#   * parse under dash (`dash -n`) — the sh every Debian/Ubuntu host in
#     the estate resolves /bin/sh to, and this pod's; and
#   * carry none of the bashisms dash parses as SOMETHING ELSE rather
#     than refusing: `[[`, `set -o pipefail` (dash has no pipefail, so a
#     `set -o pipefail` is "Illegal option" at RUN time, after the
#     parse), `${x//`, `${x^`, `declare`, `local -`, `read -a`,
#     `function name`, `$'…'`.
#
# A bash script is not read: `#!/usr/bin/env bash` may use every one of
# these. The point is not to forbid bash; it is that a file may not
# claim one parser and require another.
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  every sh script parses under sh
#   1  a sh script does not — file:line, the author's to fix
#   3  the tree could not be read, or there is no dash to parse with —
#      a fact about the MACHINE; never `clean`
#
# USAGE
#   infra/lint/a-sh-script-parses-under-sh.sh
#   infra/lint/a-sh-script-parses-under-sh.sh --self-test
set -uo pipefail

NAME="a-sh-script-parses-under-sh"
LINT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh" || exit 3
# shellcheck source=infra/lint/lib/git-answer.sh
. "$LINT_DIR/lib/git-answer.sh" || exit 3
cd "$LINT_DIR/../.." || exit 1

SH="${LINT_SH:-dash}"
command -v "$SH" >/dev/null 2>&1 || {
    echo "$NAME: no '$SH' on this machine — a sh script cannot be parsed here (exit 3, not clean)" >&2
    exit 3
}

# The bashisms dash does not refuse at parse time. One extended regex;
# a `#` comment line is not code. `[[` at a word start; `${x//`, `${x^`,
# `${x,` are substitutions dash reads as bad substitutions only when the
# line runs (`${x##*/}` and `${x%%/*}` are POSIX: the `/` there follows
# `#`/`%`, not the name). `$'…'` only at a word start: a `$'` closing a
# regex ('\.json$') is an anchor.
BASHISM='(^|[^[])\[\[[[:space:]]|set -o pipefail|set -[a-zA-Z]*o pipefail|\$\{[A-Za-z_0-9@]+(//|/|\^|,)|(^|[[:space:];&|(])(declare|typeset|local -[a-zA-Z])[[:space:]]|(^|[[:space:];&|(])read -[a-zA-Z]*a[[:space:]]|(^|[[:space:];])function[[:space:]]+[A-Za-z_]|(^|[[:space:]=(])\$'"'"''

is_sh_script() { # file
    local first
    IFS= read -r first <"$1" || return 1
    case "$first" in
        '#!/bin/sh'|'#!/bin/sh '*|'#!/usr/bin/env sh'|'#!/usr/bin/env sh '*) return 0 ;;
    esac
    return 1
}

# One file's findings as lines of `<line>: <what>`; empty = clean.
findings_in() { # file
    local parse
    # dash -n prints `file: N: Syntax error: …` on stderr and exits 2.
    parse="$("$SH" -n "$1" 2>&1 >/dev/null)"
    if [ -n "$parse" ]; then
        printf '%s\n' "$parse" | sed -n 's/^[^:]*: \([0-9][0-9]*\): /\1: sh refuses to parse it — /p'
    fi
    grep -nE "$BASHISM" "$1" | grep -vE '^[0-9]+:[[:space:]]*#' | sed 's/^\([0-9]*\):\(.*\)$/\1: a bashism sh would run as something else — \2/'
}

# --- self-test ---------------------------------------------------------
# Runs on every invocation: a parser that stopped refusing, or a regex
# that stopped matching, passes every file, and only a fixture it must
# refuse tells that from a clean tree.
self_test() {
    local t
    t="$(mktemp -d)" || { echo "$NAME: cannot make a temp dir for the self-test" >&2; return 1; }
    # shellcheck disable=SC2064
    trap "rm -rf '$t'" RETURN

    printf '#!/bin/sh\n# a comment may say <<< or [[ freely\nat=${1#*x}\n[ "$at" = "$1" ] && at=\nprintf %%s "$at" | sed -n p\n' >"$t/good.sh"
    [ -z "$(findings_in "$t/good.sh")" ] || {
        echo "$NAME: self-test FAILED — a POSIX script was refused:" >&2
        findings_in "$t/good.sh" >&2
        return 1
    }
    is_sh_script "$t/good.sh" || { echo "$NAME: self-test FAILED — #!/bin/sh not recognised" >&2; return 1; }
    printf '#!/usr/bin/env bash\ngrep -q x <<<"$1"\n' >"$t/bash.sh"
    ! is_sh_script "$t/bash.sh" || { echo "$NAME: self-test FAILED — a bash script was taken for sh" >&2; return 1; }

    # Refused: the shape that shipped (#439), and each bashism dash runs
    # rather than refuses.
    printf '#!/bin/sh\nat=$(sed -n p <<<"$1")\n' >"$t/bad1.sh"
    printf '#!/bin/sh\n[[ -f "$1" ]] && echo yes\n' >"$t/bad2.sh"
    printf '#!/bin/sh\nset -euo pipefail\n' >"$t/bad3.sh"
    printf '#!/bin/sh\nx=${1//a/b}\n' >"$t/bad4.sh"
    printf '#!/bin/sh\nf() { local -r x=1; }\n' >"$t/bad5.sh"
    printf '#!/bin/sh\nfunction f { :; }\n' >"$t/bad6.sh"
    local f
    for f in bad1.sh bad2.sh bad3.sh bad4.sh bad5.sh bad6.sh; do
        [ -n "$(findings_in "$t/$f")" ] || {
            echo "$NAME: self-test FAILED — passed:" >&2
            sed 's/^/    /' "$t/$f" >&2
            return 1
        }
    done
    echo "$NAME: self-test ok — a POSIX script passes, a bash script is not read; a here-string, [[, pipefail, a pattern substitution, local -r and function are each refused"
}

if [ "${1:-}" = "--self-test" ]; then self_test; exit $?; fi
self_test || exit 1

# --- the tree ----------------------------------------------------------
files="$(git_answer "$NAME" 0 ls-files -- '*.sh')" || exit $?

scanned=0
findings=0
while IFS= read -r file; do
    [ -n "$file" ] || continue
    [ -f "$file" ] || continue
    is_sh_script "$file" || continue
    scanned=$((scanned + 1))
    while IFS= read -r hit; do
        [ -n "$hit" ] || continue
        findings=$((findings + 1))
        echo "$NAME: $file:$hit" >&2
    done <<EOF
$(findings_in "$file")
EOF
done <<EOF
$files
EOF

if [ "$findings" -gt 0 ]; then
    cat >&2 <<'MSG'

FAIL — the line(s) above sit in a file that says #!/bin/sh and need
bash. On the forge and boss-gcp /bin/sh is dash; a here-string there is
"Syntax error: redirection unexpected" and the unit fails before its
first line runs (observe-lib.sh, 2026-09-18: two hosts unobserved).
Either write it in POSIX sh — parameter expansion (${x#*pat}, ${x%%pat})
instead of a here-string into sed, `[ ]` instead of `[[ ]]`, no
pipefail — or change the shebang to #!/usr/bin/env bash and mean it.
MSG
    exit 1
fi

lint_scanned "$NAME" "$scanned" "sh script(s) parsed under $SH"
echo "$NAME: ok — every #!/bin/sh script parses under sh"
exit 0
