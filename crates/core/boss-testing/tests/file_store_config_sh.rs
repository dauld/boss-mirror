//! The `[files]` block that turns on the packet attachment surface,
//! and the two things that must travel with it.
//!
//! `file_refs` — bytes attached to a subject, job, step or event,
//! keyed `sha256/<hex>` — has been built, tested and shipped for a
//! long time and was never switched on, so it had zero callers. This
//! pins the config that switches it on, because two of its three
//! parts fail SILENTLY when omitted:
//!
//! 1. Without `policy_api_url`, `build_files_router` falls back to
//!    `PermissivePolicyClient` and the upload/download surface runs
//!    with no policy enforcement at all — announced only by a
//!    `tracing::warn`. Turning on `[files]` without it opens an
//!    unauthenticated machine door.
//! 2. Without the store in `infra/backup.sh`, the `file_refs` rows
//!    still ride the pg_dump, so a restore produces metadata insisting
//!    bytes exist at a path holding nothing — and reports success.
//!
//! Both are read here out of the scripts themselves, so deleting
//! either line fails by name instead of by incident.

use boss_testing::repo_root;
use std::fs;

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("reading {}: {e}", p.display()))
}

/// The `content)` case of `emit_solo_config`, where boss-content-api's
/// TOML is generated.
fn content_config_case() -> String {
    let deploy = read("infra/deploy-services.sh");
    // Anchor inside emit_solo_config first: `description_of` has its
    // own `content)` arm earlier in the file, and matching that one
    // reads the service's human label instead of its config.
    let fn_start = deploy
        .find("emit_solo_config() {")
        .expect("deploy-services.sh defines emit_solo_config");
    let deploy = &deploy[fn_start..];
    let start = deploy
        .find("\n        content)")
        .expect("emit_solo_config has a `content)` config case");
    let rest = &deploy[start + 1..];
    let end = rest.find("\n            ;;").expect("the case terminates");
    rest[..end].to_string()
}

#[test]
fn the_content_config_turns_the_file_surface_on() {
    let case = content_config_case();
    assert!(
        case.contains("[files]"),
        "boss-content-api's config must carry a `[files]` block, or the \
         attachment surface mounts a 503 fallback and every packet that \
         wants to carry bytes has nowhere to put them:\n{case}"
    );
    assert!(
        case.contains("root = \"$BOSS_FILES_ROOT\""),
        "the store root must come from the shared definition, not a \
         literal — infra/backup.sh reads the same one:\n{case}"
    );
}

#[test]
fn turning_on_files_also_wires_policy() {
    // The security half. `[files]` without `policy_api_url` is an
    // unauthenticated upload/download surface, and the binary only
    // warns.
    let case = content_config_case();
    if !case.contains("[files]") {
        return; // covered by the test above
    }
    assert!(
        case.contains("policy_api_url"),
        "a `[files]` block without `policy_api_url` makes \
         build_files_router fall back to PermissivePolicyClient — file \
         uploads and downloads with NO policy enforcement, behind only \
         a tracing::warn. The two lines ship together or not at \
         all:\n{case}"
    );
}

#[test]
fn the_backup_copies_the_attachment_store() {
    // Conservation: the rows are in the pg_dump either way, so a
    // backup that skips the bytes restores dangling references and
    // calls it a success.
    let backup = read("infra/backup.sh");
    assert!(
        backup.contains("$BOSS_FILES_ROOT"),
        "infra/backup.sh must copy the attachment store; without it a \
         restore yields file_refs rows pointing at bytes that no longer \
         exist, and reports success"
    );
}

#[test]
fn the_store_path_has_exactly_one_definition() {
    // CLAUDE.md §9a. Two readers that must agree, so the path is
    // defined once and sourced — a drift here would silently stop
    // backups covering the store while both scripts kept working.
    let shared = read("infra/files-root.sh");
    assert!(
        shared.contains("BOSS_FILES_ROOT="),
        "infra/files-root.sh is the single definition of the store path"
    );
    for script in ["infra/deploy-services.sh", "infra/backup.sh"] {
        let body = read(script);
        assert!(
            body.contains("files-root.sh"),
            "{script} must source infra/files-root.sh rather than \
             hardcoding the path"
        );
        assert!(
            !body.contains("/var/lib/boss/files"),
            "{script} still hardcodes the store path — that is the \
             duplication files-root.sh exists to remove"
        );
    }
}
