#!/usr/bin/env bash
#
# invariant-register — the shape check on docs/invariants/.
#
# THE PRINCIPLE
# -------------
# Every load-bearing invariant declares how it is held
# (docs/design/design-conformance.md). The failure this guards against
# is not a wrong claim — it is an UNDECLARED one: "a claim with no
# enforcement is indistinguishable, at a glance, from one with
# enforcement." A register that lets an entry omit its enforcement
# class, or name a lint that was deleted two cars ago, reintroduces
# exactly the ambiguity it was written to remove.
#
# THE CHECKED PROPERTY
# --------------------
# The SHAPE of the register, never the truth of a claim. Specifically:
#
#   * every entry carries all seven keys, and no others;
#   * `id` is a unique, citable slug (a conformance finding cites an
#     id, so a duplicate silently merges two histories);
#   * `claim`, `source` and `enforcement` are non-empty, and
#     `enforcement` is one of enforced / checked / unenforced;
#   * enforced   → `mechanism` names a path that EXISTS ON DISK. This
#                  is the one check with teeth against rot: a lint
#                  deleted or renamed without touching the register
#                  fails here, by id.
#   * checked    → `mechanism` says how it is verified, and
#                  `last_verified` is a real YYYY-MM-DD date.
#   * unenforced → `note` says why, and `mechanism` is EMPTY. An
#                  unenforced invariant that names a mechanism is
#                  claiming enforcement it does not have.
#
#   * exactly ONE entry per file, and the file is named for its id.
#
# That last rule is why the register is a directory. As one file its
# entries appended at the tail, so two cars registering a learning on
# the same day conflicted on the same line; on 2026-08-15 resolving
# one such conflict dropped an entry's `[[invariant]]` header and
# folded its keys into the entry above, and THIS LINT SAID `ok`
# (b071994b). Two holes let that through and both are now shut: a
# duplicate key inside an entry is a finding, and the census asserts
# its own arithmetic — it used to print "43 declared (19 enforced, 4
# checked, 21 unenforced)", which does not add up, and exit 0.
#
# What this deliberately does NOT do is grade the strength of the
# declaration. An author may write `unenforced`, and that honesty is
# the point — anything else turns the rule into pressure to fake
# enforcement (design-conformance Q2).
#
# Following the no-secrets.sh precedent of checks that prove
# themselves, every run starts with a self-test: one planted fixture
# per malformed shape, each asserted caught by id AND by field, plus a
# well-formed fixture asserted clean. `--self-test` runs just that and
# stops.
#
# bash 3.2 safe: no associative arrays, no mapfile, no ${x,,}. The
# TOML subset parsed here is the one the register is written in —
# `[[invariant]]` headers and single-line `key = "value"` pairs.
#
# Usage: infra/lint/invariant-register.sh [--self-test]
# Exit:  0 clean / 1 findings or self-test failure

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh

REGISTER_DIR="docs/invariants"
REQUIRED_KEYS="id claim source enforcement mechanism last_verified note"

ABSENT="<absent>"

# ---------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------
# Findings name the file, the line the entry starts on, the id, and the
# field — "fail with the offending id and field" is the whole contract
# of an error message here. An entry whose id is itself missing reports
# as <no id> so the line number is still actionable.
FINDINGS=0

finding() {
    # finding <file> <line> <id> <field> <message>
    local file="$1" line="$2" id="$3" field="$4" msg="$5"
    # An entry whose id is missing or empty still has to report
    # something citable, so the line number carries it.
    if [ -z "$id" ] || [ "$id" = "$ABSENT" ]; then
        id="<no id>"
    fi
    echo "  ${file}:${line}: ${id}: ${field}: ${msg}"
    FINDINGS=$((FINDINGS + 1))
}

# ---------------------------------------------------------------------
# Trim helpers (portable to bash 3.2)
# ---------------------------------------------------------------------
ltrim() { printf '%s' "${1#"${1%%[![:space:]]*}"}"; }
rtrim() { printf '%s' "${1%"${1##*[![:space:]]}"}"; }

# ---------------------------------------------------------------------
# Per-entry validation
# ---------------------------------------------------------------------
# Reads the seven field variables set by the parser. Kept separate from
# the parse loop so the rules read as a list rather than as control
# flow.
validate_entry() {
    local file="$1" line="$2"

    # --- presence + non-emptiness of the always-required three -------
    local key val
    for key in $REQUIRED_KEYS; do
        eval "val=\${f_$key}"
        if [ "$val" = "$ABSENT" ]; then
            finding "$file" "$line" "$f_id" "$key" "required key is missing"
        fi
    done

    # An entry with no id cannot be cited; everything below still runs
    # so one malformed entry reports all its problems at once.
    if [ "$f_id" != "$ABSENT" ]; then
        if [ -z "$f_id" ]; then
            finding "$file" "$line" "" "id" "must not be empty"
        elif ! grep -qE '^[a-z0-9][a-z0-9-]*$' <<<"$f_id"; then
            finding "$file" "$line" "$f_id" "id" \
                "must be a lowercase slug (a-z, 0-9, dashes) so findings can cite it"
        elif grep -qx -- "$f_id" <<< "$SEEN_IDS"; then
            finding "$file" "$line" "$f_id" "id" \
                "duplicate id — an id is cited by conformance findings and must be unique"
        fi
        SEEN_IDS="${SEEN_IDS}
${f_id}"
    fi

    if [ "$f_claim" != "$ABSENT" ] && [ -z "$f_claim" ]; then
        finding "$file" "$line" "$f_id" "claim" "must not be empty"
    fi
    if [ "$f_source" != "$ABSENT" ] && [ -z "$f_source" ]; then
        finding "$file" "$line" "$f_id" "source" \
            "must name the doc or file that states the claim"
    fi

    # --- the enforcement class dictates the rest ---------------------
    case "$f_enforcement" in
        "$ABSENT")
            : ;;  # already reported as a missing key
        enforced)
            if [ -z "$f_mechanism" ]; then
                finding "$file" "$line" "$f_id" "mechanism" \
                    "enforcement = enforced requires a lint/test path"
            elif [ ! -e "$f_mechanism" ]; then
                finding "$file" "$line" "$f_id" "mechanism" \
                    "enforcement = enforced names '${f_mechanism}', which does not exist on disk"
            fi
            ;;
        checked)
            if [ -z "$f_mechanism" ]; then
                finding "$file" "$line" "$f_id" "mechanism" \
                    "enforcement = checked requires the verification method"
            fi
            if [ -z "$f_last_verified" ]; then
                finding "$file" "$line" "$f_id" "last_verified" \
                    "enforcement = checked requires a date — an unverified 'checked' is an 'unenforced'"
            elif ! grep -qE '^[0-9]{4}-[0-9]{2}-[0-9]{2}$' <<<"$f_last_verified"; then
                finding "$file" "$line" "$f_id" "last_verified" \
                    "must be YYYY-MM-DD, got '${f_last_verified}'"
            fi
            ;;
        unenforced)
            if [ -z "$f_note" ]; then
                finding "$file" "$line" "$f_id" "note" \
                    "enforcement = unenforced requires a note saying why, or what would enforce it"
            fi
            if [ -n "$f_mechanism" ]; then
                finding "$file" "$line" "$f_id" "mechanism" \
                    "enforcement = unenforced must leave mechanism empty — naming one claims enforcement it does not have"
            fi
            ;;
        *)
            finding "$file" "$line" "$f_id" "enforcement" \
                "must be enforced, checked or unenforced, got '${f_enforcement}'"
            ;;
    esac
}

reset_entry() {
    f_id="$ABSENT";        f_claim="$ABSENT"
    f_source="$ABSENT";    f_enforcement="$ABSENT"
    f_mechanism="$ABSENT"; f_last_verified="$ABSENT"
    f_note="$ABSENT"
}

# ---------------------------------------------------------------------
# Parse + check one register file
# ---------------------------------------------------------------------
# Returns 0 clean, 1 if anything was reported. Findings land on stdout
# so both the gate and the self-test read them the same way.
check_file() {
    local file="$1"
    local lineno=0 entry_line=0 in_entry=0 entries=0
    local line key val

    # FINDINGS is per-file; SEEN_IDS deliberately is NOT reset here.
    # An id is unique across the register, and now that every entry has
    # its own file, that is the only place the check can live.
    FINDINGS=0
    reset_entry

    if [ ! -f "$file" ]; then
        echo "  ${file}: register file not found"
        return 1
    fi

    while IFS= read -r line || [ -n "$line" ]; do
        lineno=$((lineno + 1))
        line=$(ltrim "$line")

        case "$line" in
            '')   continue ;;
            '#'*) continue ;;
            '[[invariant]]')
                if [ "$in_entry" -eq 1 ]; then
                    validate_entry "$file" "$entry_line"
                fi
                reset_entry
                in_entry=1
                entry_line="$lineno"
                entries=$((entries + 1))
                continue
                ;;
        esac

        case "$line" in
            *=*)
                key=$(rtrim "${line%%=*}")
                val=$(ltrim "${line#*=}")
                # Strip the surrounding double quotes of a TOML basic
                # string. Inner escaped quotes are irrelevant here —
                # this lint only ever asks whether a value is empty.
                case "$val" in
                    '"'*'"') val="${val#\"}"; val="${val%\"}" ;;
                esac

                if [ "$in_entry" -eq 0 ]; then
                    finding "$file" "$lineno" "" "$key" \
                        "key sits outside any [[invariant]] entry"
                    continue
                fi

                case "$key" in
                    id|claim|source|enforcement|mechanism|last_verified|note)
                        # A key seen twice in one entry is invalid TOML,
                        # and it is the exact signature of an entry whose
                        # [[invariant]] header was lost in a merge: its
                        # keys fold into the entry above. Overwriting
                        # silently is how that shipped once already.
                        eval "prev=\${f_$key}"
                        if [ "$prev" != "$ABSENT" ]; then
                            finding "$file" "$lineno" "$f_id" "$key" \
                                "duplicate key in one entry — invalid TOML, and the signature of a lost [[invariant]] header"
                        fi
                        eval "f_$key=\$val"
                        ;;
                    *)
                        finding "$file" "$lineno" "$f_id" "$key" \
                            "unknown key — the register's fields are: ${REQUIRED_KEYS}"
                        ;;
                esac
                ;;
            *)
                finding "$file" "$lineno" "$f_id" "<line>" \
                    "not a comment, an [[invariant]] header, or a key = \"value\" pair"
                ;;
        esac
    done < "$file"

    if [ "$in_entry" -eq 1 ]; then
        validate_entry "$file" "$entry_line"
    fi

    if [ "$entries" -eq 0 ]; then
        echo "  ${file}: holds no [[invariant]] entry"
        return 1
    fi
    # One entry per file is what makes the register uncontended: a car
    # adding an invariant creates a file and edits no shared line. A
    # file holding two is a tail growing back.
    if [ "$entries" -gt 1 ]; then
        finding "$file" "$entry_line" "$f_id" "<file>" \
            "holds ${entries} entries — one invariant per file, named for its id"
    fi

    [ "$FINDINGS" -eq 0 ]
}

# The filename IS the id. Checked separately from check_file so the
# self-test can keep planting fixtures in mktemp files.
check_filename() {
    local file="$1"
    local want stem
    stem=$(basename "$file" .toml)
    want=$(grep -m1 '^id = ' "$file" | sed -e 's/^id = "//' -e 's/"$//')
    if [ -n "$want" ] && [ "$want" != "$stem" ]; then
        echo "  ${file}: ${want}: <filename>: file is named '${stem}.toml' but declares id '${want}' — an id nobody can find by name is an id nobody cites"
        return 1
    fi
    return 0
}

# ---------------------------------------------------------------------
# Self-test — plant each malformed shape, assert it is caught
# ---------------------------------------------------------------------
# Every rule above gets a fixture that violates it and nothing else, so
# a fixture that stops failing means that rule stopped working. The
# assertion is on the ID AND THE FIELD, not merely on the exit code: a
# check that fails without naming what to fix is a check nobody can
# act on.
ST_FAILS=0
ST_RUN=0

# A well-formed entry, used as the body every fixture mutates. The
# mechanism path is this script itself, so the enforced-path rule has
# something real to find.
valid_entry() {
    cat <<'ENTRY'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture, not a real invariant."
source = "infra/lint/invariant-register.sh"
enforcement = "enforced"
mechanism = "infra/lint/invariant-register.sh"
last_verified = ""
note = ""
ENTRY
}

# assert_caught <label> <expect-id> <expect-field> <<fixture
assert_caught() {
    local label="$1" want_id="$2" want_field="$3"
    local tmp out rc
    ST_RUN=$((ST_RUN + 1))
    tmp=$(mktemp)
    cat > "$tmp"
    SEEN_IDS=""
    out=$(check_file "$tmp"); rc=$?
    rm -f "$tmp"

    if [ "$rc" -eq 0 ]; then
        echo "invariant-register self-test FAIL: ${label} — malformed shape was accepted" >&2
        ST_FAILS=1
        return
    fi
    if ! grep -q "${want_id}: ${want_field}:" <<<"$out"; then
        echo "invariant-register self-test FAIL: ${label} — caught, but did not name '${want_id}: ${want_field}'; said:" >&2
        printf '%s\n' "$out" >&2
        ST_FAILS=1
    fi
}

# assert_caught_dir <label> <expect-id> <expect-field> <file1>=<<body ...
# Planted as a whole DIRECTORY, because two of the rules — id
# uniqueness and filename-matches-id — are properties of the register
# rather than of any one file, and a fixture that cannot express that
# cannot test it.
assert_caught_dir() {
    local label="$1" want_id="$2" want_field="$3"; shift 3
    local dir out rc name
    ST_RUN=$((ST_RUN + 1))
    dir=$(mktemp -d)
    for name in "$@"; do
        valid_entry > "${dir}/${name}.toml"
    done
    out=$(check_register "$dir"); rc=$?
    rm -rf "$dir"

    if [ "$rc" -eq 0 ]; then
        echo "invariant-register self-test FAIL: ${label} — malformed register was accepted" >&2
        ST_FAILS=1
        return
    fi
    if ! grep -q "${want_id}: ${want_field}:" <<<"$out"; then
        echo "invariant-register self-test FAIL: ${label} — caught, but did not name '${want_id}: ${want_field}'; said:" >&2
        printf '%s\n' "$out" >&2
        ST_FAILS=1
    fi
}

assert_clean() {
    local label="$1"
    local tmp out rc
    ST_RUN=$((ST_RUN + 1))
    tmp=$(mktemp)
    cat > "$tmp"
    SEEN_IDS=""
    out=$(check_file "$tmp"); rc=$?
    rm -f "$tmp"
    if [ "$rc" -ne 0 ]; then
        echo "invariant-register self-test FAIL: ${label} — well-formed fixture was rejected:" >&2
        printf '%s\n' "$out" >&2
        ST_FAILS=1
    fi
}

self_test() {
    # Positive control first: if this fails, every negative below is
    # meaningless.
    assert_clean "a well-formed entry passes" < <(valid_entry)

    assert_caught "missing required key" fixture-one source <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = "why not"
EOF

    assert_caught "missing id" '<no id>' id <<'EOF'
[[invariant]]
claim = "A planted fixture."
source = "nowhere"
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = "why not"
EOF

    assert_caught "empty claim" fixture-one claim <<'EOF'
[[invariant]]
id = "fixture-one"
claim = ""
source = "nowhere"
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = "why not"
EOF

    # Two files, same id. This is what "unique across the register"
    # means now that each entry lives in its own file, and it only
    # holds because SEEN_IDS survives the loop over them.
    ST_RUN=$((ST_RUN + 1))
    st_dupe_dir=$(mktemp -d)
    valid_entry > "${st_dupe_dir}/fixture-one.toml"
    valid_entry > "${st_dupe_dir}/fixture-two.toml"
    st_out=$(check_register "$st_dupe_dir")
    rm -rf "$st_dupe_dir"
    if ! grep -q 'fixture-one: id:' <<<"$st_out"; then
        echo "invariant-register self-test FAIL: duplicate id across two files was not caught by id; said:" >&2
        printf '%s\n' "$st_out" >&2
        ST_FAILS=1
    fi

    # The lost-header signature: an entry's keys folded into the one
    # above it, so every key appears twice. The register shipped in
    # this state on 2026-08-15 and the lint said ok.
    assert_caught "a duplicate key inside one entry" fixture-one claim <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = "why not"
claim = "A second claim, folded in from an entry that lost its header."
EOF

    # A file holding two entries is a contended tail growing back.
    assert_caught "two entries in one file" fixture-one '<file>' < <(valid_entry; valid_entry)

    # A file whose name does not match its id makes the id unfindable.
    assert_caught_dir "a filename that does not match its id" fixture-one '<filename>' misnamed

    assert_caught "unknown enforcement class" fixture-one enforcement <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "mostly"
mechanism = ""
last_verified = ""
note = "why not"
EOF

    assert_caught "enforced with no mechanism" fixture-one mechanism <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "enforced"
mechanism = ""
last_verified = ""
note = ""
EOF

    assert_caught "enforced naming a path that does not exist" fixture-one mechanism <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "enforced"
mechanism = "infra/lint/deleted-two-cars-ago.sh"
last_verified = ""
note = ""
EOF

    assert_caught "checked with no last_verified" fixture-one last_verified <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "checked"
mechanism = "read it by hand"
last_verified = ""
note = ""
EOF

    assert_caught "checked with a non-date last_verified" fixture-one last_verified <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "checked"
mechanism = "read it by hand"
last_verified = "last August"
note = ""
EOF

    assert_caught "checked with no mechanism" fixture-one mechanism <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "checked"
mechanism = ""
last_verified = "2026-08-13"
note = ""
EOF

    assert_caught "unenforced with no note" fixture-one note <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = ""
EOF

    assert_caught "unenforced still naming a mechanism" fixture-one mechanism <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "unenforced"
mechanism = "infra/lint/invariant-register.sh"
last_verified = ""
note = "why not"
EOF

    assert_caught "an id that cannot be cited" 'Fixture One' id <<'EOF'
[[invariant]]
id = "Fixture One"
claim = "A planted fixture."
source = "nowhere"
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = "why not"
EOF

    assert_caught "a stray key outside any entry" '<no id>' orphan <<'EOF'
orphan = "adrift"
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = "why not"
EOF

    assert_caught "an unknown key inside an entry" fixture-one owner <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = "why not"
owner = "nobody"
EOF

    # An empty register is a register that checks nothing — the failure
    # mode where the file survives but its content is gone.
    ST_RUN=$((ST_RUN + 1))
    local tmp rc
    tmp=$(mktemp)
    echo '# no entries here' > "$tmp"
    check_file "$tmp" >/dev/null; rc=$?
    rm -f "$tmp"
    if [ "$rc" -eq 0 ]; then
        echo "invariant-register self-test FAIL: an empty register was accepted" >&2
        ST_FAILS=1
    fi

    if [ "$ST_FAILS" -ne 0 ]; then
        echo "invariant-register: self-test FAILED — the shape checks cannot be trusted, fix them first" >&2
        exit 1
    fi
    echo "invariant-register: self-test ok — ${ST_RUN}/${ST_RUN} planted shapes caught by id and field, well-formed fixture clean"
}

# ---------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------
# Walks every file in the register directory in one shell so SEEN_IDS
# accumulates ACROSS files — an id is unique in the register, not
# merely inside its own file, and now that each entry has a file of
# its own that distinction is the whole of the uniqueness check.
check_register() {
    local dir="${1:-$REGISTER_DIR}"
    local file rc=0
    SEEN_IDS=""
    for file in "$dir"/*.toml; do
        check_file "$file" || rc=1
        check_filename "$file" || rc=1
    done
    return "$rc"
}

main_scan() {
    local out rc
    if [ ! -d "$REGISTER_DIR" ]; then
        echo "FAIL — ${REGISTER_DIR}/ does not exist"
        exit 1
    fi
    if [ -z "$(ls "$REGISTER_DIR"/*.toml 2>/dev/null)" ]; then
        echo "FAIL — ${REGISTER_DIR}/ holds no invariants"
        exit 1
    fi

    out=$(check_register "$REGISTER_DIR"); rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "FAIL — ${REGISTER_DIR}/ is malformed:"
        printf '%s\n' "$out"
        echo ""
        echo "Every invariant declares how it is held: enforced (name the lint/test"
        echo "path — it must exist), checked (name the method + the date it was last"
        echo "verified), or unenforced (say why, and leave mechanism empty). Writing"
        echo "'unenforced' is always allowed; leaving the question open is not."
        echo "One invariant per file, named for its id — see ${REGISTER_DIR}/README.md."
        exit 1
    fi

    # check_register runs in a subshell, so the census is recomputed
    # here rather than carried out of it.
    local files enforced checked unenforced classified
    files=$(ls "$REGISTER_DIR"/*.toml | wc -l | tr -d ' ')
    enforced=$(cat "$REGISTER_DIR"/*.toml | grep -c '^enforcement = "enforced"')
    checked=$(cat "$REGISTER_DIR"/*.toml | grep -c '^enforcement = "checked"')
    unenforced=$(cat "$REGISTER_DIR"/*.toml | grep -c '^enforcement = "unenforced"')

    # The census asserts its own arithmetic. It counts files one way
    # and enforcement lines another, and until this comparison existed
    # it happily printed "43 invariants declared (19 enforced, 4
    # checked, 21 unenforced)" — 44 by the second count — and exited 0.
    # A summary that cannot add up is a summary nobody can read for
    # meaning.
    classified=$((enforced + checked + unenforced))
    if [ "$files" -ne "$classified" ]; then
        echo "FAIL — the census does not add up: ${files} file(s) in ${REGISTER_DIR}/ but"
        echo "${classified} enforcement line(s) (${enforced} enforced + ${checked} checked + ${unenforced} unenforced)."
        echo "One of them is miscounting, which means neither number can be trusted."
        exit 1
    fi

    lint_scanned invariant-register "$files" "invariant file(s) under $REGISTER_DIR"
    echo "invariant-register: ok — ${files} invariants declared (${enforced} enforced, ${checked} checked, ${unenforced} unenforced)"
}

case "${1:-}" in
    --self-test)
        self_test ;;
    '')
        self_test
        main_scan ;;
    *)
        echo "usage: infra/lint/invariant-register.sh [--self-test]" >&2
        exit 2 ;;
esac
