//! The public mirror's URL lives in ONE tree file —
//! `infra/estate/estate.toml`, beside the estate's other spelled-once
//! addresses — and reaches every host as `/etc/boss/sor.env` (backlog
//! f8af6040; the same mechanism as backlog 5222163e, audit H10).
//!
//! WHAT WAS MEASURED, 2026-09-20, while building the marked-claims
//! check (design 59a776c5). The landing page links the public mirror.
//! To check that the link is right, something has to declare what right
//! IS — and nothing did: `https://github.com/algedonic-dev/boss` was a
//! literal in four shell scripts and owned by none of them. One copy sat
//! inside a REFUSAL MESSAGE (`infra/prep-github-publish.sh`, "add it:
//! git remote add github …"), so a move would have left a script
//! telling an operator to add a remote that no longer exists — the
//! worst shape CLAUDE.md §9a names. It drifted freely because
//! `the-estate-address-lives-once` refuses either estate IP and had no
//! equivalent for this one, and it stayed invisible until something
//! tried to READ it as a fact rather than write it as a string.
//!
//! WHAT THIS PINS, each against the file that decides it:
//!
//!   * the source declares it once, as a URL a slug can be derived from;
//!   * `infra/estate/render-sor-env.sh` renders `BOSS_MIRROR_URL` and
//!     `BOSS_MIRROR_SLUG` from that ONE line — so the two cannot differ,
//!     the way `JOBS_API` and `BOSS_JOBS_URL` cannot — and REFUSES a
//!     source that lost the key, naming it;
//!   * no machine-read line outside the source spells the mirror: the
//!     publish path reads it through `infra/lib/sor.sh` like every other
//!     estate address, and the ALLOWANCE below is a named set with a
//!     reason per entry, never a count (§9a);
//!   * the landing page's GitHub link and the marketing claim that
//!     answers it (`apps/web/src/marketing/claims.ts`, `source.repo`)
//!     read the same declared value — the thing the URL having no home
//!     blocked, and the reason this car exists.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::{repo_root, scratch_dir, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCE: &str = "infra/estate/estate.toml";
const RENDER: &str = "infra/estate/render-sor-env.sh";
const CLAIMS: &str = "apps/web/src/marketing/claims.ts";
const LANDING: &str = "apps/web/src/landing/LandingPage.svelte";

/// A file that still spells the mirror on a machine-read line, and WHY
/// it cannot read the rendered file. A named set: an entry whose file no
/// longer carries the literal is stale and fails this test — remove it.
const ALLOWANCE: &[(&str, &str)] = &[
    (
        "apps/web/src/landing/LandingPage.svelte",
        "the page's own href — a visitor's browser reads no env file; it is the CLAIM, held equal to the source by the_landing_pages_link_is_the_declared_mirror below",
    ),
    (
        "infra/ops/verbs/publish-github-pr.json",
        "the verb's `about` prose (registry data) describing what the verb opens a PR against",
    ),
    (
        "infra/ops/verbs/read-publish-checks.json",
        "the verb's `about` prose (registry data) naming the API path it reads",
    ),
    (
        "infra/sim/boss-brewery-sim.service",
        "a systemd Documentation= URL: prose for `systemctl status`, read by no machine",
    ),
];

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// One `key = "value"` line of the source, read the way the renderer
/// reads it (with the shell's eyes: one key per line).
fn source_key(key: &str) -> String {
    let prefix = format!("{key} = \"");
    read(SOURCE)
        .lines()
        .find_map(|l| l.strip_prefix(&prefix).and_then(|r| r.strip_suffix('"')))
        .unwrap_or_else(|| panic!("{SOURCE} declares no `{key}`"))
        .to_string()
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

fn render(args: &[&str], env: &[(&str, &str)]) -> Run {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(RENDER)).args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("render-sor-env.sh runs");
    Run {
        code: out.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&out.stdout).to_string(),
        err: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

fn value_of(text: &str, key: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key}= in:\n{text}"))
        .to_string()
}

// ---------------------------------------------------------------------
// the source, and what the hosts read
// ---------------------------------------------------------------------

#[test]
fn the_source_declares_the_mirror_as_a_url_with_an_owner_and_a_repo() {
    let url = source_key("mirror_url");
    let rest = url
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("mirror_url must be an https URL: {url}"));
    let parts: Vec<&str> = rest.split('/').collect();
    assert_eq!(
        parts.len(),
        3,
        "mirror_url must be https://<host>/<owner>/<repo> — a slug is derived from it: {url}"
    );
    assert!(
        !url.ends_with(".git") && !url.ends_with('/'),
        "mirror_url is the URL a person opens; the clone URL is it plus `.git`: {url}"
    );
}

#[test]
fn the_env_file_carries_the_mirror_and_its_slug_from_one_line() {
    let r = render(&[], &[]);
    assert_eq!(r.code, 0, "{}", r.err);
    let url = source_key("mirror_url");
    assert_eq!(value_of(&r.out, "BOSS_MIRROR_URL"), url);
    // Derived, never declared twice: the slug is the URL's path. The
    // same reason JOBS_API is rendered from sor_url rather than written
    // beside it — two lines can disagree, one line cannot.
    let slug = url.split('/').skip(3).collect::<Vec<_>>().join("/");
    assert_eq!(value_of(&r.out, "BOSS_MIRROR_SLUG"), slug);
    assert!(
        slug.matches('/').count() == 1,
        "the rendered slug must be owner/repo: {slug}"
    );
}

#[test]
fn a_source_with_no_mirror_url_is_refused_by_name() {
    let dir = scratch_dir("mirror-render-refuses");
    let partial: String = read(SOURCE)
        .lines()
        .filter(|l| !l.starts_with("mirror_url"))
        .map(|l| format!("{l}\n"))
        .collect();
    let src = dir.join("estate.toml");
    write_file(&src, &partial);
    let r = render(&[], &[("BOSS_ESTATE_SOURCE", src.to_str().unwrap())]);
    assert_eq!(r.code, 1, "a source with no mirror rendered: {}", r.out);
    assert!(
        r.err.contains("mirror_url") && r.err.contains("refusing"),
        "the refusal must name the missing key: {}",
        r.err
    );
    assert!(
        r.out.is_empty(),
        "nothing may be rendered from a partial source: {}",
        r.out
    );
}

#[test]
fn a_mirror_url_no_slug_can_be_derived_from_is_refused_before_a_publish_uses_it() {
    let dir = scratch_dir("mirror-render-shape");
    let bent: String = read(SOURCE)
        .lines()
        .map(|l| {
            if l.starts_with("mirror_url") {
                // A bare host: `gh pr create --repo` would take it and
                // fail four GraphQL errors deep, in a publish nobody is
                // watching. It is refused here instead, on the host that
                // installs.
                "mirror_url = \"https://github.com\"".to_string()
            } else {
                l.to_string()
            }
        })
        .map(|l| format!("{l}\n"))
        .collect();
    let src = dir.join("estate.toml");
    write_file(&src, &bent);
    let r = render(&[], &[("BOSS_ESTATE_SOURCE", src.to_str().unwrap())]);
    assert_eq!(r.code, 1, "a mirror with no owner/repo rendered: {}", r.out);
    assert!(
        r.err.contains("owner") && r.err.contains("refusing"),
        "the refusal must say what shape it wanted: {}",
        r.err
    );
    assert!(r.out.is_empty(), "{}", r.out);
}

// ---------------------------------------------------------------------
// the tree carries no second copy
// ---------------------------------------------------------------------

/// Every file under the scanned roots, skipping build output.
fn files_under(root: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for e in entries.filter_map(|e| e.ok()) {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name == "node_modules" || name == "target" || name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            files_under(&p, into);
        } else {
            into.push(p);
        }
    }
}

#[test]
fn nothing_but_the_source_spells_the_mirror_on_a_machine_read_line() {
    let slug = {
        let url = source_key("mirror_url");
        url.split('/').skip(3).collect::<Vec<_>>().join("/")
    };
    let root = repo_root();
    let mut files = Vec::new();
    for dir in ["infra", "apps/web/src"] {
        files_under(&root.join(dir), &mut files);
    }
    let mut offences: Vec<String> = Vec::new();
    let mut seen_allowed: Vec<&str> = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        // Prose, history and fixtures, as the estate-address lint
        // excludes them: docs name addresses, migrations are
        // append-only, a test is allowed to spell what it stubs.
        if rel.ends_with(".md")
            || rel.ends_with(".test.ts")
            || rel.starts_with("infra/postgres/schema/")
            || rel == SOURCE
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let hits: Vec<(usize, &str)> = text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains(&slug))
            // A `#` or `//` line is prose, like the docs: it explains
            // history, it does not aim anything at a target.
            .filter(|(_, l)| {
                let t = l.trim_start();
                !t.starts_with('#') && !t.starts_with("//") && !t.starts_with("* ")
            })
            .collect();
        match ALLOWANCE.iter().find(|(p, _)| *p == rel) {
            Some((_, why)) => {
                assert!(
                    !hits.is_empty(),
                    "{rel} is in the allowance ({why}) but no longer spells the mirror — remove the entry (§9a: a named set, kept true)"
                );
                seen_allowed.push(why);
            }
            None => {
                for (n, line) in hits {
                    offences.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        offences.is_empty(),
        "the mirror is spelled outside {SOURCE}. Read it through infra/lib/sor.sh \
         (sor_require BOSS_MIRROR_URL / BOSS_MIRROR_SLUG), as the publish path does, \
         or add a named allowance entry saying why the file cannot:\n{}",
        offences.join("\n")
    );
    assert_eq!(
        seen_allowed.len(),
        ALLOWANCE.len(),
        "an allowance entry names a file that is gone or was not scanned"
    );
}

// ---------------------------------------------------------------------
// the website's link, which is what the URL having no home blocked
// ---------------------------------------------------------------------

/// The claim row that answers the landing page's GitHub link points at
/// the source key this car created. Until the claims resolver lands,
/// this is also the check itself: the page's href IS the declared URL.
#[test]
fn the_landing_pages_link_is_the_declared_mirror() {
    let claims = read(CLAIMS);
    assert!(
        claims.contains("id: 'source.repo'"),
        "{CLAIMS} has no `source.repo` row — the link is still unchecked, which is what backlog f8af6040 was filed to end"
    );
    assert!(
        claims.contains(&format!("where: '{SOURCE}'")) && claims.contains("pointer: 'mirror_url'"),
        "the `source.repo` claim must name {SOURCE} / mirror_url — the same value the publish path reads:\n{claims}"
    );
    let url = source_key("mirror_url");
    let landing = read(LANDING);
    let href = landing
        .lines()
        .find(|l| l.contains("data-claim=\"source.repo\""))
        .unwrap_or_else(|| panic!("{LANDING} does not mark the GitHub link as `source.repo`"));
    assert!(
        href.contains(&format!("href=\"{url}\"")),
        "the page links somewhere other than the declared mirror ({url}): {href}"
    );
}
