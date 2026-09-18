#!/usr/bin/env bash
# no-employee-id-literal.sh — production code does not name a person.
#
# WHY THIS EXISTS (backlog 3c23662d, design 42277636 first wave, audit
# H6). On 2026-09-18 fourteen production sites carried `"emp-david"` as
# the owner of every packet the platform files — every car
# (Tier-1 boss-jobs `car.rs`), every gate-run, every design doc, every
# conductor alarm, every estate/cadence/sensor/DNS alarm, the forge
# watchdog's alert and the nightly install-smoke red — and the workflow
# bootstrap walk signed its synthetic approvals `"emp-cto"`. Each one was
# a fact about ONE deployment written into code every deployment runs:
# on the playground the id names nobody, on an OSS install it names
# nobody, and the day Algedonic LLC hires a second operator (decided
# 2026-09-16) every alarm still lands on the first one, forever, unless
# fourteen files are edited in step.
#
# THE RULE. Who the platform owner IS is registry data — the people
# roster answers it (`role=platform-admin`, the first hire), and
# `boss_core::platform_owner` is the ONE port every filer reads it
# through (`BOSS_PLATFORM_OWNER` is the explicit override a launcher or
# unit may carry). A signer is the actor doing the signing, read off the
# header the call already sends. So a literal `emp-<name>` in production
# code is refused: an employee id is READ, never written.
#
# WHAT IS EXEMPT, and each is a judgement a reader can check:
#   * tests — `*/tests/*`, `*.test.*`, `*.spec.*`, `*/testdata/*`,
#     `*/fixtures/*`, and every `#[cfg(test)]` region of a Rust file
#     (a top-level one runs to the next column-0 `}`; an indented one to
#     the closing brace at its own indent, or the one statement it
#     guards). A test names people because it stages a roster.
#   * example seeds — `examples/` and `crates/tenants/` (the two example
#     tenants' `prepare` modules ARE their seed data: emp-cto, emp-coo and
#     emp-ceo are Algedonic Ales' founding operators, not the platform's).
#   * test infrastructure — `crates/core/boss-testing/` (its `src/` is
#     the harness every test runs under; `emp-smoke` is a fixture user).
#   * prose — a comment may name a person to tell the story; several do.
#   * the PLATFORM identities, which are not people: `emp-bootstrap-admin`
#     (the deployment's own transitional identity, declared ONCE as
#     `BOOTSTRAP_IDENTITY` in boss-people and retired by the first real
#     platform-admin hire) and every `id` the operator baseline hires in
#     infra/operator-baseline/operator_hires.toml (`emp-audit` today) —
#     READ from that file, not listed here, so the file stays the one
#     definition (CLAUDE.md §9a).
#   * a DECLARED site: a literal that is a placeholder the code writes
#     for someone else to rename (the tenant scaffold) or an example
#     tenant's seeded id the sim round-robins (boss-sim) says so where
#     it sits — on the line, or in the five lines above it:
#
#         // employee-id-ok: <at least three words of reason>
#
#     No list in this file, on purpose: a marker cannot drift from the
#     line it sits on, and fewer than three words is not a reason.
#
# Files read: `*.rs *.sh *.ts *.svelte *.toml *.yaml *.yml` under
# crates/ infra/ apps/ libs/. SQL is not read: a migration that seeds an
# identity is data, and migrations are append-only anyway.
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  the tree was read and no production line names an employee
#   1  the tree was read and a violation was found — the author's to fix
#   3  the tree was never read — a fact about the MACHINE; never `clean`
#
# USAGE
#   infra/lint/no-employee-id-literal.sh
#   infra/lint/no-employee-id-literal.sh --self-test
set -uo pipefail

NAME="no-employee-id-literal"
LINT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh"
cd "$LINT_DIR/../.." || exit 1

# shellcheck source=/dev/null
. "$LINT_DIR/lib/git-answer.sh"

# The platform identities: the bootstrap identity boss-people declares,
# plus every id the operator baseline hires — read from the file, so a
# baseline hire added there is exempt here without an edit to this lint.
BASELINE="infra/operator-baseline/operator_hires.toml"
PLATFORM_IDS="emp-bootstrap-admin"
if [ -r "$BASELINE" ]; then
    while IFS= read -r id; do
        [ -n "$id" ] && PLATFORM_IDS="$PLATFORM_IDS $id"
    done < <(sed -n 's/^id = "\(emp-[a-z0-9-]*\)"$/\1/p' "$BASELINE")
fi

# --- the scanner -------------------------------------------------------
# One file's findings, as `<line>\t<literal>`. Empty output = clean.
# `kind` is rs|sh|ts — which comment syntax, and whether cfg(test)
# regions exist. No `{n}` intervals: mawk answers one by matching
# nothing (see a-fixture-path-cannot-be-a-literal.sh).
findings_in() { # file kind
    awk -v kind="$2" -v platform="$PLATFORM_IDS" '
        function indent_of(s,   i, n) {
            n = 0
            for (i = 1; i <= length(s); i++) {
                if (substr(s, i, 1) == " " || substr(s, i, 1) == "\t") n++
                else break
            }
            return n
        }
        function is_comment(s) {
            if (kind == "sh") return (s ~ /^[ \t]*#/)
            return (s ~ /^[ \t]*\/\// || s ~ /^[ \t]*\/\*/ || s ~ /^[ \t]*\*/)
        }
        # The code half of a line: a trailing `//` comment (rs/ts) or
        # ` #` comment (sh) is prose too. A `//` after a `:` is a URL.
        function code_of(s,   p, q) {
            if (kind == "sh") {
                p = index(s, " #")
                return (p > 0) ? substr(s, 1, p - 1) : s
            }
            q = 0
            while ((p = index(substr(s, q + 1), "//")) > 0) {
                q += p
                if (q == 1 || substr(s, q - 1, 1) != ":") return substr(s, 1, q - 1)
                q += 1
            }
            return s
        }
        # A marker is an intent DECLARATION and must carry a reason:
        # three words or it is a rubber stamp.
        function has_marker(s,   p, rest, n, a, k, words) {
            p = index(s, "employee-id-ok:")
            if (p == 0) return 0
            rest = substr(s, p + 15)
            n = split(rest, a, /[ \t]+/)
            words = 0
            for (k = 1; k <= n; k++) if (a[k] != "") words++
            return (words >= 3)
        }
        function declared(i,   j) {
            for (j = i; j >= 1 && j > i - 6; j--) if (has_marker(L[j])) return 1
            return 0
        }
        BEGIN { n = split(platform, ids, " "); for (k = 1; k <= n; k++) is_platform[ids[k]] = 1 }
        { L[NR] = $0 }
        END {
            skipping = 0
            for (i = 1; i <= NR; i++) {
                line = L[i]
                if (skipping) {
                    # A top-level region ends at the next column-0 `}`;
                    # an indented one at the `}` on its own indent.
                    if (skip_indent == 0 && line ~ /^}/) { skipping = 0 }
                    else if (skip_indent > 0 && indent_of(line) == skip_indent && line ~ /^[ \t]*}/) { skipping = 0 }
                    continue
                }
                if (kind == "rs" && line ~ /^[ \t]*#\[cfg\(test\)\]/) {
                    skip_indent = indent_of(line)
                    # The guarded item: attributes may follow; a `;`
                    # statement (a `use`) is one line, a `{` opens a region.
                    j = i + 1
                    while (j <= NR && L[j] ~ /^[ \t]*#\[/) j++
                    if (j <= NR && L[j] ~ /;[ \t]*$/) { i = j; continue }
                    skipping = 1
                    i = j
                    continue
                }
                if (is_comment(line)) continue
                rest = code_of(line)
                while (match(rest, /emp-[a-z][a-z0-9-]*/) > 0) {
                    lit = substr(rest, RSTART, RLENGTH)
                    rest = substr(rest, RSTART + RLENGTH)
                    if (lit in is_platform) continue
                    # An id a MESSAGE quotes as an example (`emp-032`) is
                    # prose in a string, not a person named. Digits only.
                    if (lit ~ /^emp-[0-9]+$/) continue
                    if (declared(i)) break
                    printf "%d\t%s\n", i, lit
                    break
                }
            }
        }
    ' "$1"
}

kind_of() { # path
    case "$1" in
        *.rs) echo rs ;;
        *.sh|*.toml|*.yaml|*.yml) echo sh ;;
        *) echo ts ;;
    esac
}

# The exemptions, as one predicate so the self-test and the tree read
# agree on them.
exempt() { # path
    case "$1" in
        */tests/*|*.test.*|*.spec.*|*/testdata/*|*/fixtures/*) return 0 ;;
        examples/*|crates/tenants/*|crates/core/boss-testing/*) return 0 ;;
        "infra/lint/$NAME.sh") return 0 ;;
    esac
    return 1
}

# --- self-test ---------------------------------------------------------
# Runs on every invocation: a scanner whose regex stopped matching
# passes every file, and only a fixture it must refuse tells that from
# a clean tree. Fixtures live in a mktemp dir this run owns. The literal
# is passed to printf as an ARGUMENT so this file does not name a person
# in a line the scanner would read.
self_test() {
    local t p hits saved
    p="emp-someone"
    t="$(mktemp -d)" || { echo "$NAME: cannot make a temp dir for the self-test" >&2; return 1; }
    # The platform set as the self-test stages it — the bootstrap
    # identity plus one "baseline hire" — restored on the way out. The
    # DERIVATION from the real baseline file is proven by the tree read
    # (and pinned by boss-testing's no_employee_id_literal_sh.rs), not
    # here: a self-test that read the live file would pass or fail on
    # what that file happens to hire.
    saved="$PLATFORM_IDS"
    PLATFORM_IDS="emp-bootstrap-admin $p-baseline"
    # shellcheck disable=SC2064
    trap "rm -rf '$t'; PLATFORM_IDS='$saved'" RETURN

    # Accepted: prose, a cfg(test) module (top-level and indented), a
    # cfg(test) use line followed by clean production code, the
    # bootstrap identity, a numeric example id in a message.
    {
        printf '//! A doc comment may say %s to tell the story.\n' "$p"
        printf 'pub fn owner() -> String { read_it() } // not %s any more\n' "$p"
        printf 'const BOOT: &str = "emp-bootstrap-admin";\n'
        printf 'const BASELINE: &str = "%s-baseline";\n' "$p"
        printf 'fn scaffold() -> String {\n'
        printf '    // employee-id-ok: a placeholder the operator renames\n'
        printf '    format!(\n'
        printf '        r#"[\n'
        printf '  {{\n'
        printf '    "id": "%s-placeholder",\n' "$p"
        printf '  }}]"#\n'
        printf '    )\n'
        printf '}\n'
        printf 'const HINT: &str = "an employee id, e.g. emp-032";\n'
        printf 'impl X {\n'
        printf '    #[cfg(test)]\n'
        printf '    fn stage() -> &%sstatic str {\n' "'"
        printf '        "%s"\n' "$p"
        printf '    }\n'
        printf '    fn prod() -> u8 { 1 }\n'
        printf '}\n'
        printf '#[cfg(test)]\n'
        printf 'use something::%s_helper;\n' "$p"
        printf 'fn after_the_use() -> u8 { 2 }\n'
        printf '#[cfg(test)]\n'
        printf 'mod tests {\n'
        printf '    const WHO: &str = "%s";\n' "$p"
        printf '    fn f() {\n'
        printf '        assert_eq!(WHO, "%s");\n' "$p"
        printf '    }\n'
        printf '}\n'
    } >"$t/good.rs"
    hits="$(findings_in "$t/good.rs" rs)"
    [ -z "$hits" ] || {
        echo "$NAME: self-test FAILED — an accepted shape was flagged:" >&2
        printf '%s\n' "$hits" >&2
        return 1
    }
    {
        printf '# a comment may say %s\n' "$p"
        printf 'OWNER="$(boss-sor-read /api/people?role=platform-admin)"\n'
    } >"$t/good.sh"
    hits="$(findings_in "$t/good.sh" sh)"
    [ -z "$hits" ] || {
        echo "$NAME: self-test FAILED — an accepted shell shape was flagged:" >&2
        printf '%s\n' "$hits" >&2
        return 1
    }

    # Refused: each shape this repo actually shipped.
    printf '    "owner_id": "%s",\n' "$p"                      >"$t/bad1.rs"
    printf '        out.insert("signed_by".into(), json!("%s"));\n' "$p" >"$t/bad2.rs"
    printf '    printf %s{"owner_id":"%s"}%s\n' "'" "$p" "'"    >"$t/bad3.sh"
    printf 'audience = { individual = "%s" }\n' "$p"           >"$t/bad4.toml"
    {
        # A cfg(test) region must END: a literal after it is production.
        printf '#[cfg(test)]\n'
        printf 'mod tests {\n'
        printf '    const WHO: &str = "%s";\n' "$p"
        printf '}\n'
        printf 'const OWNER: &str = "%s";\n' "$p"
    } >"$t/bad5.rs"
    {
        printf '// employee-id-ok\n'
        printf 'const OWNER: &str = "%s";\n' "$p"
    } >"$t/bad6.rs"
    {
        # A marker six lines up is out of reach: it must sit with its line.
        printf '// employee-id-ok: too far from its line\n'
        printf 'fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\nfn e() {}\n'
        printf 'const OWNER: &str = "%s";\n' "$p"
    } >"$t/bad7.rs"
    local f
    for f in bad1.rs bad2.rs bad3.sh bad4.toml bad5.rs bad6.rs bad7.rs; do
        [ -n "$(findings_in "$t/$f" "$(kind_of "$f")")" ] || {
            echo "$NAME: self-test FAILED — the scanner passed:" >&2
            sed 's/^/    /' "$t/$f" >&2
            return 1
        }
    done
    # The exemption predicate, both ways.
    exempt "crates/core/x/tests/y.rs" && exempt "examples/brewery/seed.toml" \
        && exempt "crates/tenants/boss-brewery-engine/src/prepare/tenant_data.rs" \
        && ! exempt "crates/core/boss-jobs/src/car.rs" \
        && ! exempt "infra/forge/alert-lib.sh" || {
        echo "$NAME: self-test FAILED — the exemption predicate answers wrongly" >&2
        return 1
    }
    echo "$NAME: self-test ok — prose, cfg(test) regions (module, indented item, a guarded use), the platform identities, a numeric example and a declared placeholder pass; a Rust owner, a signer, a shell body, a TOML audience, a literal after a test module, a reasonless marker and a marker out of reach are each refused"
}

if [ "${1:-}" = "--self-test" ]; then self_test; exit $?; fi
self_test || exit 1

# --- the tree ----------------------------------------------------------
# Only the tracked files that hold a candidate at all go through the
# scanner (one awk per file is 2.5 s over the whole tree, 0.2 s over
# the ~120 that mention an id). `git grep -l` answers 1 for "none",
# which is a clean tree, and >1 for "could not look" — git_answer keeps
# those apart.
files="$(git_answer "$NAME" 0,1 grep -l -E 'emp-[a-z]' -- \
    'crates/*.rs' 'crates/*.sh' 'crates/*.toml' \
    'infra/*.sh' 'infra/*.toml' 'infra/*.yaml' 'infra/*.yml' \
    'apps/*.ts' 'apps/*.svelte' 'libs/*.ts' 'libs/*.svelte')"
status=$?
case "$status" in 0|1) ;; *) exit "$status" ;; esac

scanned=0
findings=0
while IFS= read -r file; do
    [ -n "$file" ] || continue
    [ -f "$file" ] || continue
    exempt "$file" && continue
    scanned=$((scanned + 1))
    while IFS=$'\t' read -r lineno lit; do
        [ -n "${lineno:-}" ] || continue
        findings=$((findings + 1))
        echo "$NAME: $file:$lineno names $lit" >&2
    done < <(findings_in "$file" "$(kind_of "$file")")
done <<EOF
$files
EOF

if [ "$findings" -gt 0 ]; then
    cat >&2 <<'MSG'

FAIL — the line(s) above write an employee id into production code. An
id is a fact about ONE deployment; this code runs on every one. Read
it instead:

  * the owner of a packet the platform files — ask the port:
      boss_core::platform_owner::PlatformOwner (boss-people-client's
      ReqwestPlatformOwner resolves it: the first active platform-admin
      hire, BOSS_PLATFORM_OWNER overriding) and file NOBODY when it
      refuses, so the jobs API resolves the kind's owner_role or
      refuses by name — never a literal.
  * a signer — the actor making the call, read off its own header.
  * in shell — BOSS_PLATFORM_OWNER from the unit, else the people API
    (infra/forge/alert-lib.sh shows the read), else nobody.
  * a test or an example seed — put it under tests/ or the tenant's
    prepare module, where a staged roster belongs.
  * a placeholder the code writes for someone else to rename, or an
    example tenant's seeded id — say so where it sits, on the line or
    within the five above it:  // employee-id-ok: <three words why>
MSG
    exit 1
fi

lint_scanned "$NAME" "$scanned" "production file(s) read for an employee-id literal"
echo "$NAME: ok — no production line names an employee"
exit 0
