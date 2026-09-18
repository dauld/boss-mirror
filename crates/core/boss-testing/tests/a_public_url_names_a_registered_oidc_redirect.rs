//! `infra/lint/a-public-url-names-a-registered-oidc-redirect.sh` is
//! RUN, not read, against fixture trees shaped like the repo: the
//! manifests directory with a `BOSS_PUBLIC_URL`, and
//! `infra/cluster/dns/access.toml` with its `[[oidc_redirect]]`
//! entries.
//!
//! WHY THIS EXISTS (backlog 198c5fe9). The gateway builds its OIDC
//! redirect_uri from BOSS_PUBLIC_URL (`<url>/api/auth/oidc/callback`,
//! boss-gateway oidc.rs) and the IdP refuses a code flow whose
//! redirect_uri it does not have registered — so flipping
//! BOSS_PUBLIC_URL to a hostname whose redirect is not registered on
//! Kanidm's `boss` client breaks every login the moment the manifest
//! converges, and nothing in the gate would have said so: the gate
//! never talks to Kanidm, and the pod cannot. The fact is therefore
//! DECLARED in access.toml with its provenance, and this lint is the
//! one check that reads the manifest against the declaration.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

const LINT_REL: &str = "infra/lint/a-public-url-names-a-registered-oidc-redirect.sh";
const ACCESS_REL: &str = "infra/cluster/dns/access.toml";
const MANIFESTS_REL: &str = "infra/cluster/manifests";

fn scratch(case: &str) -> PathBuf {
    boss_testing::scratch_dir(&format!("public-url-oidc-{case}"))
}

/// A fixture tree: the real lint, a manifest carrying `public_url` the
/// way boss.yaml does, and an access.toml with the given redirect
/// entries (`(hostname, registered)`).
fn fixture(case: &str, public_url: Option<&str>, redirects: &[(&str, bool)]) -> PathBuf {
    let tree = scratch(case).join("tree");
    let manifests = tree.join(MANIFESTS_REL);
    boss_testing::create_dir(&manifests);
    let env_line = public_url
        .map(|u| format!("            - {{name: BOSS_PUBLIC_URL, value: \"{u}\"}}\n"))
        .unwrap_or_default();
    boss_testing::write_file(
        &manifests.join("boss.yaml"),
        &format!(
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: boss\nspec:\n  template:\n    spec:\n      containers:\n        - name: gateway\n          env:\n            - {{name: BOSS_LISTEN, value: \"0.0.0.0:4443\"}}\n{env_line}            - {{name: BOSS_OIDC_CLIENT_ID, value: \"boss\"}}\n"
        ),
    );
    let mut access = String::from("account_zone = \"algedonic.dev\"\n");
    for (host, registered) in redirects {
        access.push_str(&format!(
            "\n[[oidc_redirect]]\nhostname = \"{host}\"\nredirect = \"https://{host}/api/auth/oidc/callback\"\nregistered = {registered}\nmeasured = \"fixture\"\n"
        ));
    }
    let access_path = tree.join(ACCESS_REL);
    boss_testing::create_dir(access_path.parent().unwrap());
    boss_testing::write_file(&access_path, &access);
    let dst = tree.join(LINT_REL);
    boss_testing::create_dir(dst.parent().unwrap());
    std::fs::copy(repo_root().join(LINT_REL), &dst).expect("copy the lint into the fixture");
    boss_testing::copy_lint_libs(&tree);
    tree
}

fn run_lint(tree: &Path) -> (i32, String) {
    let out = Command::new("bash")
        .arg(tree.join(LINT_REL))
        .output()
        .expect("the lint runs");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn the_tree_as_shipped_is_clean() {
    let (code, text) = run_lint(&repo_root());
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("boss.algedonic.dev"),
        "the lint names the hostname it checked: {text}"
    );
}

#[test]
fn a_public_url_whose_redirect_is_declared_registered_is_clean() {
    let tree = fixture(
        "registered",
        Some("https://boss.algedonic.dev"),
        &[
            ("boss.algedonic.dev", true),
            ("playground.algedonic.dev", true),
        ],
    );
    let (code, text) = run_lint(&tree);
    assert_eq!(code, 0, "{text}");
}

#[test]
fn a_public_url_whose_redirect_is_declared_unregistered_is_refused_naming_both() {
    let tree = fixture(
        "unregistered",
        Some("https://boss.algedonic.dev"),
        &[
            ("boss.algedonic.dev", false),
            ("playground.algedonic.dev", true),
        ],
    );
    let (code, text) = run_lint(&tree);
    assert_ne!(code, 0, "{text}");
    assert!(
        text.contains("boss.algedonic.dev") && text.contains("registered = false"),
        "the refusal names the hostname and the declared fact: {text}"
    );
    assert!(
        text.contains("boss.yaml"),
        "and the manifest that names it: {text}"
    );
}

#[test]
fn a_public_url_whose_hostname_no_redirect_declares_is_refused() {
    let tree = fixture(
        "undeclared",
        Some("https://new.algedonic.dev"),
        &[("boss.algedonic.dev", true)],
    );
    let (code, text) = run_lint(&tree);
    assert_ne!(code, 0, "{text}");
    assert!(
        text.contains("new.algedonic.dev") && text.contains("no [[oidc_redirect]]"),
        "an undeclared hostname is not a registered one: {text}"
    );
}

#[test]
fn a_tree_with_no_public_url_is_a_lint_that_lost_its_subject() {
    let tree = fixture("no-subject", None, &[("boss.algedonic.dev", true)]);
    let (code, text) = run_lint(&tree);
    assert_ne!(code, 0, "{text}");
    assert!(text.contains("BOSS_PUBLIC_URL"), "{text}");
}

#[test]
fn a_tree_with_no_access_declaration_cannot_say_clean() {
    let tree = fixture("no-declaration", Some("https://boss.algedonic.dev"), &[]);
    std::fs::remove_file(tree.join(ACCESS_REL)).unwrap();
    let (code, text) = run_lint(&tree);
    assert_ne!(code, 0, "{text}");
    assert!(text.contains("access.toml"), "{text}");
}
