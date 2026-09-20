//! BOSS_PUBLIC_URL may only name a hostname whose OIDC redirect
//! `infra/cluster/dns/access.toml` declares registered at the IdP.
//!
//! WHY THIS EXISTS (backlog 198c5fe9). The gateway builds its OIDC
//! redirect_uri from BOSS_PUBLIC_URL (`<url>/api/auth/oidc/callback`,
//! boss-gateway oidc.rs) and the IdP refuses a code flow whose
//! redirect_uri it does not have registered — so flipping
//! BOSS_PUBLIC_URL to a hostname whose redirect is not registered on
//! Kanidm's `boss` client breaks every login the moment the manifest
//! converges, and nothing else in the gate would say so: the gate never
//! talks to Kanidm, and the pod cannot. The fact is therefore DECLARED
//! in access.toml with its provenance, and this is the one check that
//! reads the manifests against the declaration.
//!
//! WHY IT LIVES HERE AND NOT IN infra/lint (backlog 63a92827). It was a
//! pre-flight shell lint with its own awk state machine over
//! access.toml — a SECOND reader of a file whose first reader is the
//! `dns.observe` handler in this crate, pinned by nothing. The two were
//! free to disagree, and did: a TABLE-HEADER typo (`[[oidc_redirct]]`)
//! is refused by name by [`parse_access_declaration`] — the document's
//! own table is closed, so a misspelled header is an unknown key of the
//! root — while the awk simply saw no matching header and judged the
//! manifests against one fewer redirect, printing `ok`. A file the
//! observer REFUSES read clean in the gate (measured 2026-09-20, the
//! red that opened this car). CLAUDE.md §9a: collapse it if you can.
//! The collapse is available because one of the two readers is already
//! the authority, so the awk was deleted and the judgement moved to
//! where the one parse lives. The cost is that the check now runs in
//! the gate's test phase rather than its pre-flight, so
//! `infra/gate.sh --quick` no longer carries it; the gate still does.

use boss_dispatcher_handlers::handlers::dns_observe::parse_access_declaration;
use std::path::{Path, PathBuf};

const ZONE: &str = "algedonic.dev";
const ACCESS_REL: &str = "infra/cluster/dns/access.toml";
const MANIFESTS_REL: &str = "infra/cluster/manifests";

/// `(file, url)` for every BOSS_PUBLIC_URL value under `manifests`, in
/// either env idiom: `{name: BOSS_PUBLIC_URL, value: "..."}` on one
/// line, or `- name: BOSS_PUBLIC_URL` followed by `value: "..."`.
fn public_urls(manifests: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(manifests) else {
        return found;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "yaml"))
        .collect();
    files.sort();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut pending = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                continue;
            }
            let names_it = trimmed.contains("BOSS_PUBLIC_URL");
            if names_it && trimmed.contains("value:") {
                found.push((name.clone(), value_after(trimmed)));
                pending = false;
            } else if names_it {
                pending = true;
            } else if pending && trimmed.contains("value:") {
                found.push((name.clone(), value_after(trimmed)));
                pending = false;
            } else if pending && trimmed.contains("name:") {
                pending = false;
            }
        }
    }
    found
}

/// The value a `value:` line carries, stripped of the quoting and the
/// inline-map punctuation both idioms wrap it in.
fn value_after(line: &str) -> String {
    let (_, rest) = line.split_once("value:").unwrap_or(("", ""));
    rest.trim()
        .trim_matches(|c: char| c == '{' || c == '}' || c == '"' || c == '\'' || c == ',')
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '}' || c == ',')
        .to_string()
}

fn host_of(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    after_scheme
        .split('/')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The judgement, over a tree shaped like the repo: `Ok` carries one
/// line per BOSS_PUBLIC_URL checked, `Err` the refusals. Every fact it
/// reads about the declaration comes from [`parse_access_declaration`]
/// — there is no second parse of access.toml to disagree with.
fn judge(tree: &Path) -> Result<Vec<String>, String> {
    let access = tree.join(ACCESS_REL);
    let text = std::fs::read_to_string(&access).map_err(|e| {
        format!(
            "{ACCESS_REL} could not be read ({e}) — the redirect declaration is where BOSS_PUBLIC_URL's hostname must be declared registered"
        )
    })?;
    let declaration = parse_access_declaration(&text, ZONE)?;
    if declaration.oidc_redirect.is_empty() {
        return Err(format!(
            "{ACCESS_REL} declares no [[oidc_redirect]] entry — nothing to judge BOSS_PUBLIC_URL against"
        ));
    }
    let urls = public_urls(&tree.join(MANIFESTS_REL));
    if urls.is_empty() {
        return Err(format!(
            "no BOSS_PUBLIC_URL in {MANIFESTS_REL}/*.yaml — the check has lost its subject"
        ));
    }
    let mut checked = Vec::new();
    let mut refusals = Vec::new();
    for (file, url) in urls {
        let host = host_of(&url);
        match declaration
            .oidc_redirect
            .iter()
            .find(|r| r.hostname == host)
        {
            None => refusals.push(format!(
                "REFUSED   {file}: BOSS_PUBLIC_URL={url} names {host}, and {ACCESS_REL} has no [[oidc_redirect]] entry for it — declare the redirect (registered = true only after reading it on the IdP: kanidm system oauth2 get boss)"
            )),
            Some(r) if !r.registered => refusals.push(format!(
                "REFUSED   {file}: BOSS_PUBLIC_URL={url} names {host}, whose [[oidc_redirect]] is declared registered = false in {ACCESS_REL} — every login would break at the first redirect; register https://{host}/api/auth/oidc/callback on the boss client, re-read it, then flip the declaration"
            )),
            Some(_) => checked.push(format!(
                "ok        {file}: BOSS_PUBLIC_URL={url} — {host} redirect declared registered"
            )),
        }
    }
    if refusals.is_empty() {
        Ok(checked)
    } else {
        Err(refusals.join("\n"))
    }
}

/// A tree shaped like the repo: a manifest carrying `BOSS_PUBLIC_URL`
/// the way boss.yaml does, and an access.toml declaring the given
/// `(hostname, registered)` redirects.
fn fixture(case: &str, public_url: Option<&str>, redirects: &[(&str, bool)]) -> PathBuf {
    let tree = boss_testing::scratch_dir(&format!("public-url-oidc-{case}")).join("tree");
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
    let mut access = format!("account_zone = \"{ZONE}\"\n");
    for (host, registered) in redirects {
        access.push_str(&format!(
            "\n[[oidc_redirect]]\nhostname = \"{host}\"\nredirect = \"https://{host}/api/auth/oidc/callback\"\nregistered = {registered}\nmeasured = \"fixture\"\n"
        ));
    }
    let access_path = tree.join(ACCESS_REL);
    boss_testing::create_dir(access_path.parent().expect("access.toml has a directory"));
    boss_testing::write_file(&access_path, &access);
    tree
}

#[test]
fn the_tree_as_shipped_names_only_registered_redirects() {
    let checked = judge(&boss_testing::repo_root()).expect("the shipped tree is clean");
    assert!(
        checked.iter().any(|l| l.contains("boss.algedonic.dev")),
        "the check names the hostname it judged: {checked:?}"
    );
}

/// THE COUNT, asserted (backlog 63a92827). The collapse is only worth
/// what something says about the number: a file that declares two
/// redirects and loses one to a typo must not read as a smaller truth.
/// The parse refuses a misspelled header, and this says how many
/// entries the shipped file is expected to hold — a third redirect is
/// a deliberate edit here, not a silent one.
#[test]
fn the_declaration_declares_exactly_the_two_redirects_the_estate_registered() {
    let text = std::fs::read_to_string(boss_testing::repo_root().join(ACCESS_REL))
        .expect("access.toml ships beside the zone file");
    let declaration = parse_access_declaration(&text, ZONE).expect("parses for the zone");
    let hostnames: Vec<&str> = declaration
        .oidc_redirect
        .iter()
        .map(|r| r.hostname.as_str())
        .collect();
    assert_eq!(
        hostnames,
        vec!["boss.algedonic.dev", "playground.algedonic.dev"],
        "both callbacks were registered on the Kanidm boss client on 2026-08-12; a redirect that disappears from this list is a login that breaks"
    );
}

#[test]
fn a_misspelled_table_header_is_refused_rather_than_read_as_one_fewer() {
    let tree = fixture(
        "header-typo",
        Some("https://boss.algedonic.dev"),
        &[
            ("boss.algedonic.dev", true),
            ("playground.algedonic.dev", true),
        ],
    );
    // Only the SECOND entry's header is misspelled — the one no
    // manifest names. The awk this replaced still found boss.'s
    // redirect, printed `ok`, and left the estate one registration
    // short. The parse refuses the file by name instead.
    let access = tree.join(ACCESS_REL);
    let text = std::fs::read_to_string(&access).expect("the fixture declaration is readable");
    let at = text
        .rfind("[[oidc_redirect]]")
        .expect("the fixture declares two redirects");
    let typo = format!(
        "{}[[oidc_redirct]]{}",
        &text[..at],
        &text[at + "[[oidc_redirect]]".len()..]
    );
    std::fs::write(&access, &typo).expect("the fixture declaration is writable");
    let err = judge(&tree).expect_err("a declaration the observer refuses is refused here too");
    assert!(
        err.contains("oidc_redirct"),
        "the refusal names the misspelled header: {err}"
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
    assert_eq!(judge(&tree).expect("clean").len(), 1);
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
    let err = judge(&tree).expect_err("an unregistered redirect is refused");
    assert!(
        err.contains("boss.algedonic.dev") && err.contains("registered = false"),
        "the refusal names the hostname and the declared fact: {err}"
    );
    assert!(
        err.contains("boss.yaml"),
        "and the manifest that names it: {err}"
    );
}

#[test]
fn a_public_url_whose_hostname_no_redirect_declares_is_refused() {
    let tree = fixture(
        "undeclared",
        Some("https://new.algedonic.dev"),
        &[("boss.algedonic.dev", true)],
    );
    let err = judge(&tree).expect_err("an undeclared hostname is not a registered one");
    assert!(
        err.contains("new.algedonic.dev") && err.contains("no [[oidc_redirect]]"),
        "{err}"
    );
}

#[test]
fn a_tree_with_no_public_url_is_a_check_that_lost_its_subject() {
    let tree = fixture("no-subject", None, &[("boss.algedonic.dev", true)]);
    let err = judge(&tree).expect_err("no subject is not a pass");
    assert!(err.contains("BOSS_PUBLIC_URL"), "{err}");
}

#[test]
fn a_tree_with_no_access_declaration_cannot_say_clean() {
    let tree = fixture("no-declaration", Some("https://boss.algedonic.dev"), &[]);
    std::fs::remove_file(tree.join(ACCESS_REL)).expect("the fixture declaration is removable");
    let err = judge(&tree).expect_err("a missing declaration is not a pass");
    assert!(err.contains("access.toml"), "{err}");
}
