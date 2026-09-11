#!/usr/bin/env bash
# a-fixture-path-cannot-be-a-literal.sh — no Rust code builds a path
# under a SHARED sticky temp directory whose name is fixed.
#
# WHY THIS EXISTS (backlog 74b8bf3d). `/tmp` and `/var/tmp` are mode
# 1777: every account on the box writes into one namespace. A fixture at
# a FIXED name there is therefore one path shared by every uid and every
# process, and this repo has hit the resulting collision FOURTEEN times,
# each found and fixed individually. Catching instances instead of the
# class is what this file ends.
#
# Two structural facts keep it coming back, and neither is going away:
#
#   * builders run in PARALLEL WORKTREES. Worktree isolation isolates the
#     repository, not /tmp. Measured 2026-09-11 14:13Z: a root-owned
#     /tmp/all reappeared that the verifying builder's own suite had not
#     written, and a /proc scan found PID 206844 — another session
#     running `cargo test -p boss-testing --all-features` from a
#     DIFFERENT worktree against the unfixed file. Two cars poisoning
#     each other through one path, live.
#   * the GATE runs as uid 65534 (since #310) and a pod session runs as
#     root, so the same path is contended across uids in BOTH directions.
#     A 65534-first run locks root out just as surely.
#
# The failure mode is the expensive part. `create_dir_all` on an existing
# directory returns Ok REGARDLESS of who owns it, so the fixture reports
# success and the run dies at the first write inside — frames away from
# the cause, with an errno and no path. Instance eleven
# (crates/core/boss-testing/tests/backup_files_its_packet.rs) scored 8/8
# alone and 5 passed / 3 failed as 65534 after a root run had left a
# root-owned /tmp/all behind.
#
# THE RULE. A path rooted at a shared temp directory must carry a token
# that makes it this run's own. Two shapes are refused:
#
#   1. `std::env::temp_dir().join(...)` whose name is a fixed literal, or
#      a `format!` that interpolates nothing unique. This is the shape the
#      packet's title names.
#   2. a string literal opening `"/tmp/<name>` or `"/var/tmp/<name>`.
#
# ACCEPTED, and each is the shape of the fix:
#
#   * `boss_testing::scratch::{scratch_dir,scratch_path}` — THE answer.
#     It carries the uid AND the pid: the pid removes every concurrent
#     collision, the uid turns "a leftover I cannot remove" into "a
#     leftover I can always remove". Read its module note; it is the
#     reasoning this lint enforces.
#   * a name interpolating `std::process::id()`, `Uuid::new_v4()`, or
#     `scratch_path`. The pid alone is accepted because it removes the
#     concurrent half, which is the half that has bitten; `scratch` is
#     the complete fix and is what the message recommends.
#   * `temp_dir().join(<expression>)` — a computed name is not a literal
#     and this lint does not judge it.
#   * prose. A comment may name a fixed path; several must, to tell the
#     story.
#
# WHY RUST ONLY, measured rather than assumed. The same rule in shell and
# in the cluster manifests is UNDECIDABLE from the text, because which
# MACHINE a path names is not in the text: `/tmp/all` in
# infra/cluster/manifests/boss-backup.yaml and `/tmp/stamp` in
# infra/forge/boss-ci/Dockerfile are a CONTAINER's own /tmp — a fresh
# single-uid filesystem per run, where a fixed name is correct and
# private. Applying the existing write-shaped proxy from
# a-lint-writes-only-where-it-owns.sh to all of infra/ outside the lint
# roster yields 0 true positives and 3 false positives (boss-backup.yaml,
# boss-dev.yaml, boss-ci/Dockerfile — all three container-internal). A
# lint that cries wolf gets exempted into uselessness, so the shell half
# is deliberately NOT here:
#
#   * infra/lint/ is already covered, by the write-shaped proxy in
#     a-lint-writes-only-where-it-owns.sh, over the one directory whose
#     scripts provably run on the host.
#   * the manifest -> test replay, which is where a container's fixed
#     path actually lands on a host, is covered at the consuming layer by
#     `assert_rebased` in backup_files_its_packet.rs. Packet fcbe1bc5
#     tracks the manifest itself.
#
# A Rust test, by contrast, always runs on the host this lint is checking
# for, so the judgement is in the text and no exemption is needed to
# express it.
#
# THE EXEMPTION, and why it is not a list in here. Some fixed temp paths
# are legitimate: a literal that names ANOTHER machine's path (a container
# script's, quoted in order to be rebased), an expectation string about a
# message, a pinned production default (`/var/tmp/boss-converge-hold` is
# a real one — ops verbs run as root with no HOME and use fixed paths
# deliberately). Which of those a line is cannot be derived from the
# text, so the CODE declares it, at the site:
#
#     // shared-tmp-ok: <at least three words of reason>
#
# A list in this file would be the CLAUDE.md §9a defect this lint exists
# to prevent — a second copy of a fact, drifting from the code it
# describes, edited on one contended tail line. An inline marker cannot
# drift from the line it sits on, and it carries its reason by
# construction: fewer than three words and it is not a marker.
#
# The marker covers the hit line, the contiguous comment block directly
# above it, and the enclosing item's own comment block (the nearest
# preceding line at a shallower indent, plus the comments above THAT).
# Deliberately no wider: a declaration that reached across siblings would
# silently cover a path added later elsewhere in the file, which is
# exactly the hole an allowlist has.
#
# WHAT THIS CANNOT SEE, stated rather than left to be discovered:
#   * a bare `"/tmp"` with no name after it — `temp_dir()`'s own value,
#     and the base of a legitimate `join`. `unwrap_or("/tmp")` in
#     boss-cli's upgrade path is the real instance.
#   * a path assembled from pieces across lines so that no single line
#     holds `"/tmp/<name>`.
#   * whether a literal is ever actually handed to the filesystem. It
#     refuses the SHAPE, because instance eleven's literal was handed to
#     `str::replace` and reached the filesystem two frames later.
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  the tree was read and no Rust file builds a fixed temp path
#   1  the tree was read and a violation was found — a fact about the
#      BRANCH, which is the author's to fix
#   3  the tree was never read — a fact about the MACHINE, which no
#      author can fix by editing code, and which must never be reported
#      as `clean`
#
# USAGE
#   infra/lint/a-fixture-path-cannot-be-a-literal.sh
#   infra/lint/a-fixture-path-cannot-be-a-literal.sh --self-test
set -uo pipefail

NAME="a-fixture-path-cannot-be-a-literal"
LINT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$LINT_DIR/../.." || exit 1

# The shared helper when the tree has it, an equivalent inline when it
# does not. The car that adds lib/git-answer.sh is in flight alongside
# this one and two cars cannot both create one file, so this survives
# either merge order and uses the shared definition the moment it lands.
# The exit code and the marker text are a PROTOCOL, not a duplicated
# fact — infra/safe-cargo.sh, infra/forge/journal-read.sh,
# infra/forge/run-car-probe.sh and infra/cluster/undeclared-objects.sh
# each already speak exit 3 for "could not answer".
if [ -r "$LINT_DIR/lib/git-answer.sh" ]; then
    # shellcheck source=/dev/null
    . "$LINT_DIR/lib/git-answer.sh"
fi
if ! declare -F git_answer >/dev/null 2>&1; then
    LINT_CANNOT_ANSWER=3
    git_answer() { # <lint> <answer-statuses> <git args...>
        local lint="$1" answers="$2"
        shift 2
        local err out status
        err="$(mktemp)" || {
            printf '%s: CANNOT ANSWER — no writable temp dir, so `git %s` could not be run with its stderr kept.\n' \
                "$lint" "$*" >&2
            return "$LINT_CANNOT_ANSWER"
        }
        out="$(git "$@" 2>"$err")"
        status=$?
        case ",${answers}," in
            *",${status},"*)
                [ -s "$err" ] && cat "$err" >&2
                [ -n "$out" ] && printf '%s\n' "$out"
                rm -f "$err"
                return "$status"
                ;;
        esac
        {
            printf '%s: CANNOT ANSWER — `git %s` exited %s, so nothing was read.\n' \
                "$lint" "$*" "$status"
            if [ -s "$err" ]; then
                printf '  git said:\n'
                sed 's/^/    /' "$err"
            else
                printf '  git said nothing on stderr, which is itself the finding.\n'
            fi
            printf '  An INFRASTRUCTURE refusal (exit %s), not a verdict on the branch:\n' \
                "$LINT_CANNOT_ANSWER"
            printf '  no file was examined, so neither `clean` nor a violation can be\n'
            printf '  claimed. Fix the git environment on this machine and re-run.\n'
        } >&2
        rm -f "$err"
        return "$LINT_CANNOT_ANSWER"
    }
fi

# --- the scanner -------------------------------------------------------
# One file's findings, as `<line>\t<path>\t<why>`. Empty output = clean.
#
# No `{n}` interval anywhere: mawk — the awk in the CI image — answers an
# interval by matching NOTHING, which is a scanner that reads as a clean
# tree. The self-test below exists because that failure is invisible from
# the outside.
findings_in() { # file
    awk '
        function indent_of(s,   i, n) {
            n = 0
            for (i = 1; i <= length(s); i++) {
                if (substr(s, i, 1) == " " || substr(s, i, 1) == "\t") n++
                else break
            }
            return n
        }
        function is_comment(s) {
            return (s ~ /^[ \t]*\/\// || s ~ /^[ \t]*\/\*/ || s ~ /^[ \t]*\*/)
        }
        # A marker is an intent DECLARATION, so it must carry a reason:
        # three words or it is a rubber stamp.
        function has_marker(s,   p, rest, n, a, k, words) {
            p = index(s, "shared-tmp-ok:")
            if (p == 0) return 0
            rest = substr(s, p + 14)
            n = split(rest, a, /[ \t]+/)
            words = 0
            for (k = 1; k <= n; k++) if (a[k] != "") words++
            return (words >= 3)
        }
        # The hit line, the comment block directly above it, and the
        # enclosing item (nearest shallower line) plus ITS comment block.
        #
        # Deliberately no wider. A declaration that reached across
        # siblings would silently cover a path added later elsewhere in
        # the file, which is exactly the hole a lint-side allowlist has,
        # so the walk up stops at the first `}` — you cannot reach a
        # declaration by leaving the block the hit is in — and a hit at
        # column 0 has no enclosing item at all.
        function declared(i,   j, ind) {
            if (has_marker(L[i])) return 1
            j = i - 1
            while (j >= 1 && is_comment(L[j])) {
                if (has_marker(L[j])) return 1
                j--
            }
            if (j < 1) return 0
            ind = indent_of(L[i])
            if (ind == 0) return 0
            while (j >= 1) {
                if (L[j] ~ /^[ \t]*$/) { j--; continue }
                # A comment passed on the way up is still inside the same
                # item and above the hit, so it counts: a declaration
                # written between an `assert_eq!(` and its arguments is a
                # natural place for one, and refusing it there would send
                # authors to restructure working code.
                if (is_comment(L[j])) {
                    if (has_marker(L[j])) return 1
                    j--
                    continue
                }
                if (index(L[j], "}") > 0) return 0
                if (indent_of(L[j]) < ind) break
                j--
            }
            if (j < 1) return 0
            if (has_marker(L[j])) return 1
            j--
            while (j >= 1 && is_comment(L[j])) {
                if (has_marker(L[j])) return 1
                j--
            }
            return 0
        }
        function opens(s,   t) { t = s; return gsub(/\(/, "", t) }
        function closes(s,   t) { t = s; return gsub(/\)/, "", t) }
        # The whole `temp_dir().join(...)` expression, which may wrap:
        # accumulate until the parentheses opened on the first line are
        # closed. Capped, so a pathological file cannot run away; past the
        # cap the accumulated text is used as-is.
        function statement(i,   j, acc, depth) {
            acc = L[i]
            depth = opens(L[i]) - closes(L[i])
            j = i
            while (depth > 0 && j < NR && j < i + 12) {
                j++
                acc = acc " " L[j]
                depth += opens(L[j]) - closes(L[j])
            }
            return acc
        }
        function quoted_after(s, anchor,   p, rest) {
            p = index(s, anchor)
            if (p == 0) return ""
            rest = substr(s, p + length(anchor))
            if (match(rest, /"[^"]*"/) == 0) return ""
            return substr(rest, RSTART, RLENGTH)
        }
        { L[NR] = $0 }
        END {
            for (i = 1; i <= NR; i++) {
                line = L[i]
                # Prose. A comment may name a fixed path, and several must.
                if (is_comment(line)) continue

                # Shape 2 — a literal under the shared sticky root.
                if (match(line, /"\/tmp\/[A-Za-z0-9_.+-]/) > 0 ||
                    match(line, /"\/var\/tmp\/[A-Za-z0-9_.+-]/) > 0) {
                    if (!declared(i)) {
                        path = substr(line, RSTART)
                        if (match(path, /"[^"]*"/) > 0) path = substr(path, RSTART, RLENGTH)
                        printf "%d\t%s\ta literal under the shared sticky temp root\n", i, path
                    }
                    continue
                }

                # Shape 1 — the packet`s named shape: a temp_dir() path
                # whose name is a fixed literal or a format! that
                # interpolates nothing unique. Only a literal or a
                # format! is judged; a computed name is not a literal.
                if (line ~ /temp_dir\(\)[ \t]*\.join\([ \t]*"/ ||
                    line ~ /temp_dir\(\)[ \t]*\.join\([ \t]*format!\(/) {
                    stmt = statement(i)
                    if (stmt !~ /process::id/ && stmt !~ /Uuid::new_v4/ &&
                        stmt !~ /scratch_dir/ && stmt !~ /scratch_path/ &&
                        stmt !~ /current_uid/) {
                        if (!declared(i)) {
                            path = quoted_after(stmt, ".join(")
                            if (path == "") path = "<the joined name>"
                            printf "%d\t%s\ta temp_dir() name with no per-process token\n", i, path
                        }
                    }
                }
            }
        }
    ' "$1"
}

# --- self-test ---------------------------------------------------------
# Runs on EVERY invocation, not only under a flag: a scanner whose regex
# has stopped matching passes every file, and the only way to tell that
# from a clean tree is to hand it something it must refuse. Fixtures live
# in a `mktemp -d` this run owns — it would be absurd for this file to
# commit the defect it forbids — and never under infra/lint/, where a
# file is discovered as a real lint by gate.sh and by the conductor.
#
# The fixed root is passed to printf as an ARGUMENT rather than spelled
# in these lines, which is the technique a-lint-writes-only-where-it-owns
# already uses: it keeps this file honest under its own sibling lint.
self_test() {
    local t r hits f
    r="/tmp"
    t="$(mktemp -d)" || { echo "$NAME: cannot make a temp dir for the self-test" >&2; return 1; }
    # shellcheck disable=SC2064
    trap "rm -rf '$t'" RETURN

    # Each accepted shape, including a prose mention and a computed name.
    {
        printf '//! A comment may name %s/all and must not trip the scanner.\n' "$r"
        printf 'fn a() -> PathBuf { scratch::scratch_dir("boss-a") }\n'
        printf 'fn b() -> PathBuf { std::env::temp_dir().join(format!("b-{}", std::process::id())) }\n'
        printf 'fn c() -> PathBuf { std::env::temp_dir().join(format!("c-{}", Uuid::new_v4())) }\n'
        printf 'fn d(n: &Path) -> PathBuf { std::env::temp_dir().join(n) }\n'
        printf 'fn e() -> PathBuf {\n'
        printf '    std::env::temp_dir().join(format!(\n'
        printf '        "e-{tag}-{}",\n'
        printf '        std::process::id()\n'
        printf '    ))\n'
        printf '}\n'
        printf '/// shared-tmp-ok: a declared intent with a real reason attached.\n'
        printf 'fn f() -> String { String::from("%s/declared-fixture") }\n' "$r"
        # A declaration written between a macro`s opener and its
        # arguments — the shape spool.rs needs.
        printf 'fn g() {\n'
        printf '    assert_eq!(\n'
        printf '        // shared-tmp-ok: a pinned operational location, not a fixture.\n'
        printf '        SPOOL_DIR_DEFAULT,\n'
        printf '        "/var%s/boss-estate-spool",\n' "$r"
        printf '    );\n'
        printf '}\n'
    } >"$t/good.rs"
    hits="$(findings_in "$t/good.rs")"
    [ -z "$hits" ] || {
        echo "$NAME: self-test FAILED — an accepted shape was flagged:" >&2
        printf '%s\n' "$hits" >&2
        return 1
    }

    # Each refused shape is one this repo has actually shipped.
    printf 'let d = std::env::temp_dir().join("boss-thing-test");\n'           >"$t/bad1.rs"
    printf 'let r = std::env::temp_dir().join(format!("boss-pubreq-{name}"));\n' >"$t/bad2.rs"
    printf 'let p = PathBuf::from("%s/boss-fixture");\n'              "$r"     >"$t/bad3.rs"
    printf 'let q = String::from("/var/tmp/boss-fixture");\n'                  >"$t/bad4.rs"
    printf 'script.replace("%s/k", &format!("{}/k", dir.display()));\n' "$r"   >"$t/bad5.rs"
    {
        printf '// shared-tmp-ok\n'
        printf 'let p = PathBuf::from("%s/bare-marker");\n' "$r"
    } >"$t/bad6.rs"
    # A declaration on one item must not reach the next. Both shapes:
    # a braced body, where the walk up must stop at the closing brace,
    # and a one-liner, where the hit sits at column 0 and so has no
    # enclosing item to inherit from.
    {
        printf '/// shared-tmp-ok: this reason belongs to one only\n'
        printf 'fn one() -> String {\n'
        printf '    String::from("%s/one")\n' "$r"
        printf '}\n'
        printf '\n'
        printf 'fn two() -> String {\n'
        printf '    String::from("%s/two")\n' "$r"
        printf '}\n'
    } >"$t/bad7.rs"
    {
        printf '/// shared-tmp-ok: this reason belongs to one only\n'
        printf 'fn one() -> String { String::from("%s/one") }\n' "$r"
        printf '\n'
        printf 'fn two() -> String { String::from("%s/two") }\n' "$r"
    } >"$t/bad8.rs"
    for f in bad1 bad2 bad3 bad4 bad5 bad6 bad7 bad8; do
        [ -n "$(findings_in "$t/$f.rs")" ] || {
            echo "$NAME: self-test FAILED — the scanner passed:" >&2
            sed 's/^/    /' "$t/$f.rs" >&2
            return 1
        }
    done
    # bad7/bad8's FIRST function is declared and must not be reported;
    # only the undeclared sibling may be. Exactly one finding each, or
    # the marker is either leaking across items or not covering its own.
    for f in bad7 bad8; do
        case "$(findings_in "$t/$f.rs")" in
            *$'\n'*) echo "$NAME: self-test FAILED — a marker did not cover its own item in $f:" >&2
                     findings_in "$t/$f.rs" >&2; return 1 ;;
        esac
    done

    echo "$NAME: self-test ok — six accepted shapes (scratch, a pid, a Uuid, a computed name, a declared intent, a declaration inside an argument list) and a prose mention pass; eight refused shapes (a literal join, a tokenless format!, /tmp and /var/tmp literals, a rebase argument, a reasonless marker, and a declaration leaking to a sibling in both brace and one-line form) are each named"
}

if [ "${1:-}" = "--self-test" ]; then self_test; exit $?; fi
self_test || exit 1

# --- the tree ----------------------------------------------------------
# git, so an untracked scratch file cannot buy a finding and a build
# artifact under target/ cannot be scanned. `git_answer` is what keeps a
# git that refuses from reading as a clean tree.
files="$(git_answer "$NAME" 0 ls-files -- '*.rs')" || exit $?

scanned=0
findings=0
while IFS= read -r file; do
    [ -n "$file" ] || continue
    [ -f "$file" ] || continue
    scanned=$((scanned + 1))
    while IFS=$'\t' read -r lineno path why; do
        [ -n "${lineno:-}" ] || continue
        findings=$((findings + 1))
        echo "$NAME: $file:$lineno — $why: $path" >&2
    done < <(findings_in "$file")
done <<EOF
$files
EOF

if [ "$findings" -gt 0 ]; then
    cat >&2 <<'MSG'

FAIL — the finding(s) above build a fixture at a FIXED path under a
world-writable sticky directory. /tmp and /var/tmp are mode 1777, so that
is one path shared by every uid and every process on the box. This repo
has hit the resulting collision fourteen times; the gate runs as uid
65534 and a pod session runs as root, and builders run in parallel
worktrees, so it is contended in both directions and concurrently.

THE FIX, in order of preference:

  1. `boss_testing::scratch` — scratch_dir / scratch_path / write_file /
     write_exec / create_dir. The root carries the uid AND the pid, and
     the writes name their path when they fail:

         let dir = scratch::scratch_dir("boss-thing-test");
         scratch::write_file(&dir.join("fixture"), body);

  2. In a crate that cannot depend on boss-testing, interpolate
     `std::process::id()` (and ideally the uid) into the name yourself.

  3. If the literal is LEGITIMATE — it names another machine's path (a
     container script's, quoted in order to be rebased), an expectation
     string about a message, or a deliberate operational location like
     /var/tmp/boss-converge-hold — declare that at the site:

         // shared-tmp-ok: <at least three words saying why>

     The marker covers the line it sits on, the comment block directly
     above it, and the enclosing item`s own comment block. There is no
     allowlist in the lint on purpose (CLAUDE.md §9a): a second copy of
     this judgement would drift from the code it describes.
MSG
    exit 1
fi

echo "$NAME: ok — $scanned Rust file(s) read, none builds a fixture at a fixed path under a shared temp directory"
exit 0
