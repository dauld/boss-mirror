#!/usr/bin/env bash
# forge-backup — the forge backs itself up: a nightly `forgejo dump` of
# the repositories and Forgejo's own database, verified, and kept on
# this host in a bounded count.
#
# WHY (backlog 121831e6, the prerequisite of the DR rehearsal dd8120ba).
# The git repositories, Forgejo's database (users, PRs, issues, the
# act_runner registration), app.ini and the token signing keys existed
# in exactly one place — this host's root disk — and nothing copied
# them. The cluster's postgres has had a nightly verified dump since
# 2026-08-12 (infra/cluster/manifests/boss-backup.yaml); the forge,
# which holds the only copy of the repository that dump's code came
# from, had none.
#
# WHAT IS DUMPED, measured rather than assumed. On 2026-09-21
# /opt/forgejo/data split git 110M (the repositories) and gitea 35G,
# and the 35G was ENTIRELY packages — the OCI registry, per-train
# images CI regenerates and the disk-floor sweep already treats as
# garbage. The irreplaceable part is ~320M (~480M with Actions logs).
# The flags below are read off Forgejo's own `dump` documentation and
# cmd/dump.go (2026-09-23), not from memory:
#   --file -            the archive on stdout, logs on stderr — so the
#                       archive lands on the HOST, never inside the
#                       container's writable layer.
#   --type tar.gz       so `gzip -t` and `tar -tzv` can prove it whole.
#   --skip-package-data the 35G registry.
#   --skip-custom-dir   THE TRAP. Forgejo's image sets
#                       GITEA_CUSTOM=/data/gitea (its Dockerfile), which
#                       is the SAME directory as APP_DATA_PATH, and the
#                       custom-dir leg excludes nothing but the output
#                       file — so without this flag the registry rides
#                       in again under `custom/`. Nothing is lost by
#                       skipping it: the data leg packs /data/gitea
#                       (conf/, jwt/, ssh/, avatars, actions_log) with
#                       the registry excluded, and app.ini is added on
#                       its own.
#   --skip-log / --skip-index / --skip-repo-archives
#                       server logs, the bleve index and generated
#                       archives: regenerable, not history anyone reads.
# The database leaves as `forgejo-db.sql`, written through Forgejo's own
# connection — the consistent copy. A plain cp of the live gitea.db with
# its 11M write-ahead log can be torn, which is the backup that restores
# into a corrupt Forgejo and looks like success (the packet's
# sqlite_caveat). The data leg also packs the raw gitea.db; restore from
# the SQL.
#
# WHAT IS PROVEN BEFORE A FILE IS CALLED A BACKUP — the artefact, not an
# exit code (the pg dump's own lesson: a pipe's left side dying leaves a
# truncated archive that looks present). The stream is whole (gzip -t);
# it lists forgejo-db.sql with a non-zero size, app.ini, and at least
# one repository's HEAD; it carries NO registry blob (data/packages/ or
# custom/); and it is at least BOSS_FORGE_BACKUP_MIN_MB. Until all of
# that holds it is `.partial-<stamp>.tar.gz`, and a run that cannot
# prove it deletes it and exits non-zero: a failed unit, and a packet on
# the `failed` terminal with `refused` naming why.
#
# THE DISK. This host has one filesystem, and CI, the registry and
# Forgejo share it; a full disk wedged Forgejo's Actions dispatcher for
# eight hours on 2026-09-03. So free space is read the way disk-report.sh
# reads it (`df -k`, available KB, nearest GiB) and the dump does not
# start unless it leaves BOSS_FORGE_BACKUP_FLOOR_GB free even if it
# grows to its ceiling. The ceiling is enforced, not hoped for: the
# stream passes through `head -c`, so one run can never write more than
# BOSS_FORGE_BACKUP_MAX_MB, and a dump that reaches it is refused as the
# wrong thing (the registry slipping back in is the likely cause). The
# retained set is bounded by COUNT, pruned oldest-first by the stamp in
# the name, every deletion printed.
#
# WHAT THIS DOES NOT DO: take the copy off this host. The boss-gcp leg
# (a deposit-only forced-command key and a receiver) and the GCS leg (a
# service-account key) each need a credential on the forge, and there is
# none. So every run says so — `offsite: NONE` in the journal and
# `offsite=none …` on the packet — rather than let a local copy be read
# as the disaster-recovery copy it is not: it shares this disk's fire,
# theft and failure.
#
# THE ARCHIVE IS SECRET. app.ini carries SECRET_KEY and INTERNAL_TOKEN,
# data/jwt the token signing key, data/ssh the host keys: the directory
# is 0700 and every file 0600, root's. Nothing here prints its contents.
#
# Runs as root (the system docker daemon Forgejo lives on; OPERATIONS.md)
# from forge-backup.service. Tested against stubs in
# crates/core/boss-testing/tests/forge_backup_sh.rs.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=infra/run-summary.sh
. "${HERE}/../run-summary.sh"

me="forge-backup"
say() { echo "$me: $*"; }

DEST="${BOSS_FORGE_BACKUP_DIR:-/var/backups/boss/forge}"
KEEP="${BOSS_FORGE_BACKUP_KEEP:-14}"
FLOOR_GB="${BOSS_FORGE_BACKUP_FLOOR_GB:-20}"
MAX_MB="${BOSS_FORGE_BACKUP_MAX_MB:-5120}"
MIN_MB="${BOSS_FORGE_BACKUP_MIN_MB:-50}"
CONTAINER="${BOSS_FORGE_CONTAINER:-forgejo}"
APP_INI="${BOSS_FORGE_APP_INI:-/data/gitea/conf/app.ini}"
DOCKER="${BOSS_FORGE_BACKUP_DOCKER:-docker}"
# WHICH DAEMON, explicitly: Forgejo runs on the SYSTEM daemon, and a
# DOCKER_HOST inherited from anywhere else (the converge builds on
# david's rootless one) would answer "no such container" about a
# container that is running.
export DOCKER_HOST="${BOSS_FORGE_BACKUP_DOCKER_HOST:-unix:///var/run/docker.sock}"

# The record starts clean, before anything can refuse (run-summary.sh
# (3)): a summary left by an earlier run must never be read as this one's.
run_summary_reset
stamp="$(date -u +%Y%m%d-%H%M%S)"
run_summary_field stamp_epoch "$(date -u +%s)"
OFFSITE_NOTE="none — this copy shares the forge's disk; the boss-gcp and GCS legs need a credential on this host that has not been placed (backlog 121831e6)"
run_summary_field offsite "$OFFSITE_NOTE"

partial="$DEST/.partial-${stamp}.tar.gz"
errlog="$DEST/.partial-${stamp}.stderr"
listing="$DEST/.partial-${stamp}.list"

refuse() {
    echo "$me: REFUSED — $*" >&2
    echo "$me: offsite: NONE (no off-host leg exists yet), and no new local copy either" >&2
    run_summary_field refused "$*"
    rm -f "$partial" "$errlog" "$listing"
    exit 1
}

for v in KEEP FLOOR_GB MAX_MB MIN_MB; do
    case "${!v}" in
        ''|*[!0-9]*) refuse "$v must be a whole number, got '${!v}'" ;;
    esac
done
[ "$KEEP" -ge 1 ] || refuse "KEEP must be at least 1 — a retention of zero deletes the backup it just made"

umask 077
mkdir -p "$DEST" || refuse "cannot create $DEST"
chmod 0700 "$DEST" || refuse "cannot make $DEST owner-only"

# A partial is never a backup. One left here was a run killed mid-dump
# (a reboot, a timeout); it is removed, and said so.
for f in "$DEST"/.partial-*; do
    [ -e "$f" ] || continue
    say "removing $(basename "$f"), left by a run that never finished"
    rm -f "$f"
done

# ---------------------------------------------------------------------
# 1. The disk, read the way disk-report.sh reads it.
# ---------------------------------------------------------------------
free_gb_now() {
    local kb
    kb=$(df -k "$DEST" 2>/dev/null | awk 'NR==2 {print $4}')
    case "${kb:-empty}" in
        empty|*[!0-9]*) return 1 ;;
    esac
    echo $(( (kb + 524288) / 1048576 ))
}
free_before=$(free_gb_now) || refuse "df -k $DEST did not answer with a number — an unmeasured disk is not a roomy one"
run_summary_field free_gb_before "$free_before"
max_gb=$(( (MAX_MB + 1023) / 1024 ))
need=$(( FLOOR_GB + max_gb ))
if [ "$free_before" -lt "$need" ]; then
    refuse "${free_before}G free on $DEST's filesystem; a dump needs the floor (${FLOOR_GB}G) plus its ceiling (${max_gb}G) = ${need}G, so it did not start. The disk-floor sweep reclaims regenerable caches hourly; read disk-report for what holds the rest"
fi

# ---------------------------------------------------------------------
# 2. Forgejo, running.
# ---------------------------------------------------------------------
running=$("$DOCKER" inspect -f '{{.State.Running}}' "$CONTAINER" 2>&1)
[ "$running" = "true" ] || refuse "container '$CONTAINER' is not running on the system daemon ($DOCKER_HOST): $running"

# ---------------------------------------------------------------------
# 3. The dump, bounded by the ceiling.
# ---------------------------------------------------------------------
max_bytes=$(( MAX_MB * 1024 * 1024 ))
say "dumping '$CONTAINER' (repositories + database; registry, custom dir, logs, index and repo archives skipped) to $(basename "$partial"), ceiling ${MAX_MB}M"
# pipefail OFF for this one pipeline, deliberately: `head -c` stopping
# early IS the ceiling, so the dump's SIGPIPE at the ceiling is the
# expected shape, not a coin. Each side's status is read from PIPESTATUS
# below and judged on its own — the ceiling by the byte count, the dump
# by its exit, the write by head's — so nothing is inferred from the
# pipeline's combined status.
set +o pipefail
"$DOCKER" exec -u git -w /tmp "$CONTAINER" \
    forgejo dump --config "$APP_INI" --file - --type tar.gz \
    --skip-package-data --skip-custom-dir --skip-log --skip-index --skip-repo-archives \
    2>"$errlog" | head -c "$max_bytes" >"$partial"
st=("${PIPESTATUS[@]}")
set -o pipefail
bytes=$(wc -c <"$partial" 2>/dev/null | tr -d ' ')
case "${bytes:-empty}" in empty|*[!0-9]*) bytes=0 ;; esac

print_dump_stderr() {
    if [ -s "$errlog" ]; then
        echo "$me: forgejo dump's stderr, whole:" >&2
        sed 's/^/    /' "$errlog" >&2
    fi
}
if [ "$bytes" -ge "$max_bytes" ]; then
    print_dump_stderr
    refuse "the dump reached its ${MAX_MB}M ceiling and was cut off there. The irreplaceable part measured ~480M on 2026-09-21, so something the skip flags should keep out is in it — the registry, most likely"
fi
if [ "${st[0]}" -ne 0 ]; then
    print_dump_stderr
    refuse "forgejo dump exited ${st[0]} after ${bytes} bytes — a truncated archive is not a backup"
fi
if [ "${st[1]}" -ne 0 ]; then
    refuse "writing the archive failed (head exited ${st[1]}) after ${bytes} bytes — is $DEST full?"
fi

# ---------------------------------------------------------------------
# 4. Verify the artefact.
# ---------------------------------------------------------------------
gzip -t "$partial" 2>"$listing" || { cat "$listing" >&2; refuse "gzip -t: the archive stream is not whole"; }
tar -tzvf "$partial" >"$listing" 2>&1 || { sed -n '1,20p' "$listing" >&2; refuse "tar cannot list the archive"; }

db_bytes=$(awk '$NF == "forgejo-db.sql" || $NF == "./forgejo-db.sql" {print $3; exit}' "$listing")
case "${db_bytes:-empty}" in
    empty|*[!0-9]*) refuse "the archive holds no forgejo-db.sql — without the database it is not a backup of the forge" ;;
esac
[ "$db_bytes" -gt 0 ] || refuse "forgejo-db.sql in the archive is empty"
grep -qE '[[:space:]](\./)?app\.ini$' "$listing" || refuse "the archive holds no app.ini — the restore would have no configuration or secrets"
repos=$(grep -cE '[[:space:]](\./)?repos/[^/]+/[^/]+\.git/HEAD$' "$listing" || true)
case "${repos:-0}" in ''|*[!0-9]*) repos=0 ;; esac
[ "$repos" -ge 1 ] || refuse "the archive holds no repository (no repos/<owner>/<name>.git/HEAD)"
leak=$(grep -m1 -E '[[:space:]](\./)?(custom|data/packages)/' "$listing" || true)
[ -z "$leak" ] || refuse "the archive carries registry or custom-dir data it was told to skip, first entry: ${leak##* }"
min_bytes=$(( MIN_MB * 1024 * 1024 ))
[ "$bytes" -ge "$min_bytes" ] || refuse "the archive is ${bytes} bytes, smaller than the ${MIN_MB}M the repositories alone measured (110M, 2026-09-21) — a dump of the wrong thing"

# ---------------------------------------------------------------------
# 5. Promote, record, prune.
# ---------------------------------------------------------------------
final="$DEST/forge-${stamp}.tar.gz"
mv -f "$partial" "$final" || refuse "could not name the verified archive $final"
chmod 0600 "$final"
rm -f "$errlog" "$listing"
echo "$final" >"$DEST/.latest"
sha=$(sha256sum "$final" | awk '{print $1}')
size=$(du -h "$final" | cut -f1)

mapfile -t all < <(printf '%s\n' "$DEST"/forge-*.tar.gz | sort)
pruned=0
if [ "${#all[@]}" -gt "$KEEP" ]; then
    for old in "${all[@]:0:$(( ${#all[@]} - KEEP ))}"; do
        say "pruned $(basename "$old") (keeping the newest $KEEP)"
        rm -f "$old"
        pruned=$((pruned + 1))
    done
fi
kept=$(( ${#all[@]} - pruned ))
free_after=$(free_gb_now) || free_after="unmeasured"

run_summary_field archive "$(basename "$final")"
run_summary_field bytes "$bytes"
run_summary_field size "$size"
run_summary_field sha256 "$sha"
run_summary_field repositories "$repos"
run_summary_field db_sql_bytes "$db_bytes"
run_summary_field kept "$kept"
run_summary_field pruned "$pruned"
run_summary_field free_gb_after "$free_after"
run_summary_field summary "dumped and verified $(basename "$final") ($size, $repos repositories); $kept kept on the forge; no offsite copy"

say "OK $(basename "$final") ($size, sha256 ${sha:0:12}) verified: forgejo-db.sql ${db_bytes} bytes, app.ini, ${repos} repositories, no registry data"
say "$kept kept in $DEST, $pruned pruned; ${free_after}G free after"
say "offsite: NONE — this copy shares the forge's disk (fire, theft, the box dying); the boss-gcp and GCS legs need a credential on this host"
