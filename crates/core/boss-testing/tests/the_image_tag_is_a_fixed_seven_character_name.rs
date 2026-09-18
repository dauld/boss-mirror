//! The image tag the cluster deploy runner pushes and the tag the
//! boss-gcp CLI installer pulls are ONE name: the first seven
//! characters of the merge's full sha.
//!
//! Measured 2026-09-17 (backlog 7f4a5a3c): the runner tagged with
//! `git rev-parse --short`, whose length git chooses from the object
//! count, and on this history it had grown to eight — the cluster ran
//! `boss:3b73d515` while `install-cli-from-image.sh` pulled
//! `boss:3b73d51` (`${SHA:0:7}`) and every boss-gcp converge failed
//! its CLI step with MANIFEST_UNKNOWN. A tag is a name with no
//! ambiguity to resolve, so both sides take a fixed substring; this
//! test is the equality test CLAUDE.md §9a asks for when a fact must
//! live in two files.

use boss_testing::repo_root;

const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const INSTALLER: &str = "infra/estate/install-cli-from-image.sh";

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The runner's tag is a seven-character substring of the full sha.
#[test]
fn the_runner_tags_with_seven_characters_of_the_full_sha() {
    let runner = read(RUNNER);
    assert!(
        runner.contains("HEAD_FULL=$(git rev-parse forgejo/main)")
            && runner.contains("HEAD=${HEAD_FULL:0:7}"),
        "{RUNNER}: the tag must be HEAD=${{HEAD_FULL:0:7}} of the full sha"
    );
    let code = runner
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("rev-parse --short"),
        "{RUNNER}: `git rev-parse --short` has a length git chooses (it was 8 on 2026-09-17); \
         a tag is a fixed seven-character name"
    );
}

/// The installer pulls the same seven characters.
#[test]
fn the_installer_pulls_the_same_seven_characters() {
    let installer = read(INSTALLER);
    assert!(
        installer.contains("TAG=\"${SHA:0:7}\""),
        "{INSTALLER}: the tag must be TAG=\"${{SHA:0:7}}\" — the runner's name"
    );
}
