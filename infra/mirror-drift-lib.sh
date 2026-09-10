#!/usr/bin/env bash
# mirror-drift-lib.sh — decide which commits a public mirror carries that
# the source ref does not are FOREIGN WORK, and which are the publish
# protocol's own bookkeeping.
#
# WHY COMMIT IDENTITY IS THE WRONG COMPARISON. The question worth
# blocking on is "would publishing a snapshot over the mirror lose file
# content that exists nowhere else?" Commit identity cannot answer it,
# because the publish protocol manufactures commits the source ref can
# never contain: the snapshot is `git commit-tree <source tree>` on top
# of the mirror's main, and merging the PR adds GitHub's merge commit
# above that. So `rev-list --count SOURCE..TARGET` is ≥ 1 from the first
# successful publish onward, forever — on 2026-09-10 it stood at 1 and
# refused a 238-commit / 763-file publish, and the one "absent" commit
# was b178a534 "Publish: forge main through 2026-08-27 (#129) (#236)"
# whose tree, 02a76d57, IS the tree of forge main's b78ae6d2. A check
# whose condition its own success makes permanently true is not a check.
#
# WHAT IS COMPARED INSTEAD. Trees. A mirror-only commit is bookkeeping
# when it introduced no content the source history does not already
# hold, which is true in exactly two shapes:
#
#   1. its tree is one the source history holds — the snapshot commit of
#      any past publish, and any commit that merely re-points to such a
#      tree; or
#   2. its tree equals a PARENT's tree — it changed no file at all: a
#      merge taking one side wholesale, or a re-commit of what we
#      published. (If that parent is itself foreign, the parent is in
#      this same set and gets named on its own line, so nothing hides.)
#
# Anything else changed a file relative to what was published, which is
# the work the original check was written to protect, and it blocks.
#
# THE BOUNDARY, NAMED. Shape 1 accepts a mirror commit that rolls the
# mirror BACK to an older source tree. That is deliberate: an old source
# tree holds no content the source history lacks, so a publish over it
# loses nothing unique — which is the question being asked. It is not a
# claim that the mirror is in a state anyone intended.
#
# A QUESTION THAT CANNOT BE ANSWERED IS A REFUSAL, NOT AN ANSWER. Every
# git invocation here is checked and a failure returns 2 with nothing on
# stdout. The script sourcing this file once reported `has_drift:false`
# — "the mirror is current" — from a `|| echo 0` while the mirror was
# 211 commits behind; a wrong target must error, not answer (CLAUDE.md
# §Doors).
#
# Sourced by infra/prep-github-publish.sh; self-tested against throwaway
# local repositories by infra/lint/the-mirrors-own-merge-is-not-foreign-work.sh.

# foreign_mirror_commits <source_ref> <mirror_ref>
#
# Prints one TAB-separated line per foreign mirror commit:
#   <short sha><TAB><subject><TAB><space-separated files it changed>
# Exit 0 = classified; an empty stdout means every mirror-only commit is
#          bookkeeping.
# Exit 2 = could not classify. Nothing on stdout, the reason on stderr.
foreign_mirror_commits() {
    local source_ref="$1" mirror_ref="$2"
    local only source_trees commit tree parents parent parent_tree
    local subject files benign

    only=$(git rev-list "$source_ref".."$mirror_ref") || {
        echo "mirror-drift: git rev-list $source_ref..$mirror_ref failed" >&2
        return 2
    }
    [ -n "$only" ] || return 0

    # Every tree the source history has ever held. One walk, reused for
    # each mirror-only commit (there are normally one or two).
    source_trees=$(git log --format=%T "$source_ref") || {
        echo "mirror-drift: cannot list the trees of $source_ref" >&2
        return 2
    }
    [ -n "$source_trees" ] || {
        echo "mirror-drift: $source_ref yielded no trees; refusing to call the mirror clean on an empty history" >&2
        return 2
    }

    while read -r commit; do
        [ -n "$commit" ] || continue
        tree=$(git rev-parse --verify --quiet "$commit^{tree}") || {
            echo "mirror-drift: cannot read the tree of mirror commit $commit" >&2
            return 2
        }

        # Shape 1: content the source history already holds.
        if grep -qxF "$tree" <<<"$source_trees"; then
            continue
        fi

        # Shape 2: changed no file against any parent.
        parents=$(git rev-list --parents -n 1 "$commit") || {
            echo "mirror-drift: cannot read the parents of mirror commit $commit" >&2
            return 2
        }
        benign=0
        for parent in ${parents#* }; do
            [ "$parent" = "$parents" ] && break # a root commit: no parents
            parent_tree=$(git rev-parse --verify --quiet "$parent^{tree}") || {
                echo "mirror-drift: cannot read the tree of $parent, parent of $commit" >&2
                return 2
            }
            if [ "$parent_tree" = "$tree" ]; then
                benign=1
                break
            fi
        done
        [ "$benign" -eq 1 ] && continue

        # Foreign: name it and the files it changed. Against the first
        # parent for a child commit, against the empty tree for a root —
        # either way, the files this commit put on the mirror.
        subject=$(git log -1 --format=%s "$commit") || {
            echo "mirror-drift: cannot read the subject of $commit" >&2
            return 2
        }
        files=$(git diff-tree -r -m --root --no-commit-id --name-only "$commit") || {
            echo "mirror-drift: cannot diff mirror commit $commit" >&2
            return 2
        }
        files=$(printf '%s\n' "$files" | sort -u | grep . | tr '\n' ' ')
        printf '%s\t%s\t%s\n' \
            "$(git rev-parse --short "$commit")" "$subject" "${files% }"
    done <<<"$only"

    return 0
}
