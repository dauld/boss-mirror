#!/usr/bin/env bash
# no-personal-address-in-infra.sh — an e-mail address under infra/ is a
# declaration about who may do what to the estate, and infra/ is
# published to the public mirror; a person's private address is neither.
#
# WHY THIS EXISTS (2026-09-18, backlog 67e754cc). The publish-to-github
# review of packet b831512f read the 31 files that would become public
# for the first time and found, in infra/cluster/dns/access.toml, the
# playground's onboarding policy declaring four individuals by PERSONAL
# address — David's own gmail and three third parties admitted as
# visitors on 4/25/2026. infra/lint/no-secrets.sh was right that they
# are not credentials, so the tree read "clean" all the way to the one
# step a person reads, and the publish stopped at sign-off on that
# finding. The declaration now says `include.unmanaged = true` (the
# members are the dashboard's); this lint is what stops the next one at
# pre-flight instead of at the mirror's door.
#
# THE RULE. Every address in a tracked file under infra/ belongs to
#   * the company — `algedonic.dev` or a subdomain of it — or
#   * a RESERVED domain that names nobody: example.com/net/org, any
#     `.example`, `.invalid`, `.test`, `.local`, `.localhost`, or
#   * GitHub's per-account no-reply (`users.noreply.github.com`), which
#     is a commit identity, not a mailbox, or
#   * any `noreply@` / `no-reply@` / `no_reply@` local part: a mailbox
#     nobody reads is a machine's, not a person's. The commit trailer
#     every car carries (`Co-Authored-By: … <noreply@anthropic.com>`)
#     reached infra/ as a platform document on 2026-09-18 and the train
#     gate for #461 refused the assembled tree on it, the day this lint
#     landed.
# A systemd instance name (`wg-quick@wg0.service`) has the shape of an
# address and is not one; unit suffixes are excluded.
#
# THE FINDING names the file, the line and the address's DOMAIN — never
# the local part. A lint that prints the address has copied it into the
# gate log, which is the thing it exists to prevent.
#
# Files outside infra/ are not read: fixtures, seeds and docs carry
# made-up people on purpose, and a tenant's own people live in its
# repository, not this one.
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  no personal address under infra/
#   1  one or more — file:line and domain, the author's to fix
#   3  the tree could not be listed — a fact about the MACHINE; never
#      `clean`
#
# USAGE
#   infra/lint/no-personal-address-in-infra.sh
#   infra/lint/no-personal-address-in-infra.sh --self-test
set -uo pipefail

NAME="no-personal-address-in-infra"
LINT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh"
# shellcheck source=infra/lint/lib/git-answer.sh
. "$LINT_DIR/lib/git-answer.sh"
cd "$LINT_DIR/../.." || exit 1

COMPANY_DOMAIN="algedonic.dev"
# The shape of an address, as grep -oE prints it. The domain must end in
# a letters-only label so `1.2.3.4`-style hosts are not read as one.
ADDRESS='[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}'

# is_no_reply <local-part> — 0 iff the mailbox is one nobody reads.
is_no_reply() {
    case "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" in
        noreply|no-reply|no_reply|noreply.*|no-reply.*|no_reply.*|*.noreply|*.no-reply|*-noreply|*-no-reply) return 0 ;;
    esac
    return 1
}

# is_allowed_domain <domain> — 0 iff the address belongs to nobody in
# particular, or to the company.
is_allowed_domain() {
    local d
    d="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
    case "$d" in
        "$COMPANY_DOMAIN"|*".$COMPANY_DOMAIN") return 0 ;;
        example.com|example.net|example.org) return 0 ;;
        *.example|*.invalid|*.test|*.local|*.localhost) return 0 ;;
        users.noreply.github.com) return 0 ;;
        *.service|*.timer|*.socket|*.target|*.path|*.mount) return 0 ;;
    esac
    return 1
}

# One file's findings as lines of `<line>: <domain>`; empty = clean.
# One finding per (line, domain): two visitors at one provider on one
# line are one thing to fix.
findings_in() { # file
    local hit line addr domain local_part
    grep -noE "$ADDRESS" "$1" 2>/dev/null | while IFS= read -r hit; do
        [ -n "$hit" ] || continue
        line=${hit%%:*}
        addr=${hit#*:}
        domain=${addr##*@}
        local_part=${addr%@*}
        is_no_reply "$local_part" && continue
        is_allowed_domain "$domain" && continue
        printf '%s: %s\n' "$line" "$domain"
    done | sort -u -t: -k1,1n -k2
}

# --- self-test ---------------------------------------------------------
# Runs on every invocation: a pattern that stopped matching, or a domain
# rule that stopped sorting, passes every file, and only a fixture it
# must refuse tells that from a clean tree. Fixture addresses are made
# up and never printed by the lint itself.
self_test() {
    local t
    t="$(mktemp -d)" || { echo "$NAME: cannot make a temp dir for the self-test" >&2; return 1; }
    # shellcheck disable=SC2064
    trap "rm -rf '$t'" RETURN

    # Every fixture address is assembled here from a local part and a
    # domain, so no line of THIS file has the shape of an address: the
    # lint scans itself on every run (it lives under infra/).
    local at='@' g='gmail' h='Hotmail' sa='saouma'
    {
        printf 'a = "david%s%s"\nb = "guest%s%s"\nc = "auth%ssend.%s"\n' "$at" "$COMPANY_DOMAIN" "$at" "$COMPANY_DOMAIN" "$at" "$COMPANY_DOMAIN"
        printf 'd = alice%sexample.com\ne = lint%sexample.invalid\nf = va%sboss.local\n' "$at" "$at" "$at"
        printf 'g = x%sboss.example\nh = t%sx.test\ni = Requires=wg-quick%swg0.service\n' "$at" "$at" "$at"
        printf 'j = dauld%susers.noreply.github.com\n' "$at"
        printf 'k = Co-Authored-By: a model <noreply%santhropic.com>\nl = no-reply%svendor.io\n' "$at" "$at"
    } >"$t/good.toml"
    [ -z "$(findings_in "$t/good.toml")" ] || {
        echo "$NAME: self-test FAILED — a company, reserved, unit or no-reply address was refused:" >&2
        findings_in "$t/good.toml" >&2
        return 1
    }

    {
        printf '# ask visitor%s%s.com\n' "$at" "$g"
        printf 'emails = ["david%s%s", "one%s%s.com", "two%s%s.ch"]\n' "$at" "$COMPANY_DOMAIN" "$at" "$g" "$at" "$sa"
        printf 'x = Someone%s%s.COM\n' "$at" "$h"
    } >"$t/bad.toml"
    local got want
    got="$(findings_in "$t/bad.toml")"
    want="$(printf '1: %s.com\n2: %s.com\n2: %s.ch\n3: %s.COM' "$g" "$g" "$sa" "$h")"
    [ "$got" = "$want" ] || {
        echo "$NAME: self-test FAILED — expected three lines of personal domains, got:" >&2
        printf '%s\n' "$got" >&2
        return 1
    }
    case "$got" in
        *visitor*|*one"$at"*|*two"$at"*|*Someone*) echo "$NAME: self-test FAILED — a local part was printed" >&2; return 1 ;;
    esac
    echo "$NAME: self-test ok — company, reserved, unit, GitHub no-reply and noreply@ addresses pass; a personal address is named by line and domain only"
}

if [ "${1:-}" = "--self-test" ]; then self_test; exit $?; fi
self_test || exit 1

# --- the tree ----------------------------------------------------------
files="$(git_answer "$NAME" 0 ls-files -- 'infra/')" || exit $?

scanned=0
findings=0
while IFS= read -r file; do
    [ -n "$file" ] || continue
    [ -f "$file" ] || continue
    scanned=$((scanned + 1))
    while IFS= read -r hit; do
        [ -n "$hit" ] || continue
        findings=$((findings + 1))
        echo "$NAME: $file:$hit — a personal address; infra/ is public" >&2
    done <<EOT
$(findings_in "$file")
EOT
done <<EOT
$files
EOT

if [ "$findings" -gt 0 ]; then
    cat >&2 <<'MSG'

FAIL — the line(s) above name a person by a private address in a file
the public mirror publishes (found first by the publish-to-github
review, 2026-09-18, backlog 67e754cc). An Access policy that admits
individuals declares `include.unmanaged = true` and keeps its members
in the dashboard (infra/cluster/dns/access.toml); a company mailbox is
<name>@algedonic.dev; a fixture uses a reserved domain (example.com,
.invalid, .test). The domain is printed, the address is not.
MSG
    exit 1
fi

lint_scanned "$NAME" "$scanned" "file(s) under infra/ read"
echo "$NAME: ok — no personal address under infra/"
exit 0
