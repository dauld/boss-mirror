//! `infra/lint/the-image-carries-what-build-scripts-read.sh` is RUN
//! against synthetic trees, so every property below is one the lint
//! actually has.
//!
//! THE CLASS (2026-09-19, train #473). boss-core gained
//! `include_str!("../../../../infra/platform/tiers.toml")` (ba429e7f);
//! the rust-build stage of `infra/oss-quickstart/Dockerfile` never
//! copied that file, so every image build from be78c815 died on
//! "couldn't read ../infra/platform/tiers.toml" and the cluster could
//! not converge. The lint that exists for exactly this said CARRIED,
//! because the RUNTIME stage has `COPY infra/platform /opt/boss/...`
//! and the ancestor probe matched it. A COPY in another stage is a
//! different filesystem. This file owns the verdict: only the COPYs of
//! the stage that runs cargo count, a runtime-stage COPY is not
//! coverage, an ancestor COPY in the cargo stage still is, and a
//! Dockerfile whose cargo stage cannot be found is red rather than
//! quietly clean.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::PathBuf;
use std::process::{Command, Output};

const LINT: &str = "infra/lint/the-image-carries-what-build-scripts-read.sh";

/// A synthetic repository: one crate that `include_str!`s a file two
/// directories up (`infra/x/y.toml`), the file itself, the lint at the
/// path its own `cd "$(dirname $0)/../.."` resolves from, its helpers,
/// and a Dockerfile the test writes per case.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str, dockerfile: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("image-carries-{tag}"));
        boss_testing::copy_lint_libs(&root);
        let body = std::fs::read_to_string(repo_root().join(LINT))
            .unwrap_or_else(|e| panic!("read {LINT}: {e}"));
        scratch::write_exec(&root.join(LINT), &body);
        scratch::create_dir(&root.join("crates/x/src"));
        scratch::create_dir(&root.join("infra/x"));
        scratch::create_dir(&root.join("infra/oss-quickstart"));
        scratch::write_file(
            &root.join("crates/x/src/lib.rs"),
            "pub const Y: &str = include_str!(\"../../../infra/x/y.toml\");\n",
        );
        scratch::write_file(&root.join("infra/x/y.toml"), "tier = 1\n");
        scratch::write_file(&root.join("infra/oss-quickstart/Dockerfile"), dockerfile);
        Tree(root)
    }

    fn run(&self) -> Output {
        Command::new("bash")
            .arg(self.0.join(LINT))
            .current_dir(&self.0)
            .output()
            .unwrap_or_else(|e| panic!("run the lint in {}: {e}", self.0.display()))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The shape of the real Dockerfile: a cargo stage that copies a
/// subset, then a runtime stage that copies what services read.
fn dockerfile(cargo_stage_copies: &str, runtime_copies: &str) -> String {
    format!(
        "FROM rust:slim AS rust-build\n\
         WORKDIR /build\n\
         COPY Cargo.toml Cargo.lock ./\n\
         COPY crates ./crates\n\
         {cargo_stage_copies}\
         RUN --mount=type=cache,target=/build/target \\\n\
         \x20   cargo build --release --workspace --bins\n\
         \n\
         FROM debian:slim AS runtime\n\
         COPY --from=rust-build /out/ /usr/local/bin/\n\
         {runtime_copies}"
    )
}

#[test]
fn the_file_copied_in_the_cargo_stage_is_carried() {
    let tree = Tree::new(
        "file-in-cargo-stage",
        &dockerfile("COPY infra/x/y.toml ./infra/x/y.toml\n", ""),
    );
    let out = tree.run();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("scanned 1 out-of-crate path"),
        "{}",
        text(&out)
    );
}

#[test]
fn an_ancestor_copied_in_the_cargo_stage_is_carried() {
    let tree = Tree::new(
        "ancestor-in-cargo-stage",
        &dockerfile("COPY infra ./infra\n", ""),
    );
    let out = tree.run();
    assert!(out.status.success(), "{}", text(&out));
}

/// The train #473 shape: the runtime stage copies the whole directory
/// for the services; rustc, in the earlier stage, never sees it.
#[test]
fn a_runtime_stage_copy_is_not_coverage() {
    let tree = Tree::new(
        "runtime-only",
        &dockerfile("", "COPY infra/x /opt/boss/infra/x\n"),
    );
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let t = text(&out);
    assert!(
        t.contains("a crate reads infra/x/y.toml at build time"),
        "{t}"
    );
    assert!(t.contains("runtime stage does not count"), "{t}");
    assert!(t.contains("COPY infra/x/y.toml ./infra/x/y.toml"), "{t}");
}

#[test]
fn a_file_copied_nowhere_is_red() {
    let tree = Tree::new("nowhere", &dockerfile("", ""));
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("a crate reads infra/x/y.toml at build time"),
        "{}",
        text(&out)
    );
}

/// A Dockerfile with no stage running cargo is one this lint cannot
/// read; it says so instead of finding every path uncovered — or, in
/// an earlier shape, covered by any COPY anywhere.
#[test]
fn a_dockerfile_without_a_cargo_stage_is_red_by_name() {
    let tree = Tree::new(
        "no-cargo-stage",
        "FROM debian:slim AS runtime\nCOPY infra ./infra\n",
    );
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out)
            .contains("no stage of infra/oss-quickstart/Dockerfile both runs cargo and COPYs"),
        "{}",
        text(&out)
    );
}

/// The real Dockerfile passes the real lint — the pin that this car's
/// COPY line is where rustc reads from, not just somewhere in the file.
#[test]
fn the_repo_dockerfile_carries_every_reach_in_its_cargo_stage() {
    let out = Command::new("bash")
        .arg(repo_root().join(LINT))
        .current_dir(repo_root())
        .output()
        .expect("run the lint in the repo");
    assert!(out.status.success(), "{}", text(&out));
}
