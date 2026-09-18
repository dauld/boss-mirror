//! The platform step-plugin bundle — `infra/platform/step-plugins/`, one
//! `<kind>.toml` per plugin — is well-formed, complete, and points at a
//! JS bundle that exists, read with no database.
//!
//! WHY (backlog 393d3234, consolidation H4, car 2 of 4). Until
//! 2026-09-18 seven migrations were the only place a step plugin's ROW
//! was declared, while the JS it points at lived under
//! `infra/step-plugins/`. The row now lives here: the seed publishes it
//! insert-if-missing by (kind, version) at every start, and
//! `infra/lint/migrations-declare-schema-only.sh` refuses `INSERT INTO
//! step_plugins` in any migration newer than the cutover. The equality
//! pin against the migrations' own rows is
//! `the_step_plugins_bundle_is_the_migrations_pg.rs`; this file holds
//! the rules that need no database and range over every file.
//!
//! The `[[step_plugin]]` shape is authored, not generated —
//! `StepPluginSpec` does not serialize to TOML (TOML has no null) — and
//! the loader refuses a file named for a kind it does not hold, a file
//! holding two, a status other than `active`, and a version below 1.
//! The twelve kinds below are the ones the migrations seeded; a plugin
//! added later is a file dropped in, a JS bundle beside the others, and
//! a line here.

use boss_jobs::seed_loader::{bundle_files, load_step_plugins};
use boss_jobs::step_plugin_seed::{platform_step_plugins_path, step_plugin_js_path};
use boss_jobs::{StepPluginRegistry, StepPluginSpec};
use std::path::Path;

fn bundle() -> Vec<StepPluginSpec> {
    load_step_plugins(platform_step_plugins_path()).expect("the platform step-plugin bundle parses")
}

/// One plugin per file, and the file is named for its kind, so `ls`
/// answers "which plugins does a deployment seed".
#[test]
fn the_bundle_is_one_file_per_kind() {
    let dir = Path::new(platform_step_plugins_path());
    let files = bundle_files(dir).expect("the bundle directory lists its plugin files");
    assert!(!files.is_empty(), "an empty bundle would prove nothing");
    for file in &files {
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("a plugin file has a UTF-8 stem");
        let rows = load_step_plugins(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let kinds: Vec<&str> = rows.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [stem],
            "{} holds exactly its own plugin",
            file.display()
        );
    }
    let mut kinds: Vec<String> = bundle().into_iter().map(|s| s.kind).collect();
    kinds.sort();
    assert_eq!(
        kinds,
        [
            "answer-question",
            "checklist",
            "correction-verdict",
            "diagnostic-call",
            "incident-review",
            "marketing-attribution",
            "marketing-brief",
            "marketing-launch",
            "review-design",
            "scope-declaration",
            "sign-off",
            "sr-triage",
        ],
        "the twelve platform step plugins the migrations seeded, and no other"
    );
}

/// A row and its JS are two artefacts that must agree: every bundled
/// row's `frontend_url` is a bare file name that exists under
/// `infra/step-plugins/`, where the converge runner builds the
/// step-plugins ConfigMap from. The SPA prefers a registered plugin
/// over its built-in surface, so a row whose bundle is missing does
/// not degrade — the step renders broken, and only once deployed.
#[test]
fn every_row_points_at_a_js_bundle_that_exists() {
    for spec in bundle() {
        assert!(
            !spec.frontend_url.contains('/') && spec.frontend_url.ends_with(".js"),
            "{}: frontend_url `{}` is a bare `<name>.js` under infra/step-plugins/",
            spec.kind,
            spec.frontend_url
        );
        let js = step_plugin_js_path(&spec.frontend_url);
        assert!(
            js.is_file(),
            "{}: row names `{}` but {} does not exist",
            spec.kind,
            spec.frontend_url,
            js.display()
        );
    }
}

/// Every bundled row publishes as-is into an empty registry at its
/// declared version — `sign-off` lands at v3 with no v1/v2 history
/// nobody wrote.
#[tokio::test]
async fn every_bundled_plugin_is_publishable_at_its_declared_version() {
    let registry = boss_jobs::InMemoryStepPlugins::new();
    let actor = boss_core::actor::ActorId::Automation("platform-workflow-seed".into());
    let now = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH;
    for spec in bundle() {
        let declared = spec.version;
        let kind = spec.kind.clone();
        let published = registry
            .publish_declared(spec, &actor, now)
            .await
            .expect("a bundle row publishes");
        assert_eq!(
            published.version, declared,
            "{kind}: the declared version lands"
        );
        assert_eq!(
            registry.list_versions(&kind).await.expect("versions").len(),
            1,
            "{kind}: no synthetic history below the declared version"
        );
    }
}

/// A file that declares anything but an active row, or a version below
/// 1, is refused by the loader with the kind named — a bundle row is
/// what a fresh deployment gets, and that is an active row.
#[test]
fn a_bundle_row_must_be_active_at_a_real_version() {
    let retired = "\
[[step_plugin]]
kind = \"x\"
version = 1
status = \"retired\"
label = \"X\"
category = \"platform\"
frontend_url = \"x.js\"
owning_team = \"platform\"
[step_plugin.metadata_schema]
type = \"object\"
";
    let err = boss_jobs::seed_loader::parse_step_plugins(retired, "x.toml")
        .expect_err("a retired declaration is refused");
    assert!(err.to_string().contains("`x`"), "{err}");
    assert!(err.to_string().contains("retired"), "{err}");

    let err = boss_jobs::seed_loader::parse_step_plugins(
        &retired
            .replace("version = 1", "version = 0")
            .replace("retired", "active"),
        "x.toml",
    )
    .expect_err("version 0 is refused");
    assert!(err.to_string().contains("version"), "{err}");
}
