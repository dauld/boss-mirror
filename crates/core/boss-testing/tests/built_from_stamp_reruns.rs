//! boss-cli's build script recomputes the built-from stamp on EVERY
//! build. It used to key on one worktree's HEAD file, which a shared
//! target dir made the wrong worktree's HEAD (2026-09-12: `boss
//! --version` said 47a31701 for a binary built from a7c3bf4f), and which
//! a commit on a branch never touches at all. The pin is on the forcing
//! key, because the behaviour it buys is what `boss orient`'s freshness
//! line rests on.

#[test]
fn the_cli_build_script_forces_a_rerun_every_build() {
    let p = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../orchestrators/boss-cli/build.rs"
    );
    let src = std::fs::read_to_string(p).expect("boss-cli/build.rs");
    assert!(
        src.contains("cargo:rerun-if-changed=.boss-built-from-is-recomputed-every-build"),
        "build.rs no longer forces a rerun; the stamp will follow whichever worktree built last"
    );
    assert!(
        !src.contains("--git-path"),
        "build.rs keys its rerun on a HEAD path again — that is the stale-stamp defect"
    );
}
