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
/// GET (recording the URL it was asked for in `get.url`, so a case can
/// read the query the runner sent), copies a PUT's `--data-binary
/// @file` payload to `put.json`, and a PATCH's to `patch.json` — the
/// request-level `exit` the runner writes through the job metadata door
/// (f47861a5). A `gh` stub stands in for the publish verb's `--check`
/// tool probe. Every PATCH is also appended to `STUB_PATCH_LOG` when
/// set, because one pass may write two (the queue reading, then a
/// refused completion). A PUT answers `STUB_PUT_CODE` (default 200) on
/// `-w` and writes `STUB_PUT_BODY` to its `-o` file — the server's
/// refusal, which is what a refused completion must carry.
fn stub_sor(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(
        &bin.join("curl"),
        "#!/bin/sh\n\
         m=GET; prev=; o=; w=\n\
         for a in \"$@\"; do [ \"$prev\" = -X ] && m=\"$a\"; [ \"$prev\" = -o ] && o=\"$a\"; [ \"$prev\" = -w ] && w=1; prev=\"$a\"; done\n\
         for a in \"$@\"; do case \"$a\" in @*)\n\
             if [ \"$m\" = PATCH ]; then cp \"${a#@}\" \"$STUB_PATCH\"\n\
                 if [ -n \"${STUB_PATCH_LOG:-}\" ]; then cat \"${a#@}\" >> \"$STUB_PATCH_LOG\"; echo >> \"$STUB_PATCH_LOG\"; fi\n\
             else cp \"${a#@}\" \"$STUB_PUT\"\n\
                 if [ -n \"$o\" ]; then printf '%s' \"${STUB_PUT_BODY:-}\" > \"$o\"; fi\n\
                 if [ -n \"$w\" ]; then printf '%s' \"${STUB_PUT_CODE:-200}\"; fi\n\
             fi\n\
             exit 0;; esac; done\n\
         for a in \"$@\"; do case \"$a\" in http*) printf '%s\\n' \"$a\" > \"$STUB_GET\";; esac; done\n\
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
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"{host}","verb":"{verb}","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}],"total":1}}"#
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
    let _ = std::fs::remove_file(root.join("patch.json"));
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
        .env("STUB_PUT", &put)
        .env("STUB_PATCH", root.join("patch.json"))
        .env("STUB_GET", root.join("get.url"));
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
    // The forge's address file (/etc/boss/sor.env on the host), rendered
    // from the one source: the verb derives its forge clone URL from it.
    let sor_env = etc.join("sor.env");
    let rendered = Command::new("bash")
        .arg(repo_root().join("infra/estate/render-sor-env.sh"))
        .arg("--to")
        .arg(&sor_env)
        .output()
        .expect("render sor.env");
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    vec![
        ("BOSS_PUBLISH_STATE_DIR", state.display().to_string()),
        ("BOSS_FORGE_REPO_PATH", forge.display().to_string()),
        ("BOSS_GITHUB_TOKEN_FILE", token.display().to_string()),
        ("BOSS_SOR_ENV", sor_env.display().to_string()),
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

/// AN ARG IS ONE WORD, OR IT IS REFUSED (security review of fd7090cc,
/// 2026-09-24). jq's `test` runs Oniguruma in Perl syntax, where `$`
/// matches before a TRAILING NEWLINE — measured on jq 1.6:
/// `"target-a\n" | test("^[a-z-]{1,20}$")` is `true`. The argv is then
/// rebuilt from jq's output one line per word, so that newline became a
/// SECOND, empty word, and every placeholder after it moved one place —
/// on an approval verb, the place the signed plan's hash rides. So a
/// control character anywhere in an arg is refused by name before any
/// pattern is consulted, and every pattern must match the WHOLE string.
#[test]
fn an_arg_carrying_a_newline_or_a_control_character_is_refused() {
    needs_jq!();
    let root = scratch("control-chars");
    stub_sor(&root);
    let verbs = verbs_dir(
        &root,
        &[(
            "echo-word",
            r#"{"about":"test","hosts":["forge"],"argv":["echo","{1}","{2}"],
                "params":[{"name":"word","pattern":"^[a-z-]{1,20}$"},
                          {"name":"tail","pattern":"^[a-z]{1,5}$","optional":true}]}"#,
        )],
    );
    for (args, shown) in [
        (r#"["target-a\n"]"#, "U+000A"),
        (r#"["target-a\n","x"]"#, "U+000A"),
        (r#"["targ\u0001et"]"#, "U+0001"),
        (r#"["target-a\r"]"#, "U+000D"),
        (r#"["target-a\u007f"]"#, "U+007F"),
    ] {
        packet(&root, "echo-word", args);
        let (out, payload) = run(&root, &verbs, &[]);
        let md = payload.unwrap_or_else(|| panic!("{args}: no step completed: {out}"));
        assert_eq!(md["disposition"], "refused", "{args}: {md} / {out}");
        let reason = md["reason"].as_str().unwrap_or_default();
        assert!(
            reason.contains("control character") && reason.contains("word"),
            "{args}: the refusal names the arg and why: {reason}"
        );
        assert!(
            reason.contains(shown),
            "{args}: and names the character by code point, not raw: {reason}"
        );
        assert!(
            !reason.chars().any(|c| c == '\n' || c == '\u{1}'),
            "{args}: the reason itself carries no control character: {reason:?}"
        );
    }
    // And the clean word still runs.
    packet(&root, "echo-word", r#"["target-a"]"#);
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(md["output"], "target-a\n", "{md}");
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

/// A VERB THAT IS THE TREE'S OWN CLI SIGNS AS THE RUNNER. `run-car-probe`
/// runs `boss prove … --unattended` since backlog 9f00a805 (car 2): the
/// CLI signs every jobs-API call as `BOSS_ACTOR` and refuses a write
/// unnamed, so the runner hands its own account over in the verb's
/// environment — the same identity the step completion carries — and a
/// unit that set `BOSS_ACTOR` itself wins. Read back through a bare
/// command on PATH, the way `boss` resolves on the forge.
#[test]
fn a_cli_verb_signs_as_the_runners_own_account() {
    needs_jq!();
    let root = scratch("cli-verb-actor");
    let bin = stub_sor(&root);
    write_exec(
        &bin.join("who-signs"),
        "#!/bin/sh\nprintf 'signs-as=%s packet=%s\\n' \"${BOSS_ACTOR:-unset}\" \"${OPS_REQUEST_ID:-unset}\"\n",
    );
    let verbs = verbs_dir(
        &root,
        &[(
            "who-signs",
            r#"{"about": "prints the actor a CLI verb would sign as", "hosts": ["forge"],
                "argv": ["who-signs"], "params": []}"#,
        )],
    );
    packet(&root, "who-signs", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    let output = md["output"].as_str().unwrap_or("");
    assert!(
        output.contains("signs-as=automation:ops-runner"),
        "the verb must see the runner's own account as BOSS_ACTOR: {md} / {out}"
    );
    assert!(
        output.contains("packet=aaaaaaaa-0000-4000-8000-000000000000"),
        "the packet id still rides the environment: {md}"
    );

    // The runner's account is BOSS_OPS_ACTOR when a unit names one…
    let (out, payload) = run(
        &root,
        &verbs,
        &[("BOSS_OPS_ACTOR", "automation:forge-ops".into())],
    );
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert!(
        md["output"]
            .as_str()
            .unwrap_or("")
            .contains("signs-as=automation:forge-ops"),
        "{md}"
    );
    // …and an explicit BOSS_ACTOR on the unit outranks both.
    let (out, payload) = run(&root, &verbs, &[("BOSS_ACTOR", "emp-operator".into())]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert!(
        md["output"]
            .as_str()
            .unwrap_or("")
            .contains("signs-as=emp-operator"),
        "{md}"
    );

    // And the shipped verb itself is the CLI, not a script: a bare
    // `boss` on PATH, with the car first and both flags fixed words.
    let shipped: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/run-car-probe.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        shipped["argv"],
        serde_json::json!(["boss", "prove", "{1}", "--from-car", "--unattended"]),
        "run-car-probe's argv is the tree's CLI (9f00a805 car 2)"
    );
    assert!(
        !repo_root().join("infra/forge/run-car-probe.sh").exists(),
        "the shell twin was retired with 9f00a805 car 2; a script here is a second definition"
    );
}

/// THE VERB'S EXIT IS RECORDED ONCE, ON THE STEP (backlog 50fede8b).
/// It used to be recorded twice under two names — `exit_code` on the
/// execute step and `exit` on the request — written by one act and
/// held equal by nothing, which is the fact-that-lives-twice shape
/// CLAUDE.md §9a refuses. It was benign only while one writer wrote
/// both; a retry, a hand correction or a second runner writing one
/// without the other hands a reader a stale exit and a verdict on a
/// run that did not have it. Measured 2026-09-20 before the collapse:
/// every consumer already read the STEP — `boss ops --wait`'s verdict
/// line, `verb_failure` for the whole answered-ops-request judge
/// family, the yard's runner shed and signals — and no rule predicate,
/// handler or surface read the request-level copy. The request level
/// needs no copy to be readable, either: `GET /api/jobs?kind=ops-request`
/// returns each row WITH its steps, which is how the yard reads the
/// exit off `execute` from a list.
///
/// f47861a5's finding stands and is served by the step: a verb that
/// ran and failed must be visible above `answered` (publish-github-pr
/// printed `FAILED`, exited 1, and the publish step it was filed for
/// sat ready for five hours). What reads it is the
/// `complete-publish-pr-step-on-publish-github-pr-answered` rule, off
/// `exit_code`. The outcome stays `answered`: every judge rule keys on
/// it, and a verb that ran and said no is an answer, not a refusal.
#[test]
fn an_answered_verbs_exit_is_recorded_once_on_its_step() {
    needs_jq!();
    let root = scratch("exit-on-request");
    stub_sor(&root);
    let fails = root.join("fails.sh");
    write_exec(
        &fails,
        "#!/bin/sh\necho 'fails: step one ok'\necho 'fails: FAILED — the thing did not happen' >&2\nexit 3\n",
    );
    let verbs = verbs_dir(
        &root,
        &[(
            "fails",
            &format!(
                r#"{{"about":"a verb that fails","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                fails.display()
            ),
        )],
    );
    packet(&root, "fails", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    assert_eq!(md["exit_code"], "3", "{md} / {out}");
    let patch: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("patch.json"))
            .unwrap_or_else(|e| panic!("no PATCH on the request's metadata: {e}; {out}")),
    )
    .expect("the PATCH body is JSON");
    assert_eq!(
        // What is left on this door is the queue reading (1ffb3305) —
        // a request-level fact with no home on the step, unlike the
        // exit. This fixture's packet carries no `opened_at`, so it
        // has no `queued_s`. Nothing ELSE reaches the request's
        // metadata, and in particular no second spelling of the exit.
        patch,
        serde_json::json!({"queue_depth": 1}),
        "the request carries the queue it waited in and nothing else — the exit lives once, on the step: {patch}"
    );
    assert!(out.contains("answered fails"), "{out}");

    // A refusal ran nothing, so there is nothing to record about it
    // on the request at all.
    packet(&root, "not-a-verb", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md} / {out}");
    assert!(
        !root.join("patch.json").exists(),
        "a refusal writes nothing on the request: {out}"
    );
}

/// HOW LONG THE VERB TOOK, RECORDED WHERE ITS EXIT IS (backlog
/// b7bfe821). An answered request said THAT the verb exited, with what
/// code, and what it printed — and nothing about how long it ran. The
/// only duration derivable from the record was `opened_at` to
/// `closed_at`, which is dominated by up to 60 s of this runner's poll
/// latency and so cannot see a verb at all: the builder costing the
/// run-car-probe cadence on 2026-09-19 had to infer per-probe cost
/// from the SPREAD WITHIN a simultaneous batch — five packets filed at
/// 16:10:55 closing across 0.5 s — which is exactly the re-derivation
/// CLAUDE.md §Diagnosis refuses. The runner holds the number at the
/// moment it writes the exit and was dropping it, and that matters
/// most now: run-car-probe was 171 of the last 300 ops-requests and
/// its cadence went daily to hourly the same day, so the verb about to
/// dominate this serial walk had no per-run duration series to notice
/// a change in.
///
/// It rides the execute step, beside `exit_code` and `output`, because
/// it is a fact about the VERB'S RUN and not about the queue in front
/// of it — which is why `queue_depth` / `queued_s` ride the request
/// instead. One spelling, one writer, the 50fede8b rule above. A list
/// read returns each row with its steps, so the series this exists for
/// is one jobs-API query.
///
/// A refusal ran nothing, so it records no duration, the same way it
/// records no exit.
#[test]
fn an_answered_verbs_duration_is_recorded_on_its_step() {
    needs_jq!();
    let root = scratch("duration-on-step");
    stub_sor(&root);
    let slow = root.join("slow.sh");
    write_exec(&slow, "#!/bin/sh\nsleep 0.4\necho 'slow: done'\n");
    let verbs = verbs_dir(
        &root,
        &[(
            "slow",
            &format!(
                r#"{{"about":"a verb that takes a measurable moment","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                slow.display()
            ),
        )],
    );
    packet(&root, "slow", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md} / {out}");
    let ms = md["duration_ms"]
        .as_u64()
        .unwrap_or_else(|| panic!("no numeric duration_ms on the answered step: {md} / {out}"));
    assert!(
        (300..60_000).contains(&ms),
        "the recorded duration must be the verb's own run — it slept 0.4 s and the step says {ms} ms: {md} / {out}"
    );

    // A refusal ran nothing, so there is no duration to record — the
    // same posture as `exit_code`.
    packet(&root, "not-a-verb", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md} / {out}");
    assert!(
        md.get("duration_ms").is_none(),
        "a refusal ran nothing to time: {md} / {out}"
    );
}

/// A REFUSED COMPLETION SAYS WHY, ON THE REQUEST (post-mortem
/// 3c3b202c). On 2026-09-22 from 00:50 to 02:54 UTC both runners
/// stalled together, the forge's oldest request waiting 7394 s, and
/// nothing in the system of record named a cause. The cause: ops-request
/// v2 gated `execute` on `NOT requires_approval OR approve.done`, and
/// the step PUT's unresolved-blockers guard refused every completion
/// over the pending `approve` with a 409 (fixed in bd0f3369). The runner
/// ran each verb, met the 409, counted it `failed` and moved on, so it
/// re-ran every open request's verb once a minute for two hours. What
/// the record held about that: nothing. `curl -f` threw away the 409's
/// body, which named the blocker, so even the journal carried only the
/// status, and the unit going red was watched by nobody.
///
/// So a completion the server REFUSES (it answered, and not 2xx) is
/// written onto the request through the metadata door, which kept
/// working all night: the status, the server's own words, when, how
/// many times, and whether the verb ran anyway. That last one matters
/// because a refused answer re-runs the verb on the next pass. The
/// packet then says why it is stuck, and a queue alarm (a45b38c1) can
/// quote it rather than send someone to a journal.
/// A VERB WHOSE COMPLETION WAS REFUSED IS NEVER RUN AGAIN BY ITSELF
/// (backlog 865d37df, post-mortem 3c3b202c). The runner runs a verb
/// BEFORE its completion PUT, so a refused PUT left the step ready and
/// the next pass ran the verb again: on 2026-09-22 every open verb re-ran
/// about 120 times in two hours (58 cluster-converge packets in 66
/// minutes from one converge request). Those verbs were reads and
/// converges; ops-request v2 exists for destructive ones. So once the
/// request carries a refusal whose verb RAN, the runner holds it — skips
/// it, names the reason — and re-running is an explicit act: clearing
/// `completion_refused`. At-most-once matters more than completion.
#[test]
fn a_verb_whose_completion_was_refused_runs_exactly_once_across_passes() {
    needs_jq!();
    let root = scratch("completion-refused-held");
    stub_sor(&root);
    let ran = root.join("ran.count");
    let verb = root.join("verb.sh");
    write_exec(
        &verb,
        &format!("#!/bin/sh\necho x >> \"{}\"\necho did it\n", ran.display()),
    );
    let verbs = verbs_dir(
        &root,
        &[(
            "once",
            &format!(
                r#"{{"about":"a verb that must not repeat","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                verb.display()
            ),
        )],
    );
    let log = root.join("patches.jsonl");
    let env = vec![
        ("STUB_PUT_CODE", "409".to_string()),
        (
            "STUB_PUT_BODY",
            r#"{"error":"step has unresolved blockers"}"#.to_string(),
        ),
        ("STUB_PATCH_LOG", log.display().to_string()),
    ];
    let runs = || {
        std::fs::read_to_string(&ran)
            .unwrap_or_default()
            .lines()
            .count()
    };

    // Pass 1: the verb runs, the server refuses its completion, and the
    // refusal (verb_ran: true) is written onto the request.
    packet(&root, "once", "[]");
    let (out, _) = run(&root, &verbs, &env);
    assert_eq!(runs(), 1, "the first pass runs the verb: {out}");
    let written = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v.get("completion_refused").is_some())
        .unwrap_or_else(|| panic!("no refusal written: {out}"));
    assert_eq!(written["completion_refused"]["verb_ran"], true, "{written}");

    // Passes 2 and 3 see the request AS THE RUNNER LEFT IT.
    let job = serde_json::json!({"data":[{
        "id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open",
        "metadata":{"host":"forge","verb":"once","args":[],
                    "completion_refused": written["completion_refused"]},
        "steps":[{"id":"s-execute","spec_slug":"execute","status":"ready",
                  "metadata":{"authority_role":"platform-admin"}}]}],"total":1});
    std::fs::write(root.join("jobs.json"), job.to_string()).unwrap();
    for pass in 2..=3 {
        let (out, payload) = run(&root, &verbs, &env);
        assert_eq!(
            runs(),
            1,
            "pass {pass} re-ran a verb whose completion was refused: {out}"
        );
        assert!(payload.is_none(), "a held request is not completed: {out}");
        assert!(
            out.contains("held after a refused completion")
                && out.contains("step has unresolved blockers"),
            "the hold names itself and the server's reason: {out}"
        );
    }
}

#[test]
fn a_refused_completion_is_written_onto_its_request_with_the_servers_reason() {
    needs_jq!();
    let root = scratch("completion-refused");
    stub_sor(&root);
    let ok = root.join("ok.sh");
    write_exec(&ok, "#!/bin/sh\necho ok\n");
    let verbs = verbs_dir(
        &root,
        &[(
            "ok",
            &format!(
                r#"{{"about":"a verb that answers","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                ok.display()
            ),
        )],
    );
    let refusal = r#"{"error":"step has unresolved blockers","step_id":"s-execute","unresolved_blockers":["s-approve=pending"]}"#;
    let log = root.join("patches.jsonl");
    let env = |log: &Path| {
        vec![
            ("STUB_PUT_CODE", "409".to_string()),
            ("STUB_PUT_BODY", refusal.to_string()),
            ("STUB_PATCH_LOG", log.display().to_string()),
        ]
    };
    let refused_patch = |log: &Path, out: &str| -> serde_json::Value {
        std::fs::read_to_string(log)
            .unwrap_or_else(|e| panic!("no PATCH at all: {e}; {out}"))
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("a PATCH body is JSON"))
            .find(|v| v.get("completion_refused").is_some())
            .unwrap_or_else(|| panic!("the refused completion never reached the request: {out}"))
    };

    packet(&root, "ok", "[]");
    let (out, _) = run(&root, &verbs, &env(&log));
    assert!(
        out.contains("step has unresolved blockers"),
        "the journal line carries the server's reason, not only its status: {out}"
    );
    let r = &refused_patch(&log, &out)["completion_refused"];
    assert_eq!(r["http"], 409, "{r} / {out}");
    assert!(
        r["reason"]
            .as_str()
            .is_some_and(|s| s.contains("s-approve=pending")),
        "the server's own words ride the request: {r}"
    );
    assert_eq!(r["count"], 1, "{r}");
    assert_eq!(r["verb_ran"], true, "the verb ran before the refusal: {r}");
    let first = r["first_at"].as_str().unwrap_or_default().to_string();
    assert!(
        first.ends_with('Z') && r["last_at"] == first.as_str(),
        "a first refusal is stamped once, in UTC: {r}"
    );
    assert!(
        out.contains("failed=1"),
        "a refused completion is still a failed pass, and the unit still goes red: {out}"
    );

    // The next pass meets the same refusal. The request already carries
    // the first one, and the record accumulates rather than resets: how
    // long a request has been jammed is the number a reader wants.
    std::fs::write(
        root.join("jobs.json"),
        r#"{"data":[{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{"host":"forge","verb":"ok","args":[],"completion_refused":{"http":409,"count":4,"first_at":"2026-09-22T00:51:02Z"}},"steps":[{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{"authority_role":"platform-admin"}}]}],"total":1}"#,
    )
    .unwrap();
    let _ = std::fs::remove_file(&log);
    let (out, _) = run(&root, &verbs, &env(&log));
    let r = &refused_patch(&log, &out)["completion_refused"];
    assert_eq!(r["count"], 5, "{r} / {out}");
    assert_eq!(r["first_at"], "2026-09-22T00:51:02Z", "{r}");

    // A completion the server ACCEPTS writes no refusal.
    packet(&root, "ok", "[]");
    let _ = std::fs::remove_file(&log);
    let (out, payload) = run(
        &root,
        &verbs,
        &[("STUB_PATCH_LOG", log.display().to_string())],
    );
    assert!(payload.is_some(), "{out}");
    assert!(
        !std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("completion_refused"),
        "an accepted completion is not a refusal: {out}"
    );
}

/// A QUEUE WHOSE DEPTH NOBODY READS (backlog 1ffb3305). This runner
/// walks up to 100 open requests SERIALLY in one oneshot with no
/// per-verb fairness, so a latency-sensitive verb queues behind
/// whatever is ahead of it in the same run — a converge, the verb that
/// deploys a fix, waits behind however many run-car-probes are in
/// front of it. Measured 2026-09-19: run-car-probe was 171 of the last
/// 300 ops-requests against 42 converges, and its cadence went daily to
/// hourly the same day. Today's volumes are comfortable; the serial
/// walk is now the only real bound on raising any probe cadence
/// further, and nothing measured it. A queue nobody reads is one that
/// gets discovered at its worst moment, by a converge that did not
/// deploy when it should have.
///
/// So the runner reads its own queue before it walks it, and the
/// reading lands in two places on purpose: the journal line carries
/// the gauge for EVERY run, depth zero included — a gauge that appears
/// only when it is non-zero cannot be told apart from a runner that
/// stopped — and each answered request carries what IT waited, so the
/// series is a system-of-record query rather than an ssh. Per-verb
/// fairness or a priority lane is the larger change and waits for this
/// reading to say it is needed.
#[test]
fn the_runner_reads_its_own_queue_depth_and_oldest_wait() {
    needs_jq!();
    let root = scratch("queue-reading");
    stub_sor(&root);
    let ok = root.join("ok.sh");
    write_exec(&ok, "#!/bin/sh\necho ok\n");
    let verbs = verbs_dir(
        &root,
        &[(
            "ok",
            &format!(
                r#"{{"about":"a verb that answers","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                ok.display()
            ),
        )],
    );

    // An empty queue is a reading too, and the one the runner takes
    // most often.
    std::fs::write(root.join("jobs.json"), r#"{"data":[],"total":0}"#).unwrap();
    let (out, _) = run(&root, &verbs, &[]);
    assert_eq!(
        queue_line(&out),
        "ops-runner: queue host=forge depth=0 oldest_wait_s=-",
        "an empty queue still reports its depth: {out}"
    );

    // Three open requests for this host, the oldest filed 600s ago.
    queue(&root, "ok", &[Some(120), Some(600), Some(30)]);
    let (out, _) = run(&root, &verbs, &[]);
    let line = queue_line(&out);
    assert!(line.contains("depth=3"), "{line}");
    let oldest: i64 = field(&line, "oldest_wait_s")
        .parse()
        .unwrap_or_else(|_| panic!("oldest_wait_s is a number: {line}"));
    assert!(
        (600..660).contains(&oldest),
        "the oldest wait is the oldest packet's age, not the newest's: {line}"
    );

    // A packet nobody stamped has NO age, and the reading says so
    // rather than answering zero: `date -d ''` answers midnight, which
    // would report a fresh packet as a decades-old wait (the probe-time
    // trap, crates/core/boss-jobs/src/probe.rs).
    queue(&root, "ok", &[None]);
    let (out, _) = run(&root, &verbs, &[]);
    assert_eq!(
        queue_line(&out),
        "ops-runner: queue host=forge depth=1 oldest_wait_s=-",
        "an unstamped packet has no age: {out}"
    );

    // And the answered request carries what IT waited, beside the exit
    // that already rides there — so the depth series is a jobs-API
    // query, not a journal nobody opens.
    queue(&root, "ok", &[Some(300), Some(45)]);
    let (out, _) = run(&root, &verbs, &[]);
    let patch: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("patch.json"))
            .unwrap_or_else(|e| panic!("no PATCH on the request's metadata: {e}; {out}")),
    )
    .expect("the PATCH body is JSON");
    assert_eq!(patch["queue_depth"], 2, "{patch} / {out}");
    let waited = patch["queued_s"]
        .as_i64()
        .unwrap_or_else(|| panic!("the request records its own wait: {patch} / {out}"));
    assert!(
        (45..105).contains(&waited),
        "the LAST packet walked waited its own 45s, not the queue's oldest: {patch}"
    );
}

/// A LIMIT IS NOT A FILTER (backlog 2cfb4562). The reading above was
/// taken from `?kind=ops-request&status=open&limit=100` — EVERY host's
/// open requests, one page of them, with the `total` the API answers
/// beside the rows thrown away. At a depth above 100 the walk silently
/// ignored the tail and the gauge printed the page as though it were
/// the whole queue: a queue stuck at 400 reads 100 forever, and any
/// threshold set above 100 could never be crossed. The same shape was
/// found the same day in a recorded probe (limit=300 against a live
/// total of 345, its one qualifying row in the unread tail).
///
/// So the query is narrowed to THIS host by the server, the rows are
/// compared against the `total` it answers, and a reading that could
/// not see the whole queue says so in a shape no reader can take for
/// an exact number: `depth=` only when the count is the server's own
/// for this host, `depth>=` when it is a lower bound, and the oldest
/// wait likewise — the list is newest first, so the tail it did not
/// read is exactly where the oldest packets are.
#[test]
fn a_queue_the_runner_could_not_see_all_of_is_not_reported_as_exact() {
    needs_jq!();
    let root = scratch("queue-truncated");
    stub_sor(&root);
    let ok = root.join("ok.sh");
    write_exec(&ok, "#!/bin/sh\necho ok\n");
    let verbs = verbs_dir(
        &root,
        &[(
            "ok",
            &format!(
                r#"{{"about":"a verb that answers","hosts":["forge"],"argv":["{}"],"params":[]}}"#,
                ok.display()
            ),
        )],
    );

    // The server is asked for THIS host's queue, not every host's
    // first page: the host rides the containment filter, url-encoded.
    queue(&root, "ok", &[Some(30)]);
    let (out, _) = run(&root, &verbs, &[]);
    let url = std::fs::read_to_string(root.join("get.url"))
        .unwrap_or_else(|e| panic!("the runner made no list read: {e}; {out}"));
    assert!(
        url.contains("metadata=%7B%22host%22%3A%22forge%22%7D"),
        "the list read is narrowed to this host by the server: {url}"
    );
    assert!(
        !url.split(['?', '&']).any(|kv| kv.trim() == "limit=100"),
        "the page is not the old 100 cap: {url}"
    );

    // Two rows on the page, five open: the depth is the server's count
    // and the walk knows it saw only part of it.
    queue_of(&root, "ok", &[Some(120), Some(30)], Some(5));
    let (out, _) = run(&root, &verbs, &[]);
    let line = queue_line(&out);
    assert_eq!(
        field(&line, "depth"),
        "5",
        "the depth is the server's total: {line}"
    );
    assert!(
        line.contains("oldest_wait_s>=") && line.contains("truncated"),
        "the oldest wait of a partial read is a lower bound, and the line says why: {line}"
    );
    assert!(
        !line
            .split_whitespace()
            .any(|w| w.starts_with("oldest_wait_s=")),
        "a lower bound is never printed in the exact shape: {line}"
    );
    let patch = read_patch(&root, &out);
    assert_eq!(
        patch["queue_depth"], 5,
        "the request carries the whole queue's depth: {patch}"
    );

    // No `total` at all: the runner cannot know how much it did not
    // see, so it REFUSES the exact reading rather than printing the
    // page as the queue — and the request says "at least", under a key
    // no reader of `queue_depth` can mistake for the count.
    queue_of(&root, "ok", &[Some(120), Some(30)], None);
    let (out, _) = run(&root, &verbs, &[]);
    let line = queue_line(&out);
    assert!(
        line.contains("depth>=2") && line.contains("truncated"),
        "an unverifiable count is a lower bound, named: {line}"
    );
    assert!(
        !line.split_whitespace().any(|w| w.starts_with("depth=")),
        "no exact depth without a total to check it against: {line}"
    );
    let patch = read_patch(&root, &out);
    assert!(
        patch.get("queue_depth").is_none(),
        "a lower bound is not written as the depth: {patch}"
    );
    assert_eq!(patch["queue_depth_at_least"], 2, "{patch}");

    // A server that ignored the host filter answers every host's
    // `total` — the wrong-target shape (CLAUDE.md §Doors: it answers
    // instead of erroring). A row for another host on the page is the
    // evidence, and that total is not this host's depth.
    std::fs::write(
        root.join("jobs.json"),
        r#"{"data":[{"id":"bbbbbbbb-0000-4000-8000-000000000000","status":"open","metadata":{"host":"boss-gcp","verb":"ok","args":[]},"steps":[]}],"total":1}"#,
    )
    .unwrap();
    let (out, _) = run(&root, &verbs, &[]);
    let line = queue_line(&out);
    assert!(
        line.contains("depth>=0") && line.contains("truncated"),
        "another host's total is not this host's depth: {line}"
    );
}

/// The request-level PATCH the last run wrote, or a panic naming what
/// the run printed instead.
fn read_patch(root: &Path, out: &str) -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(root.join("patch.json"))
            .unwrap_or_else(|e| panic!("no PATCH on the request's metadata: {e}; {out}")),
    )
    .expect("the PATCH body is JSON")
}

/// The runner's one-line queue reading, or a panic naming what it
/// printed instead.
fn queue_line(out: &str) -> String {
    out.lines()
        .find(|l| l.starts_with("ops-runner: queue "))
        .unwrap_or_else(|| panic!("no queue reading in the run's output:\n{out}"))
        .to_string()
}

/// `key=value` off a space-separated reading line.
fn field(line: &str, key: &str) -> String {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in {line}"))
        .to_string()
}

/// A queue of open ops-requests for the forge, one per entry, each
/// carrying the `opened_at` a live request carries (`Some(age)`
/// seconds ago) or none at all.
fn queue(root: &Path, verb: &str, ages_s: &[Option<u64>]) {
    queue_of(root, verb, ages_s, Some(ages_s.len()));
}

/// [`queue`], with the `total` the server answers beside the rows set
/// by hand: `Some(n)` larger than the rows is a page that did not hold
/// the whole queue, and `None` is a response that carries no `total`.
fn queue_of(root: &Path, verb: &str, ages_s: &[Option<u64>], total: Option<usize>) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let rows: Vec<String> = ages_s
        .iter()
        .enumerate()
        .map(|(i, age)| {
            let opened = match age {
                Some(a) => format!(r#","opened_at":"{}""#, iso_at(now - a)),
                None => String::new(),
            };
            format!(
                r#"{{"id":"aaaaaaaa-0000-4000-8000-00000000000{i}","status":"open","metadata":{{"host":"forge","verb":"{verb}","args":[]{opened}}},"steps":[{{"id":"s-{i}","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}"#
            )
        })
        .collect();
    std::fs::write(
        root.join("jobs.json"),
        match total {
            Some(t) => format!(r#"{{"data":[{}],"total":{t}}}"#, rows.join(",")),
            None => format!(r#"{{"data":[{}]}}"#, rows.join(",")),
        },
    )
    .unwrap();
}

/// The timestamp shape a live ops-request carries in `metadata.opened_at`
/// (read off request e8248c26, 2026-09-20): RFC 3339, nanoseconds, an
/// explicit `+00:00` offset.
fn iso_at(epoch: u64) -> String {
    let out = Command::new("date")
        .args([
            "-u",
            "-d",
            &format!("@{epoch}"),
            "+%Y-%m-%dT%H:%M:%S.000000000+00:00",
        ])
        .output()
        .expect("date runs");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A verb that declares `requires_approval` is REFUSED unless the runner
/// verified an approval — and the refusal says which verb, and why.
///
/// This was the safety half of design 17835005 (passkey-approved,
/// machine-executed destructive operations), and it landed BEFORE the
/// thing that issues approvals, deliberately: the guard matters more than
/// the approval, so the first such verb was inert on arrival. The
/// approval channel landed later (backlog fd7090cc; its cases are
/// `ops_runner_approval_sh.rs`), and this case keeps the floor under it:
/// a request with no approve step at all — nothing any approval could
/// ride on — is still refused, naming the verb.
///
/// FAIL CLOSED IS THE WHOLE POINT. A runner that cannot check an
/// approval must refuse, never assume; `commission-a-disk` partitions a
/// block device, and the failure mode of assuming is a wiped host.
#[test]
fn a_verb_requiring_approval_is_refused_without_a_verified_approval() {
    needs_jq!();
    let root = scratch("requires-approval");
    stub_sor(&root);
    let verbs = verbs_dir(
        &root,
        &[(
            "needs-a-human",
            r#"{"about":"MUTATING — test fixture.","hosts":["forge"],
                "requires_approval":true,
                "argv":["true"],"params":[]}"#,
        )],
    );
    packet(&root, "needs-a-human", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "refused", "{md}");
    let reason = md["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("no reason on the step: {md}"));
    assert!(
        reason.contains("needs-a-human") && reason.contains("approval"),
        "the refusal names the verb and what it wants: {reason}"
    );
    assert!(
        md.get("exit_code").is_none(),
        "a refusal ran nothing — and this one in particular must not: {md}"
    );
}

/// The refusal is about the DECLARATION, not about the verb being
/// unusual: an ordinary verb in the same allowlist still runs. Without
/// this control the test above passes against a runner that refuses
/// everything.
#[test]
fn a_verb_that_requires_no_approval_still_runs_beside_one_that_does() {
    needs_jq!();
    let root = scratch("requires-approval-control");
    stub_sor(&root);
    let verbs = verbs_dir(
        &root,
        &[
            (
                "needs-a-human",
                r#"{"about":"MUTATING — test fixture.","hosts":["forge"],
                    "requires_approval":true,"argv":["true"],"params":[]}"#,
            ),
            (
                "ordinary",
                r#"{"about":"a read.","hosts":["forge"],"argv":["true"],"params":[]}"#,
            ),
        ],
    );
    packet(&root, "ordinary", "[]");
    let (out, payload) = run(&root, &verbs, &[]);
    let md = payload.unwrap_or_else(|| panic!("no step completed: {out}"));
    assert_eq!(md["disposition"], "answered", "{md}");
    assert_eq!(md["exit_code"], "0", "{md}");
}

/// The shipped verb declares it, and the shipped script refuses the
/// shapes that would aim it at the wrong disk.
///
/// The declaration is a one-word difference between a verb that is
/// inert and a verb that partitions a block device on request, so it
/// is pinned rather than trusted to review.
#[test]
fn the_disk_verb_is_inert_and_refuses_an_unstable_target() {
    let root = repo_root();
    let spec: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("infra/ops/verbs/commission-a-disk.json"))
            .expect("the verb file"),
    )
    .expect("the verb file is JSON");
    assert_eq!(
        spec["requires_approval"], true,
        "commission-a-disk writes a partition table; it stays inert until an approval can \
         be verified"
    );
    assert!(
        spec["about"]
            .as_str()
            .unwrap_or_default()
            .contains("MUTATING"),
        "the bounded-verbs lint derives its roster from that word"
    );

    // The script's refusals, exercised. A success path cannot be tested
    // here — it would partition the gate runner — so the failure paths
    // are the whole test, which is the right way round for a verb whose
    // only interesting property is what it declines to do.
    let script = root.join("infra/forge/commission-a-disk.sh");
    let cases: [(&[&str], &str); 4] = [
        (&["/dev/nvme0n1", "/srv/data"], "stable identity"),
        (&["nvme0n1", "/srv/data"], "stable identity"),
        (
            &["/dev/disk/by-id/nvme-NO-SUCH-DEVICE-0000", "/srv/data"],
            "no such device",
        ),
        (
            &["/dev/disk/by-id/nvme-NO-SUCH-DEVICE-0000", "relative/path"],
            "must be absolute",
        ),
    ];
    for (args, expected) in cases {
        let out = Command::new("sh")
            .arg(&script)
            .args(args)
            .output()
            .expect("the script runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(78),
            "a wrong request is EX_CONFIG, not a failed run: {args:?} -> {text}"
        );
        assert!(
            text.contains(expected),
            "the refusal says which precondition failed: {args:?} -> {text}"
        );
    }
}
