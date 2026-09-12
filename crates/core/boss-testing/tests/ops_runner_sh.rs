//! `infra/ops/ops-runner.sh` is RUN, not read — against a stubbed
//! system of record (a `curl` on PATH that serves one open packet on
//! GET and records the completion on PUT), so every verdict below is
//! one the runner actually made.
//!
//! THE DEFECT (backlog 6964f9e8, measured 2026-09-08). The forge runner
//! refused ops-request 6d0c4e37 — `publish-github-pr --check`, the
//! verb's own no-network input check — with `verb publish-github-pr
//! takes at most 0 arg(s), got 1`. Two gaps: the allowlist had no way
//! to admit a bounded literal, so the only way to exercise the verb
//! was the real run; and the reason lived only in the forge journal —
//! the packet's step said `refused` and nothing else (CLAUDE.md
//! §Diagnosis: a verdict must name what failed).
//!
//! So: a param may be a `one_of` literal list (equality, never a
//! packet-supplied word in the argv), `optional` drops the placeholder
//! when the arg is absent, and every refusal writes `reason` on the
//! step — the same text the runner logs.
//!
//! The runner is sh + jq. A box without jq skips these with a line
//! saying so; the gate image has it.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// A scratch directory per case, so cases cannot see each other's
/// fixtures.
fn scratch(case: &str) -> PathBuf {
    // Per-uid and per-process, and it REFUSES by name if a
    // leftover cannot be cleared — see `boss_testing::scratch`.
    boss_testing::scratch_dir(&format!("ops-runner-sh-{case}"))
}

fn write_exec(path: &Path, body: &str) {
    boss_testing::write_exec(path, body);
}

/// The stubbed system of record: `bin/curl` serves `jobs.json` on any
/// GET and copies a PUT's `--data-binary @file` payload to `put.json`.
/// A `gh` stub stands in for the publish verb's `--check` tool probe.
fn stub_sor(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(
        &bin.join("curl"),
        "#!/bin/sh\n\
         for a in \"$@\"; do case \"$a\" in @*) cp \"${a#@}\" \"$STUB_PUT\"; exit 0;; esac; done\n\
         cat \"$STUB_JOBS\"\n",
    );
    write_exec(&bin.join("gh"), "#!/bin/sh\nexit 0\n");
    bin
}

/// One open ops-request for the named host, `execute` ready, carrying
/// the given verb and args. The runner acts only on an exact
/// `metadata.host` match, so this is also what decides whether a run
/// sees the packet at all.
fn packet_for(root: &Path, host: &str, verb: &str, args: &str) {
    std::fs::write(
        root.join("jobs.json"),
        format!(
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"{host}","verb":"{verb}","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}]}}"#
        ),
    )
    .unwrap();
}

/// One open ops-request for the forge, `execute` ready, carrying the
/// given verb and args.
fn packet(root: &Path, verb: &str, args: &str) {
    packet_for(root, "forge", verb, args);
}

/// Run the runner once against the stub, with `verbs` as its allowlist
/// DIRECTORY (one file per verb — the shape the shipped
/// `infra/ops/verbs/` has). Returns (stdout+stderr, the PUT payload's
/// step metadata if a step was completed).
fn run(
    root: &Path,
    verbs: &Path,
    extra_env: &[(&str, String)],
) -> (String, Option<serde_json::Value>) {
    let put = root.join("put.json");
    let _ = std::fs::remove_file(&put);
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("sh");
    cmd.arg(repo_root().join("infra/ops/ops-runner.sh"))
        .env_clear()
        .env("PATH", path)
        .env("HOST_ID", "forge")
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        .env("OPS_VERBS_DIR", verbs)
        .env("STUB_JOBS", root.join("jobs.json"))
        .env("STUB_PUT", &put);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("ops-runner.sh runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let payload = std::fs::read_to_string(&put)
        .ok()
        .map(|s| serde_json::from_str::<serde_json::Value>(&s).expect("PUT payload is JSON"))
        .map(|v| v["metadata"].clone());
    (text, payload)
}

/// The real allowlist, verbatim: `infra/ops/verbs/*.json` copied file
/// by file. Its script paths are repo-relative and the runner resolves
/// them against its own checkout, so no rewriting is needed here
/// (66077f9c — this harness used to carry one of the four copies of
/// the `/home/david/boss/` substitution).
fn real_verbs(root: &Path) -> PathBuf {
    let dir = root.join("verbs");
    std::fs::create_dir_all(&dir).unwrap();
    for f in shipped_verb_files() {
        std::fs::copy(&f, dir.join(f.file_name().unwrap())).unwrap();
    }
    dir
}

/// A fixture allowlist: one file per (name, spec) under `verbs/`.
fn verbs_dir(root: &Path, specs: &[(&str, &str)]) -> PathBuf {
    let dir = root.join("verbs");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, spec) in specs {
        std::fs::write(dir.join(format!("{name}.json")), spec).unwrap();
    }
    dir
}

/// Every `*.json` under the shipped `infra/ops/verbs/`, sorted — the
/// directory IS the allowlist, so this is the definition every reader
/// is measured against.
fn shipped_verb_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(repo_root().join("infra/ops/verbs"))
        .expect("infra/ops/verbs/ exists")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
}

/// What `publish-github-pr.sh --check` needs to say ok without a
/// network: a state dir, a bare repo standing in for the forge
/// checkout, and a 0600 token file (the value is never printed).
///
/// The stand-in carries a `main` commit, because since 2026-09-11
/// `--check` FETCHES `refs/heads/main` rather than only reading the
/// directory — a cheaper check passed while the publish failed.
fn publish_check_env(root: &Path) -> Vec<(&'static str, String)> {
    use std::os::unix::fs::PermissionsExt;
    let state = root.join("state");
    let etc = root.join("etc");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&etc).unwrap();
    let forge = root.join("forge.git");
    let st = Command::new("git")
        .args(["init", "-q", "--bare", forge.to_str().unwrap()])
        .status()
        .expect("git runs");
    assert!(st.success());
    for args in [
        vec!["hash-object", "-t", "tree", "-w", "--stdin"],
        vec![
            "commit-tree",
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            "-m",
            "seed",
        ],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(&forge)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if args[0] == "commit-tree" {
            let st = Command::new("git")
                .arg("-C")
                .arg(&forge)
                .args(["update-ref", "refs/heads/main", &sha])
                .status()
                .expect("git runs");
            assert!(st.success(), "update-ref refs/heads/main");
        }
    }
    let token = etc.join("github.token");
    std::fs::write(&token, "not-a-real-token\n").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    vec![
        ("BOSS_PUBLISH_STATE_DIR", state.display().to_string()),
        ("BOSS_FORGE_REPO_PATH", forge.display().to_string()),
        ("BOSS_GITHUB_TOKEN_FILE", token.display().to_string()),
    ]
}

macro_rules! needs_jq {
    () => {
        if !has("jq") {
            eprintln!("ops_runner_sh: SKIPPED — no jq on this box; the gate image has it");
            return;
        }
    };
}

/// A verb's script is named RELATIVE to the repo and resolved against
/// the runner's OWN checkout (backlog 66077f9c). Eleven of sixteen verbs
/// baked `/home/david/boss/infra/forge/…` — the forge checkout's path —
/// into argv[0], so none could run on boss-gcp (/opt/boss) and every
/// consumer (two lints, this harness) carried its own substitution of
/// that prefix: one path assumption in four places. The runner knows
/// where it is; a relative argv[0] resolves against that, an absent
/// script is a REFUSAL naming the resolved path, and a bare command is
/// left to PATH as before.
#[test]
fn a_relative_argv0_resolves_against_the_runners_own_checkout() {
    needs_jq!();
    let root = scratch("relative-argv0");
    stub_sor(&root);
    let verbs = verbs_dir(
        &root,
        &[
            (
                "probe",
                r#"{"about": "a tree lint, as a probe of resolution", "hosts": ["forge"],
                    "argv": ["infra/lint/no-manifest-mounts-a-hostpath.sh"], "params": []}"#,
            ),
            (
                "gone",
                r#"{"about": "a script this checkout does not carry", "hosts": ["forge"],
                    "argv": ["infra/ops/does-not-exist.sh"], "params": []}"#,
            ),
        ],
    );
    packet(&root, "probe", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert!(
        md["output"]
            .as_str()
            .unwrap_or("")
            .contains("no-manifest-mounts-a-hostpath: ok"),
        "the relative script ran from this checkout: {md} / {out}"
    );

    packet(&root, "gone", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md} / {out}");
    let reason = md["reason"].as_str().unwrap_or("");
    assert!(
        reason.contains("infra/ops/does-not-exist.sh") && reason.contains("not in this checkout"),
        "the refusal names the resolved path: {md} / {out}"
    );

    // The shipped allowlist carries NO absolute checkout path any more.
    for f in shipped_verb_files() {
        let shipped = std::fs::read_to_string(&f).unwrap();
        assert!(
            !shipped.contains("/home/david/boss/"),
            "{} names a script by one host's checkout, not relative to the repo",
            f.display()
        );
    }
}

/// The allowed literal reaches the verb: `publish-github-pr --check` is
/// ANSWERED through the runner, and the verb's own check ran and said
/// ok. Before 6964f9e8 this exact packet was refused.
#[test]
fn the_allowed_literal_is_answered_through_the_runner() {
    needs_jq!();
    let root = scratch("literal-answered");
    stub_sor(&root);
    let verbs = real_verbs(&root);
    let env = publish_check_env(&root);
    packet(&root, "publish-github-pr", r#"["--check"]"#);
    let (out, payload) = run(&root, &verbs, &env);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(md["exit_code"], "0", "{md} / {out}");
    assert!(
        md["output"].as_str().unwrap_or("").contains("--check ok"),
        "{md} / {out}"
    );
    assert!(out.contains("answered publish-github-pr"), "{out}");
}

/// A word outside the literal list is refused, and the refusal names
/// itself on the step: `reason` is present, equals `output`, names the
/// admitted literals, and is the SAME text the runner logs.
#[test]
fn a_word_outside_the_literal_list_is_refused_with_the_reason_on_the_step() {
    needs_jq!();
    let root = scratch("literal-refused");
    stub_sor(&root);
    let verbs = real_verbs(&root);
    packet(&root, "publish-github-pr", r#"["--force"]"#);
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md}");
    let reason = md["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("no reason on the step: {md}"));
    assert!(
        reason.contains("arg mode value --force is not one of --check"),
        "{reason}"
    );
    assert_eq!(md["reason"], md["output"], "reason and output differ: {md}");
    assert!(md.get("exit_code").is_none(), "a refusal ran nothing: {md}");
    assert!(
        out.contains(&format!("refused aaaaaaaa — {reason}")),
        "the journal line must carry the same reason: {out}"
    );
}

/// Every refusal path writes through the one site: an unknown verb
/// and an over-long arg list each land their reason on the step too.
#[test]
fn every_refusal_path_names_its_reason_on_the_step() {
    needs_jq!();
    let root = scratch("refusal-paths");
    stub_sor(&root);
    let verbs = real_verbs(&root);

    packet(&root, "rm-rf", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md}");
    let reason = md["reason"].as_str().unwrap();
    assert!(
        reason.contains("verb rm-rf is not in the allowlist"),
        "{reason}"
    );
    assert!(
        out.contains(&format!("refused aaaaaaaa — {reason}")),
        "{out}"
    );

    packet(&root, "uptime", r#"["now"]"#);
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md}");
    let reason = md["reason"].as_str().unwrap();
    assert!(
        reason.contains("verb uptime takes at most 0 arg(s), got 1"),
        "{reason}"
    );
    assert!(
        out.contains(&format!("refused aaaaaaaa — {reason}")),
        "{out}"
    );
}

/// An optional literal that is absent DROPS its placeholder word: the
/// verb runs with no trailing empty argument (which `echo` would show
/// as a trailing space), and present it rides through verbatim.
#[test]
fn an_absent_optional_literal_drops_its_placeholder_word() {
    needs_jq!();
    let root = scratch("optional-omitted");
    stub_sor(&root);
    let verbs = verbs_dir(
        &root,
        &[(
            "say",
            r#"{"about":"echo","hosts":["forge"],"argv":["echo","ran","{1}"],"params":[{"name":"mode","one_of":["--check"],"optional":true}]}"#,
        )],
    );

    packet(&root, "say", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(
        md["output"], "ran\n",
        "an omitted optional must not leave an empty word: {md}"
    );

    packet(&root, "say", r#"["--check"]"#);
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["output"], "ran --check\n", "{md}");
}

/// A VERB SERVES NAMED HOSTS. boss-gcp got a runner on 2026-09-11, and
/// 11 of the allowlist's 16 verbs name a script under the FORGE's
/// checkout (`/home/david/boss/infra/forge/...`), which does not exist
/// there. Unscoped, standing that runner up advertised a vocabulary of
/// which 11 could only fail on ENOENT — and an exec failure is not a
/// verdict (CLAUDE.md §Diagnosis). So a verb whose `hosts` does not
/// list this runner's HOST_ID is REFUSED, and the refusal names the
/// verb, this host, the hosts that verb does serve, and what this host
/// can be asked for instead.
#[test]
fn a_verb_that_does_not_serve_this_host_is_refused_by_name() {
    needs_jq!();
    let root = scratch("host-not-served");
    stub_sor(&root);
    let verbs = real_verbs(&root);
    packet_for(&root, "boss-gcp", "converge", "[]");
    let (out, payload) = run(&root, &verbs, &[("HOST_ID", "boss-gcp".to_string())]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md} / {out}");
    let reason = md["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("no reason on the step: {md}"));
    assert!(
        reason.contains("verb converge does not serve host boss-gcp"),
        "the refusal must name the verb and this host: {reason}"
    );
    assert!(
        reason.contains("scopes it to forge"),
        "the refusal must name the hosts the verb DOES serve: {reason}"
    );
    assert!(
        reason.contains("verbs this host serves: ") && reason.contains("df"),
        "the refusal must say what this host can be asked for instead: {reason}"
    );
    assert_eq!(md["reason"], md["output"], "reason and output differ: {md}");
    assert!(md.get("exit_code").is_none(), "a refusal ran nothing: {md}");
    assert!(
        out.contains(&format!("refused aaaaaaaa — {reason}")),
        "the journal line must carry the same reason: {out}"
    );
}

/// And the five read-only, host-agnostic reads DO serve boss-gcp: the
/// bastion answers `df` through the same runner, with `runner_host`
/// recording which host answered.
#[test]
fn a_read_only_verb_answers_on_boss_gcp() {
    needs_jq!();
    let root = scratch("host-served");
    stub_sor(&root);
    let verbs = real_verbs(&root);
    packet_for(&root, "boss-gcp", "df", "[]");
    let (out, payload) = run(&root, &verbs, &[("HOST_ID", "boss-gcp".to_string())]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(md["exit_code"], "0", "{md} / {out}");
    assert_eq!(md["runner_host"], "boss-gcp", "{md}");
    assert!(out.contains("answered df"), "{out}");
}

/// ABSENT MEANS REFUSE, so a verb cannot reach a host by forgetting to
/// say which hosts it serves — the fail-closed half, without which the
/// scoping would be advice rather than a rule.
#[test]
fn a_verb_declaring_no_hosts_is_refused_everywhere() {
    needs_jq!();
    let root = scratch("hosts-absent");
    stub_sor(&root);
    let verbs = verbs_dir(
        &root,
        &[(
            "say",
            r#"{"about":"echo","argv":["echo","ran"],"params":[]}"#,
        )],
    );

    packet(&root, "say", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(
        md["disposition"], "refused",
        "a verb with no hosts must be refused, not run: {md} / {out}"
    );
    let reason = md["reason"].as_str().unwrap();
    assert!(
        reason.contains("no host") && reason.contains("refused everywhere"),
        "the refusal must say the allowlist entry declares no hosts: {reason}"
    );
}

/// THE ALLOWLIST IS A DIRECTORY, one file per verb, and the verb's
/// name is its file name (backlog 5086842d). `infra/ops/verbs.json`
/// was one JSON object, and a JSON object has no uncontended insertion
/// point: on 2026-09-12 three cars each added a verb, two inserted
/// before the same key, and the conductor left one behind
/// (`conflict: infra/ops/verbs.json`) — the shape CLAUDE.md §9a records
/// for rules.toml before rules became one file each. Now adding a verb
/// is dropping a file in, touching no shared line.
///
/// The runner is the reader that matters most: it runs on two hosts
/// from their converged checkouts, and if it cannot load the directory
/// every ops verb dies. So this RUNS it over a directory and asks that
/// a verb be reachable by its file name, and that an unknown verb's
/// refusal lists every file — which is the runner saying, on the
/// packet, that it loaded them all.
#[test]
fn the_allowlist_is_the_directory_and_a_verb_is_named_by_its_file() {
    needs_jq!();
    let root = scratch("directory-allowlist");
    stub_sor(&root);
    let verbs = verbs_dir(
        &root,
        &[
            (
                "say-hello",
                r#"{"about":"echo","hosts":["forge"],"argv":["echo","hello"],"params":[]}"#,
            ),
            (
                "say-bye",
                r#"{"about":"echo","hosts":["forge"],"argv":["echo","bye"],"params":[]}"#,
            ),
        ],
    );
    // A README beside the verbs is prose, not a verb: the loader must
    // take only `*.json`.
    std::fs::write(verbs.join("README.md"), "# not a verb\n").unwrap();

    packet(&root, "say-bye", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(md["output"], "bye\n", "{md}");

    packet(&root, "rm-rf", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md} / {out}");
    let reason = md["reason"].as_str().unwrap();
    assert!(
        reason.contains("infra/ops/verbs/") && reason.ends_with("verbs: say-bye, say-hello"),
        "the refusal names the directory and every verb file in it: {reason}"
    );
}

/// The SHIPPED directory loads whole: the runner's own listing of what
/// it knows equals the file names under `infra/ops/verbs/`. This is the
/// equality that makes the directory the definition — a verb file the
/// runner silently skipped would show up here as a name missing from
/// the refusal.
#[test]
fn the_runner_loads_every_shipped_verb_file() {
    needs_jq!();
    let root = scratch("shipped-directory");
    stub_sor(&root);
    let verbs = real_verbs(&root);
    let expected: Vec<String> = shipped_verb_files()
        .iter()
        .map(|f| f.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(expected.len() >= 16, "{expected:?}");
    assert!(
        !repo_root().join("infra/ops/verbs.json").exists(),
        "infra/ops/verbs.json is back — the directory is the allowlist now (5086842d); \
         a verb goes in infra/ops/verbs/<name>.json"
    );

    packet(&root, "rm-rf", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    let reason = md["reason"].as_str().unwrap();
    let listed = reason
        .rsplit("verbs: ")
        .next()
        .unwrap()
        .split(", ")
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        listed, expected,
        "the runner's allowlist is not the directory: {reason}"
    );
}

/// A directory the runner cannot load is a REFUSAL TO RUN naming the
/// directory (EX_CONFIG, the same exit an unreadable allowlist got),
/// never an empty allowlist that refuses every packet as unknown: no
/// directory, an empty one, and a file that is not a JSON object each
/// stop the runner before it touches a packet, and each says which.
#[test]
fn a_directory_the_runner_cannot_load_stops_it_by_name() {
    needs_jq!();
    let root = scratch("directory-broken");
    stub_sor(&root);
    packet(&root, "uptime", "[]");

    let missing = root.join("no-such-dir");
    let (out, payload) = run(&root, &missing, &[]);
    assert!(
        payload.is_none(),
        "a runner with no allowlist touched a packet: {out}"
    );
    assert!(
        out.contains("no-such-dir") && out.contains("refusing to run"),
        "{out}"
    );

    let empty = root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let (out, payload) = run(&root, &empty, &[]);
    assert!(payload.is_none(), "{out}");
    assert!(
        out.contains("holds no verb") && out.contains("empty"),
        "an empty directory must be named as the fault, not treated as an allowlist: {out}"
    );

    let broken = verbs_dir(
        &root,
        &[
            (
                "ok",
                r#"{"about":"echo","hosts":["forge"],"argv":["echo","ok"],"params":[]}"#,
            ),
            ("bad", "{not json"),
        ],
    );
    let (out, payload) = run(&root, &broken, &[]);
    assert!(payload.is_none(), "{out}");
    assert!(
        out.contains("bad.json"),
        "the fault must name the file that would not parse: {out}"
    );
}
