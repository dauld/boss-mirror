//! What this binary was built from, and whether that is still main.
//!
//! The operator's tools are a door like any other, and a door that
//! cannot say it is stale gets trusted while stale (895c9a3b: the pod's
//! `boss` ran eleven hours behind main, and a guard that had landed was
//! unenforced on the box that gates). `boss --version` carries the
//! commit; `boss orient` — the session's first verb — compares it to
//! `origin/main` and prints the rebuild line when they differ. The
//! comparison is a pure function so the wording is pinned.

/// The commit this binary was compiled from (`build.rs`): a full sha
/// from git, or `unknown`. In the cluster image there is no git in the
/// build stage and — since 2026-09-12 — no `BOSS_BUILD_COMMIT` at
/// compile time either (it moved to the runtime stage so a train
/// without a Rust change ships without a Rust build), so there this is
/// `unknown` and [`built_from`] reads the runtime variable instead.
pub const COMPILED_FROM: &str = env!("BOSS_CLI_BUILT_FROM");

/// The commit this binary was built from, as the operator should read
/// it: the compile-time sha when git could say, else the image's
/// `BOSS_BUILD_COMMIT` from the environment, else `unknown`. Resolved
/// once per process.
pub fn built_from() -> &'static str {
    static BUILT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| resolve_built_from(COMPILED_FROM, std::env::var("BOSS_BUILD_COMMIT").ok()))
}

/// PURE: the precedence [`built_from`] applies.
pub fn resolve_built_from(compiled: &str, runtime: Option<String>) -> String {
    if compiled != "unknown" && !compiled.trim().is_empty() {
        return compiled.to_string();
    }
    runtime
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// What `boss --version` prints: the crate version and the commit.
pub fn version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| format!("{} built from {}", env!("CARGO_PKG_VERSION"), built_from()))
}

/// The rebuild recipe the pod uses — one line, the cargo bound sourced
/// from its one home (infra/dev/pod-build.env) rather than retyped.
pub const REBUILD: &str = "set -a; . infra/dev/pod-build.env; set +a; \
                           CARGO_TARGET_DIR=/scratch/target cargo build -p boss-cli";

/// Where the built commit stands against `origin/main`, by ancestry —
/// the question the pod shim (infra/dev/boss) already answers with
/// `git merge-base --is-ancestor`. Equality was the first comparison
/// and it read a CLI built from a car branch on CURRENT main as STALE
/// (f63da97f, 2026-09-19): the tree was newer, and the rebuild the
/// line prescribed would have produced the same verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ancestry {
    /// origin/main is an ancestor of the built commit: a branch cut
    /// from current main, or main itself.
    MainIsAncestor,
    /// It is not: the binary predates main, or sits on a stale base.
    MainIsNotAncestor,
    /// git could not say (the built commit is not in this repository,
    /// or git is absent) — not evidence of either.
    Unknown,
}

/// `git merge-base --is-ancestor <main> <built>` in the cwd: exit 0
/// is an ancestor, 1 is not, anything else is a question git could
/// not answer (an unfetched commit, no repository).
pub fn ancestry(built: &str, main: &str) -> Ancestry {
    match std::process::Command::new("git")
        .args(["merge-base", "--is-ancestor", main, built])
        .output()
    {
        Ok(out) if out.status.success() => Ancestry::MainIsAncestor,
        Ok(out) if out.status.code() == Some(1) => Ancestry::MainIsNotAncestor,
        _ => Ancestry::Unknown,
    }
}

/// One line for `boss orient`: current (main itself, or ahead of it on
/// a current base), stale (with the rebuild line), unplaceable, or
/// unknown — never silent, because silence is what let the lag last.
/// `main` is `None` when origin/main could not be read; that is said
/// too, since "could not compare" is not "current".
pub fn freshness_line(built: &str, main: Option<&str>, ancestry: Ancestry) -> String {
    let short = |s: &str| s.chars().take(7).collect::<String>();
    match main {
        _ if built == "unknown" => "  binary    built from an unknown commit (no git and no \
                                    BOSS_BUILD_COMMIT in the environment) — cannot tell whether it \
                                    lags main"
            .to_string(),
        None => format!(
            "  binary    built from {} — origin/main unreadable, so freshness is UNKNOWN",
            short(built)
        ),
        Some(m) if m == built => format!("  binary    built from {} = origin/main", short(built)),
        Some(m) => match ancestry {
            Ancestry::MainIsAncestor => format!(
                "  binary    built from {} — AHEAD of origin/main {} on a current base: current",
                short(built),
                short(m)
            ),
            Ancestry::MainIsNotAncestor => format!(
                "  binary    built from {} but origin/main is {} — STALE: this verb and every \
                 other ran an older tree. Rebuild: {REBUILD}",
                short(built),
                short(m)
            ),
            Ancestry::Unknown => format!(
                "  binary    built from {} — git cannot place it against origin/main {} (not \
                 fetched here?), so freshness is UNKNOWN; if in doubt rebuild: {REBUILD}",
                short(built),
                short(m)
            ),
        },
    }
}

/// `origin/main`'s head as the forge reports it right now (an
/// `ls-remote`, no fetch), or `None` with the reason on stderr.
pub fn origin_main_head() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["ls-remote", "--heads", "origin", "refs/heads/main"])
        .output()
        .ok()?;
    if !out.status.success() {
        eprintln!(
            "boss orient: could not read origin/main: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_matching_commit_reads_current() {
        let l = freshness_line(
            "85e102f6551451709546c535aa3e785fccd2a7aa",
            Some("85e102f6551451709546c535aa3e785fccd2a7aa"),
            Ancestry::MainIsAncestor,
        );
        assert!(l.contains("85e102f = origin/main"), "{l}");
        assert!(!l.contains("STALE"));
    }

    #[test]
    fn a_lagging_binary_is_named_stale_with_the_rebuild_line() {
        let l = freshness_line(
            "131a783fa05a229d0d2492c2bf62e4826515df15",
            Some("85e102f6551451709546c535aa3e785fccd2a7aa"),
            Ancestry::MainIsNotAncestor,
        );
        for must in [
            "131a783",
            "85e102f",
            "STALE",
            "cargo build -p boss-cli",
            "pod-build.env",
        ] {
            assert!(l.contains(must), "line must carry {must}: {l}");
        }
    }

    #[test]
    fn an_unreadable_main_is_unknown_not_current() {
        let l = freshness_line(
            "131a783fa05a229d0d2492c2bf62e4826515df15",
            None,
            Ancestry::Unknown,
        );
        assert!(l.contains("UNKNOWN") && l.contains("131a783"), "{l}");
        assert!(!l.contains("= origin/main"));
    }

    /// The 2026-09-19 reading: a CLI built from a car branch whose base
    /// IS origin/main printed STALE and prescribed a rebuild that would
    /// have printed the same. Ancestry, not equality, places it.
    #[test]
    fn a_binary_ahead_of_main_on_a_current_base_is_current_not_stale() {
        let l = freshness_line(
            "b688791d678c9c08cf51648740ca01b50fcc766c",
            Some("be78c8150000000000000000000000000000abcd"),
            Ancestry::MainIsAncestor,
        );
        assert!(
            l.contains("AHEAD of origin/main be78c81") && l.contains("current"),
            "{l}"
        );
        assert!(!l.contains("STALE") && !l.contains("cargo build"), "{l}");
    }

    #[test]
    fn a_binary_git_cannot_place_is_unknown_with_the_rebuild_line_not_stale() {
        let l = freshness_line(
            "b688791d678c9c08cf51648740ca01b50fcc766c",
            Some("be78c8150000000000000000000000000000abcd"),
            Ancestry::Unknown,
        );
        assert!(
            l.contains("UNKNOWN") && l.contains("cargo build -p boss-cli"),
            "{l}"
        );
        assert!(!l.contains("STALE"), "{l}");
    }

    /// The ancestry read is git's own answer, on this repository.
    #[test]
    fn ancestry_reads_git_merge_base() {
        let head = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git");
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        assert_eq!(ancestry(&head, &head), Ancestry::MainIsAncestor);
        let parent = std::process::Command::new("git")
            .args(["rev-parse", "HEAD~1"])
            .output()
            .expect("git");
        let parent = String::from_utf8_lossy(&parent.stdout).trim().to_string();
        assert_eq!(ancestry(&head, &parent), Ancestry::MainIsAncestor);
        assert_eq!(ancestry(&parent, &head), Ancestry::MainIsNotAncestor);
        assert_eq!(
            ancestry(&head, "0000000000000000000000000000000000000000"),
            Ancestry::Unknown
        );
    }

    #[test]
    fn an_unstamped_binary_says_so() {
        let l = freshness_line(
            "unknown",
            Some("85e102f6551451709546c535aa3e785fccd2a7aa"),
            Ancestry::Unknown,
        );
        assert!(l.contains("unknown commit"), "{l}");
        assert!(!l.contains("STALE") && !l.contains("= origin/main"));
    }

    #[test]
    fn this_binary_is_stamped() {
        let b = built_from();
        assert!(b == "unknown" || b.len() == 40, "{b}");
        assert!(version().contains(" built from "), "{}", version());
    }

    /// The image compiles without git and without the variable, so
    /// `COMPILED_FROM` is `unknown` there; the runtime stage's
    /// `BOSS_BUILD_COMMIT` is then what the operator reads. A dev build
    /// that git could stamp keeps the compiled sha whatever the
    /// environment says.
    #[test]
    fn the_image_reads_its_commit_from_the_environment_and_a_dev_build_from_git() {
        assert_eq!(
            resolve_built_from("unknown", Some("abc123\n".into())),
            "abc123"
        );
        assert_eq!(resolve_built_from("unknown", Some("  ".into())), "unknown");
        assert_eq!(resolve_built_from("unknown", None), "unknown");
        assert_eq!(
            resolve_built_from(
                "0123456789abcdef0123456789abcdef01234567",
                Some("abc123".into())
            ),
            "0123456789abcdef0123456789abcdef01234567"
        );
    }
}
