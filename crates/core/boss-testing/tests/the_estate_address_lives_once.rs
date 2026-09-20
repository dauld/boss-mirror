//! The system of record's address and the forge's live in ONE tree
//! file — `infra/estate/estate.toml` — and reach every host as
//! `/etc/boss/sor.env` (backlog 5222163e, design 42277636; audit H10 of
//! 2026-09-18).
//!
//! WHAT WAS MEASURED. `http://10.20.0.34:7900` was spelled in 47 files
//! three ways — a `/usr/bin/env BOSS_JOBS_URL=…` prefix on Exec lines,
//! `Environment=JOBS_API=…` in the observers, `${JOBS_API:-…}` fallbacks
//! in the scripts they ran — while `infra/dev/sor-url` called itself
//! "the ONE spelling"; the forge's IP sat in 62 files. Each was a
//! fallback that keeps answering on the day the address moves (CLAUDE.md
//! §Doors: a wrong target answers instead of erroring), and the
//! boss.algedonic.dev cutover would have had to find them all.
//!
//! WHAT THIS PINS, each against the files that decide it:
//!
//!   * `infra/estate/render-sor-env.sh` renders the env file from the
//!     source — every key, both spellings of the record from ONE line,
//!     `--to` atomically, `--value` one key — and REFUSES a source that
//!     lost a key, naming it;
//!   * the pod's door (`infra/dev/sor-url`) is the source's cluster
//!     spelling, and the MetalLB pin that DEFINES the LAN address
//!     (`infra/cluster/manifests/boss-jobs-internal.yaml`) is the
//!     source's host — the two facts that cannot be collapsed into the
//!     TOML are held equal to it (§9a);
//!   * `infra/lib/sor.sh` loads the file — a file NAMED with
//!     `BOSS_SOR_ENV` is the source and replaces the environment, the
//!     default `/etc/boss/sor.env` fills only what is unset — and
//!     `sor_require` refuses by name, naming the file and the installs
//!     that render it — never a fallback literal;
//!   * the two installer lints (`boss-gcp-converges-itself`,
//!     `forge-install-covers-the-ops-runner`) judge the address the
//!     rendered file carries, whatever the process environment says —
//!     the conductor pod runs the consist check with a cluster-internal
//!     `BOSS_JOBS_URL` and no `/etc/boss/sor.env`, and refused this
//!     car for exactly that (2026-09-18);
//!   * `infra/forge/forge-defaults.sh` derives every image repo from the
//!     file's registry host, honours every override it replaced, and
//!     refuses by name when neither says;
//!   * every systemd unit under infra/ that speaks to the record reads
//!     the file with `EnvironmentFile=` — optional (`-`) only on the two
//!     converges that render it — and carries no inline pin;
//!   * the lint `infra/lint/the-estate-address-lives-once.sh` is red on
//!     a planted literal, red on a stale allowance, green on the tree.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCE: &str = "infra/estate/estate.toml";
const RENDER: &str = "infra/estate/render-sor-env.sh";
const SOR_SH: &str = "infra/lib/sor.sh";
const DEFAULTS: &str = "infra/forge/forge-defaults.sh";
const LINT: &str = "infra/lint/the-estate-address-lives-once.sh";
const DEV_SOR_URL: &str = "infra/dev/sor-url";
const LB_MANIFEST: &str = "infra/cluster/manifests/boss-jobs-internal.yaml";

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

fn bash(args: &[&str], env: &[(&str, &str)], unset: &[&str], cwd: Option<&Path>) -> Run {
    let mut cmd = Command::new("bash");
    cmd.args(args);
    for k in unset {
        cmd.env_remove(k);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.current_dir(cwd.unwrap_or(&repo_root()));
    let o = cmd.output().expect("bash runs");
    Run {
        code: o.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&o.stdout).to_string(),
        err: String::from_utf8_lossy(&o.stderr).to_string(),
    }
}

fn render(args: &[&str], env: &[(&str, &str)]) -> Run {
    let script = repo_root().join(RENDER);
    let mut all = vec![script.to_str().unwrap()];
    all.extend_from_slice(args);
    bash(&all, env, &[], None)
}

/// The env file rendered into a scratch dir — the shape every host has.
fn rendered_env(dir: &Path) -> PathBuf {
    let file = dir.join("sor.env");
    let r = render(&["--to", file.to_str().unwrap()], &[]);
    assert_eq!(r.code, 0, "render --to failed: {}{}", r.out, r.err);
    file
}

fn value_of(env_file: &Path, key: &str) -> String {
    std::fs::read_to_string(env_file)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("{} has no {key}=", env_file.display()))
        .to_string()
}

// ---------------------------------------------------------------------
// the renderer
// ---------------------------------------------------------------------

#[test]
fn the_env_file_renders_every_key_from_the_source() {
    let r = render(&[], &[]);
    assert_eq!(r.code, 0, "{}", r.err);
    let sor = source_key("sor_url");
    for (key, want) in [
        ("BOSS_JOBS_URL", sor.clone()),
        ("JOBS_API", sor.clone()),
        ("BOSS_FORGE_HOST", source_key("forge_host")),
        ("BOSS_FORGE_URL", source_key("forge_url")),
        ("BOSS_FORGE_REGISTRY_HOST", source_key("forge_registry")),
        ("BOSS_FORGE_JOURNAL_URL", source_key("forge_journal")),
    ] {
        assert!(
            r.out.lines().any(|l| l == format!("{key}={want}")),
            "the render lacks `{key}={want}`:\n{}",
            r.out
        );
    }
    // Both spellings of the record from ONE source line: the observers
    // read JOBS_API, the wrap reads BOSS_JOBS_URL, and they cannot differ.
    assert_eq!(
        value_of_str(&r.out, "JOBS_API"),
        value_of_str(&r.out, "BOSS_JOBS_URL")
    );
    // A comment saying where it came from and not to edit it: the next
    // converge rewrites the file.
    assert!(r.out.starts_with("# /etc/boss/sor.env"), "{}", r.out);
    assert!(r.out.contains("Do not edit"), "{}", r.out);
}

fn value_of_str(text: &str, key: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key}= in:\n{text}"))
        .to_string()
}

#[test]
fn the_renderer_writes_the_file_atomically_and_answers_one_key() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir("estate-render-to");
    let nested = dir.join("etc/boss/sor.env");
    let r = render(&["--to", nested.to_str().unwrap()], &[]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    // The count is DERIVED from the render, not retyped: it was `6
    // keys` here and grew to 8 when the public mirror moved into the
    // source (backlog f8af6040), which is one edit this file should
    // never have asked for (§9a).
    let keys = render(&[], &[])
        .out
        .lines()
        .filter(|l| l.contains('=') && !l.starts_with('#'))
        .count();
    assert!(
        r.out.contains("wrote") && r.out.contains(&format!("{keys} keys")),
        "the renderer wrote a count that is not the number of keys it renders ({keys}): {}",
        r.out
    );
    let mode = std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o644, "a unit running as another user must read it");
    // No temp file left beside it: the rename either happened or did not.
    let stray: Vec<_> = std::fs::read_dir(nested.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".sor.env."))
        .collect();
    assert!(stray.is_empty(), "temp files left behind: {stray:?}");
    assert_eq!(value_of(&nested, "BOSS_JOBS_URL"), source_key("sor_url"));

    let one = render(&["--value", "BOSS_FORGE_REGISTRY_HOST"], &[]);
    assert_eq!(one.code, 0, "{}", one.err);
    assert_eq!(one.out.trim(), source_key("forge_registry"));
    let none = render(&["--value", "NOT_A_KEY"], &[]);
    assert_eq!(
        none.code, 2,
        "an unknown key is a usage error: {}",
        none.err
    );
}

#[test]
fn a_source_that_lost_a_key_is_refused_by_name() {
    let dir = scratch_dir("estate-render-refuses");
    let partial: String = read(SOURCE)
        .lines()
        .filter(|l| !l.starts_with("forge_journal"))
        .map(|l| format!("{l}\n"))
        .collect();
    let src = dir.join("estate.toml");
    write_file(&src, &partial);
    let r = render(&[], &[("BOSS_ESTATE_SOURCE", src.to_str().unwrap())]);
    assert_eq!(r.code, 1, "a partial source rendered: {}", r.out);
    assert!(
        r.err.contains("forge_journal") && r.err.contains("refusing"),
        "the refusal must name the missing key: {}",
        r.err
    );
    assert!(
        r.out.is_empty(),
        "nothing may be rendered from a partial source: {}",
        r.out
    );
}

// ---------------------------------------------------------------------
// the two facts that cannot live in the TOML, pinned to it (§9a)
// ---------------------------------------------------------------------

/// The pod's door reads the cluster spelling from beside the script;
/// that file is WRITTEN from the source (`render-sor-env.sh
/// --dev-sor-url`), and this holds them equal.
#[test]
fn the_pods_door_is_the_sources_cluster_spelling() {
    let r = render(&["--dev-sor-url"], &[]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(
        read(DEV_SOR_URL).trim(),
        r.out.trim(),
        "{DEV_SOR_URL} is not what `{RENDER} --dev-sor-url` prints — write it from the source"
    );
    assert_eq!(r.out.trim(), source_key("sor_cluster_url"));
}

/// MetalLB assigns the LAN address in the manifest; the source names it.
/// A manifest cannot read TOML, so the pin is the mechanism.
#[test]
fn the_metallb_pin_is_the_sources_address() {
    let sor = source_key("sor_url");
    let host = sor
        .strip_prefix("http://")
        .and_then(|r| r.split(':').next())
        .unwrap_or_else(|| panic!("sor_url is not http://<host>:<port>: {sor}"));
    let manifest = read(LB_MANIFEST);
    let pinned = manifest
        .lines()
        .find_map(|l| l.trim().strip_prefix("loadBalancerIP:"))
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| panic!("{LB_MANIFEST} pins no loadBalancerIP"));
    assert_eq!(
        pinned, host,
        "{LB_MANIFEST} pins {pinned} but {SOURCE} sor_url says {host} — they move together"
    );
    assert!(
        manifest.contains(&format!("loadBalancerIPs: \"{host}\"")),
        "the MetalLB annotation names another address than the spec"
    );
}

// ---------------------------------------------------------------------
// sor.sh — a named file is the source, the default fills gaps, refuse by name
// ---------------------------------------------------------------------

fn sor(body: &str, env: &[(&str, &str)], unset: &[&str]) -> Run {
    let script = format!(". '{}'\n{body}\n", repo_root().join(SOR_SH).display());
    bash(&["-c", &script], env, unset, None)
}

#[test]
fn sor_sh_loads_the_file_and_a_named_file_outranks_the_environment() {
    let dir = scratch_dir("estate-sor-sh");
    let file = rendered_env(&dir);
    let f = file.to_str().unwrap();
    let r = sor(
        "sor_require BOSS_JOBS_URL JOBS_API BOSS_FORGE_REGISTRY_HOST; echo \"$BOSS_JOBS_URL|$JOBS_API|$BOSS_FORGE_REGISTRY_HOST\"",
        &[("BOSS_SOR_ENV", f)],
        &["BOSS_JOBS_URL", "JOBS_API"],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    let sor_url = source_key("sor_url");
    assert_eq!(
        r.out.trim(),
        format!("{sor_url}|{sor_url}|{}", source_key("forge_registry"))
    );
    // NAMING the file names the source: a converge that has just
    // rendered it names it (its own unit loaded the PREVIOUS file at
    // start, so on the day the address moves the environment is the
    // stale copy), and a lint that rendered a scratch copy names that —
    // in the conductor pod the environment carries a cluster-internal
    // BOSS_JOBS_URL that is nobody's expected answer. The named file
    // replaces what the environment had.
    let r = sor(
        "sor_require JOBS_API; echo \"$JOBS_API\"",
        &[("BOSS_SOR_ENV", f), ("JOBS_API", "http://stub")],
        &[],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(
        r.out.trim(),
        sor_url,
        "a file named with BOSS_SOR_ENV outranks the environment"
    );
    // With NO file named, the environment stands: a unit's
    // EnvironmentFile= has already loaded the default file (the same
    // values), and a test's stub is the address the test means.
    let r = sor(
        "sor_require JOBS_API; echo \"$JOBS_API\"",
        &[("JOBS_API", "http://stub")],
        &["BOSS_SOR_ENV"],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(
        r.out.trim(),
        "http://stub",
        "with no file named, the environment is not overridden"
    );
    // Exported, so a child process (the installer's sub-installers) sees it.
    let r = sor(
        "bash -c 'echo \"$BOSS_FORGE_JOURNAL_URL\"'",
        &[("BOSS_SOR_ENV", f)],
        &["BOSS_FORGE_JOURNAL_URL"],
    );
    assert_eq!(r.out.trim(), source_key("forge_journal"), "{}", r.err);
}

#[test]
fn sor_require_refuses_without_the_file_and_names_it() {
    let dir = scratch_dir("estate-sor-refuses");
    let absent = dir.join("no-such-sor.env");
    let a = absent.to_str().unwrap();
    let r = sor(
        "sor_require BOSS_JOBS_URL; echo REACHED",
        &[("BOSS_SOR_ENV", a)],
        &["BOSS_JOBS_URL"],
    );
    assert_eq!(r.code, 1, "an absent file must refuse: {}", r.out);
    assert!(
        !r.out.contains("REACHED"),
        "the script carried on past the refusal"
    );
    for phrase in [
        "BOSS_JOBS_URL is not set",
        a,
        "is absent",
        "render-sor-env.sh",
        "infra/forge/install.sh",
        "infra/gcp/boss-gcp-converge.sh",
        "wrong target answers instead of erroring",
    ] {
        assert!(
            r.err.contains(phrase),
            "the refusal lacks `{phrase}`:\n{}",
            r.err
        );
    }
    // A file that exists but lacks the key: a different sentence.
    let partial = dir.join("partial.env");
    write_file(&partial, "BOSS_FORGE_HOST=1.2.3.4\n");
    let r = sor(
        "sor_require BOSS_JOBS_URL",
        &[("BOSS_SOR_ENV", partial.to_str().unwrap())],
        &["BOSS_JOBS_URL"],
    );
    assert_eq!(r.code, 1);
    assert!(
        r.err.contains("carries no BOSS_JOBS_URL= line"),
        "{}",
        r.err
    );
    // And no literal anywhere in the library: the refusal is the design.
    let lib = read(SOR_SH);
    assert!(
        !lib.lines()
            .any(|l| !l.trim_start().starts_with('#') && l.contains("http://")),
        "{SOR_SH} carries an address of its own"
    );
}

// ---------------------------------------------------------------------
// forge-defaults.sh
// ---------------------------------------------------------------------

fn defaults(body: &str, env: &[(&str, &str)], unset: &[&str]) -> Run {
    let script = format!(". '{}'\n{body}\n", repo_root().join(DEFAULTS).display());
    let mut all_unset = vec![
        "BOSS_FORGE_REGISTRY",
        "REGISTRY",
        "BOSS_CI_IMAGE_REPO",
        "BOSS_FORGE_REGISTRY_BASE",
        "BOSS_CI_REGISTRY",
        "BOSS_FORGE_REGISTRY_HOST",
        "BOSS_CONVERGE_HOLD",
        "BOSS_FORGE_LAST_BUILT",
    ];
    all_unset.extend_from_slice(unset);
    bash(&["-c", &script], env, &all_unset, None)
}

#[test]
fn forge_defaults_derive_every_image_repo_from_the_files_registry_host() {
    let dir = scratch_dir("estate-forge-defaults");
    let file = rendered_env(&dir);
    let f = file.to_str().unwrap();
    let reg = source_key("forge_registry");
    let r = defaults(
        "forge_need REGISTRY CI_IMAGE_REPO REGISTRY_BASE; echo \"$REGISTRY|$BOSS_FORGE_REGISTRY|$CI_IMAGE_REPO|$REGISTRY_BASE|$HOLD_FILE|$PG_WORKLOAD|$PG_CONTAINER|$PG_USER|$LAST_BUILT_NAME\"",
        &[("BOSS_SOR_ENV", f)],
        &[],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(
        r.out.trim(),
        format!(
            "{reg}/david/boss|{reg}/david/boss|{reg}/david/boss-ci|{reg}/david|/var/tmp/boss-converge-hold|sts/postgres|postgres|boss|.boss-last-built"
        )
    );
    // Every override the verbs used to read still wins, under both
    // spellings of the image-repo override.
    for (env, want) in [
        (
            vec![
                ("BOSS_SOR_ENV", f),
                ("BOSS_FORGE_REGISTRY", "reg.test/x/boss"),
            ],
            "reg.test/x/boss|reg.test/x/boss",
        ),
        (
            vec![("BOSS_SOR_ENV", f), ("REGISTRY", "reg.test/y/boss")],
            "reg.test/y/boss|reg.test/y/boss",
        ),
    ] {
        let r = defaults(
            "forge_need REGISTRY; echo \"$REGISTRY|$BOSS_FORGE_REGISTRY\"",
            &env,
            &[],
        );
        assert_eq!(r.code, 0, "{}", r.err);
        assert_eq!(r.out.trim(), want);
    }
    let r = defaults(
        "forge_need REGISTRY_BASE; echo \"$REGISTRY_BASE\"",
        &[("BOSS_SOR_ENV", f), ("BOSS_CI_REGISTRY", "reg.test/z")],
        &[],
    );
    assert_eq!(r.out.trim(), "reg.test/z", "{}", r.err);
    let r = defaults(
        "echo \"$HOLD_FILE|$STAMP_FILE\"",
        &[
            ("BOSS_SOR_ENV", f),
            ("BOSS_CONVERGE_HOLD", "/x/hold"),
            ("BOSS_FORGE_LAST_BUILT", "/x/stamp"),
        ],
        &[],
    );
    assert_eq!(r.out.trim(), "/x/hold|/x/stamp", "{}", r.err);
}

#[test]
fn forge_defaults_refuse_an_unknown_registry_by_name_and_only_when_asked() {
    let dir = scratch_dir("estate-forge-defaults-refuse");
    let absent = dir.join("none.env");
    let a = absent.to_str().unwrap();
    // Sourcing alone must not refuse: a verb that never touches the
    // registry (converge-hold, the census) runs where no file exists.
    let r = defaults("echo \"$HOLD_FILE\"", &[("BOSS_SOR_ENV", a)], &[]);
    assert_eq!(r.code, 0, "{}", r.err);
    // shared-tmp-ok: the hold's deliberate operational path, read back as an expectation
    assert_eq!(r.out.trim(), "/var/tmp/boss-converge-hold");
    // Asking is what refuses — by the name of the missing variable.
    let r = defaults(
        "forge_need REGISTRY; echo REACHED",
        &[("BOSS_SOR_ENV", a)],
        &[],
    );
    assert_eq!(r.code, 1, "{}", r.out);
    assert!(!r.out.contains("REACHED"));
    assert!(
        r.err.contains("BOSS_FORGE_REGISTRY_HOST is not set"),
        "{}",
        r.err
    );
    // And no literal registry anywhere in the file.
    let lib = read(DEFAULTS);
    assert!(
        !lib.lines()
            .any(|l| !l.trim_start().starts_with('#') && l.contains(":3000")),
        "{DEFAULTS} carries a registry address of its own"
    );
    // Runs with no HOME at all (the ops runner's environment).
    let r = defaults("echo ok", &[("BOSS_SOR_ENV", a)], &["HOME"]);
    assert_eq!(r.code, 0, "{}", r.err);
}

// ---------------------------------------------------------------------
// the units
// ---------------------------------------------------------------------

/// Every unit under infra/ whose Exec lines speak to the record — the
/// wrap pair, the ops runner, the observers, the watchdog, the smoke —
/// reads /etc/boss/sor.env. Hard, except on the two converges that
/// render the file and must start on a host that has none.
#[test]
fn every_unit_that_speaks_to_the_record_reads_the_file() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "service") {
                out.push(p);
            }
        }
    }
    let mut found = Vec::new();
    walk(&repo_root().join("infra"), &mut found);
    let units: Vec<String> = found
        .iter()
        .map(|p| {
            p.strip_prefix(repo_root())
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    assert!(units.len() > 20, "too few units found: {units:?}");
    let readers = [
        "boss-maintenance-wrap.sh",
        "boss-step.sh",
        "ops-runner.sh",
        "observe-host.sh",
        "observe-units.sh",
        "observe-codebase.sh",
        "cluster-watchdog.sh",
        "install-smoke-nightly.sh",
        "boss train cadence",
    ];
    let optional_ok = [
        "infra/forge/forge-converge.service",
        "infra/gcp/boss-gcp-converge.service",
    ];
    let mut checked = 0;
    for u in &units {
        let text = read(u);
        let exec: Vec<&str> = text.lines().filter(|l| l.starts_with("Exec")).collect();
        if !exec.iter().any(|l| readers.iter().any(|r| l.contains(r))) {
            continue;
        }
        checked += 1;
        let hard = text
            .lines()
            .any(|l| l == "EnvironmentFile=/etc/boss/sor.env");
        let soft = text
            .lines()
            .any(|l| l == "EnvironmentFile=-/etc/boss/sor.env");
        if optional_ok.contains(&u.as_str()) {
            assert!(
                soft,
                "{u}: the converge that renders the file reads it OPTIONALLY (EnvironmentFile=-)"
            );
        } else {
            assert!(
                hard,
                "{u} does not read /etc/boss/sor.env with a hard EnvironmentFile="
            );
            assert!(
                !soft,
                "{u}: only the two converges may read the file optionally"
            );
        }
        for l in text.lines().filter(|l| !l.trim_start().starts_with('#')) {
            assert!(
                !l.contains("/usr/bin/env BOSS_JOBS_URL=") && !l.contains("/usr/bin/env JOBS_API="),
                "{u} still pins the record inline: {l}"
            );
            assert!(
                !(l.starts_with("Environment=BOSS_JOBS_URL=")
                    || l.starts_with("Environment=JOBS_API=")),
                "{u} carries a second copy of the record: {l}"
            );
        }
    }
    assert!(
        checked >= 20,
        "only {checked} units matched the readers — the pattern list has rotted"
    );
}

// ---------------------------------------------------------------------
// the lint
// ---------------------------------------------------------------------

/// A scratch repository with the source and whatever files a case plants.
fn scratch_tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = scratch_dir(name);
    let q = |args: &[&str]| {
        let o = Command::new("git")
            .args(args)
            .current_dir(&dir)
            .output()
            .expect("git");
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    q(&["init", "-q"]);
    q(&["config", "user.email", "t@example"]);
    q(&["config", "user.name", "t"]);
    create_dir(&dir.join("infra/estate"));
    write_file(&dir.join(SOURCE), &read(SOURCE));
    for (rel, body) in files {
        let p = dir.join(rel);
        create_dir(p.parent().unwrap());
        write_file(&p, body);
    }
    q(&["add", "-A"]);
    q(&["commit", "-qm", "fixture"]);
    dir
}

fn lint(tree: &Path) -> Run {
    bash(
        &[repo_root().join(LINT).to_str().unwrap()],
        &[("BOSS_LINT_TREE", tree.to_str().unwrap())],
        &[],
        None,
    )
}

#[test]
fn the_lint_is_red_on_a_planted_literal_and_green_on_the_tree() {
    let sor = source_key("sor_url");
    let forge = source_key("forge_host");
    let red = scratch_tree(
        "estate-lint-red",
        &[
            (
                "infra/x.sh",
                &format!("JOBS_API=\"${{JOBS_API:-{sor}}}\"\n"),
            ),
            (
                "infra/y.service",
                &format!("[Service]\nEnvironment=REGISTRY={forge}:3000/david/boss\n"),
            ),
        ],
    );
    let r = lint(&red);
    assert_eq!(
        r.code, 1,
        "two planted literals passed:\n{}{}",
        r.out, r.err
    );
    assert!(r.err.contains("infra/x.sh:1:"), "{}", r.err);
    assert!(r.err.contains("infra/y.service:2:"), "{}", r.err);
    assert!(r.err.contains("2 file(s) spell"), "{}", r.err);

    // Prose is prose: a comment, a doc, a test fixture, a migration.
    let green = scratch_tree(
        "estate-lint-green",
        &[
            (
                "infra/z.sh",
                &format!("# measured against {sor} on 2026-09-17\necho ok\n"),
            ),
            ("docs/history.md", &format!("the record was {sor}\n")),
            (
                "crates/x/tests/fixture.rs",
                &format!("const R: &str = \"{forge}:3000\";\n"),
            ),
            (
                "infra/postgres/schema/1-x.sql",
                &format!("INSERT INTO nodes VALUES ('{forge}');\n"),
            ),
            (
                "apps/web/src/it/a.test.ts",
                &format!("const t = '{sor}';\n"),
            ),
        ],
    );
    let r = lint(&green);
    assert_eq!(r.code, 0, "prose was refused:\n{}{}", r.out, r.err);
    assert!(r.out.contains("clean"), "{}", r.out);

    // The real tree, from the lint's own location.
    let r = bash(
        &[repo_root().join(LINT).to_str().unwrap()],
        &[],
        &["BOSS_LINT_TREE"],
        None,
    );
    assert_eq!(
        r.code, 0,
        "the tree spells an address outside the source:\n{}{}",
        r.out, r.err
    );
}

#[test]
fn the_lint_refuses_a_stale_allowance_and_notes_a_dead_one() {
    let sor = source_key("sor_url");
    // An allowed path present WITHOUT the literal is stale: the entry
    // would keep a future literal there invisible.
    let stale = scratch_tree(
        "estate-lint-stale",
        &[("infra/gate-runner/run.sh", "echo no address here\n")],
    );
    let r = lint(&stale);
    assert_eq!(r.code, 1, "{}{}", r.out, r.err);
    assert!(
        r.err.contains("STALE allowance") && r.err.contains("infra/gate-runner/run.sh"),
        "{}",
        r.err
    );
    // An allowed path present WITH the literal is allowed; the absent
    // ones are noted, not failed (the MetalLB pin is absent from this
    // scratch tree, present in every real one).
    let allowed = scratch_tree(
        "estate-lint-allowed",
        &[(
            "infra/gate-runner/run.sh",
            &format!("JOBS_API=\"${{JOBS_API:-{sor}}}\"\n"),
        )],
    );
    let r = lint(&allowed);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(
        r.out
            .contains("note — allowance for infra/cluster/manifests/boss-jobs-internal.yaml"),
        "{}",
        r.out
    );
    // The lint carries neither address itself: both are read off the
    // source, so a moved address needs no edit here.
    let text = read(LINT);
    for ip in [
        sor.trim_start_matches("http://").split(':').next().unwrap(),
        &source_key("forge_host"),
    ] {
        assert!(
            !text
                .lines()
                .any(|l| !l.trim_start().starts_with('#') && l.contains(ip)),
            "{LINT} spells {ip} in code"
        );
    }
    // And a source with no address at all is a refusal, not a pass.
    let hollow = scratch_dir("estate-lint-hollow");
    let q = |args: &[&str]| {
        let o = Command::new("git")
            .args(args)
            .current_dir(&hollow)
            .output()
            .expect("git");
        assert!(o.status.success());
    };
    q(&["init", "-q"]);
    create_dir(&hollow.join("infra/estate"));
    write_file(&hollow.join(SOURCE), "# nothing\n");
    write_exec(&hollow.join("infra/x.sh"), "echo\n");
    q(&["add", "-A"]);
    q(&[
        "-c",
        "user.email=t@example",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "x",
    ]);
    let r = lint(&hollow);
    assert_eq!(r.code, 1, "{}{}", r.out, r.err);
    assert!(r.err.contains("does not spell both"), "{}", r.err);
}

// ---------------------------------------------------------------------
// the installer lints judge the file's address, not the environment's
// ---------------------------------------------------------------------

const INSTALLER_LINTS: [&str; 2] = [
    "infra/lint/boss-gcp-converges-itself.sh",
    "infra/lint/forge-install-covers-the-ops-runner.sh",
];
/// What the conductor pod's environment carries when it runs the
/// consist check: the cluster-internal service, which is not the
/// address estate.toml renders and which no lint may take as its own.
const FOREIGN: &str = "http://boss-jobs-internal.boss.svc.cluster.local:7900";

fn installer_lint(root: &Path, rel: &str, env: &[(&str, &str)]) -> Run {
    bash(&[root.join(rel).to_str().unwrap()], env, &[], Some(root))
}

#[test]
fn the_installer_lints_pass_with_a_foreign_address_in_the_environment() {
    for rel in INSTALLER_LINTS {
        let r = installer_lint(
            &repo_root(),
            rel,
            &[("BOSS_JOBS_URL", FOREIGN), ("JOBS_API", FOREIGN)],
        );
        assert_eq!(
            r.code, 0,
            "{rel} is red under the conductor's environment (BOSS_JOBS_URL={FOREIGN}):\n{}{}",
            r.out, r.err
        );
    }
}

/// A copy of `infra/` whose ops-runner installer reports an address
/// the rendered file does not carry — the divergence the lints exist
/// to catch. Only `infra/` is copied: both lints resolve the tree from
/// their own location and read nothing outside it.
///
/// The copy keeps content, modes and links but NOT owners: the test
/// needs the scripts executable, not owned by whoever checked out the
/// tree. A plain `-a` tries to preserve ownership, and on the dev pod
/// (uid 0 without `CAP_CHOWN`, files owned `0:1500`) that is refused,
/// `cp` exits non-zero, and this one fixture redded the whole
/// `boss-testing --all-features` suite for every builder there while the
/// gate, as uid 65534 in a workspace it owns, stayed green (backlog
/// d6859c00, 2026-09-18). Running this test as root on the pod IS the
/// pin.
fn infra_copy_reporting(name: &str, reported: &str) -> PathBuf {
    let root = scratch_dir(name);
    let o = Command::new("cp")
        .args([
            "-a",
            "--no-preserve=ownership",
            repo_root().join("infra").to_str().unwrap(),
        ])
        .arg(&root)
        .output()
        .expect("cp");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let installer = root.join("infra/ops/install-ops-runner.sh");
    let body = std::fs::read_to_string(&installer).unwrap();
    let needle = "reporting to $JOBS_URL";
    assert!(
        body.contains(needle),
        "{} no longer prints `{needle}` — this fixture must mutate the line the lints read",
        installer.display()
    );
    write_exec(
        &installer,
        &body.replace(needle, &format!("reporting to {reported}")),
    );
    root
}

#[test]
fn the_installer_lints_are_red_when_the_installer_reports_another_address() {
    let root = infra_copy_reporting("estate-installer-lints-diverge", FOREIGN);
    for rel in INSTALLER_LINTS {
        let r = installer_lint(&root, rel, &[]);
        assert_eq!(
            r.code, 1,
            "{rel} passed although the installer reported {FOREIGN}:\n{}{}",
            r.out, r.err
        );
        assert!(
            r.err.contains("did not report the address it checked"),
            "{rel} is red for another reason:\n{}",
            r.err
        );
    }
}
