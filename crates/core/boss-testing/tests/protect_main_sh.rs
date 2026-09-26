//! `infra/forge/protect-main.sh` — the forge converge makes forge main's
//! branch protection what `infra/forge/main-protection.json` declares,
//! and reads it back (backlog f9256445, car 4 of design d812f1b7; David
//! answered Q2 "yes", 2026-09-25: direct push and force push off, so only
//! a PR merge — the train conductor's — moves main; declared in the tree,
//! applied by the converge, never set by hand in the Forgejo UI).
//!
//! Pinned here, against a stub curl that keeps the forge's rule in a
//! file and records every call:
//!   * absent -> created from the declaration, then READ BACK and compared
//!     key by key; already as declared -> no write at all; drifted (a hand
//!     in the UI turned push back on) -> edited, naming the keys
//!   * a refused write (403: the converge's credential cannot administer
//!     the repository) is exit 1 and says protection is NOT in force
//!   * a write that answers 2xx and does not read back as declared is
//!     exit 1 — an answer is not an effect
//!   * no credential, or a forge that cannot be read: exit 4, and no
//!     write is ever attempted on a guess
//!   * the token never reaches argv, stdout or stderr
//!   * the DECLARATION keeps the conductor's merge path open: nothing in it
//!     a squash merge by the conductor's account could fail (status checks,
//!     approvals, outdated-branch blocks, a merge allowlist, protected
//!     files), with the Forgejo v16.0.2 pre-receive code that makes that
//!     true cited beside the file
//!   * forge-converge.sh runs it, and its verdict reaches the packet

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

const TOKEN: &str = "fj-token-must-never-print-0123456789";

fn script() -> PathBuf {
    repo_root().join("infra/forge/protect-main.sh")
}

fn declaration() -> serde_json::Value {
    let text = std::fs::read_to_string(repo_root().join("infra/forge/main-protection.json"))
        .expect("infra/forge/main-protection.json is the declaration");
    serde_json::from_str(&text).expect("the declaration is JSON")
}

/// The stub curl. The script calls it in ONE fixed shape:
///   curl -sS -m <s> -o <out> -w %{http_code} -H @<auth> -H <ct> -X <M> [--data-binary @<body>] <url>
/// It records `<M> <url>` and the body (if any) to `$STUB_LOG`, and keeps
/// the forge's rule for main in `$STUB_DIR/live.json`:
///   GET    -> live.json and 200, or 404 when there is none
///   POST   -> live.json := body, 201
///   PATCH  -> live.json := live.json * body, 200
/// Faults: `$STUB_DIR/fail` (curl itself fails, exit 7), `$STUB_DIR/
/// status_<M>` (answer that status to method M and change nothing),
/// `$STUB_DIR/ignore_writes` (answer 2xx to a write and change nothing).
fn write_curl_stub(dir: &Path) -> PathBuf {
    let stub = dir.join("curl");
    boss_testing::write_exec(
        &stub,
        r#"#!/usr/bin/env bash
set -u
out=""; method=GET; body=""; url=""; auth=""
while [ $# -gt 0 ]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        -X) method="$2"; shift 2 ;;
        -H) case "$2" in @*) auth="${2#@}" ;; esac; shift 2 ;;
        --data-binary) body="${2#@}"; shift 2 ;;
        -m|-w) shift 2 ;;
        -*) shift ;;
        *) url="$1"; shift ;;
    esac
done
{ echo "$method $url"; [ -n "$body" ] && cat "$body"; echo; echo '=== call ==='; } >> "$STUB_LOG"
[ -e "$STUB_DIR/fail" ] && { echo "curl: (7) Failed to connect to forge port 3000" >&2; exit 7; }
grep -q '^Authorization: token ' "$auth" || { echo '{"message":"no token"}' > "$out"; printf 401; exit 0; }
if [ -e "$STUB_DIR/status_$method" ]; then
    echo '{"message":"stub refusal"}' > "$out"; cat "$STUB_DIR/status_$method"; exit 0
fi
live="$STUB_DIR/live.json"
case "$method" in
    GET)
        if [ -e "$live" ]; then cp "$live" "$out"; printf 200
        else echo '{"message":"The target couldn'"'"'t be found."}' > "$out"; printf 404; fi ;;
    POST)
        [ -e "$STUB_DIR/ignore_writes" ] || jq '. + {created_at: "2026-09-25T23:00:00Z"}' "$body" > "$live"
        cp "$body" "$out"; printf 201 ;;
    PATCH)
        [ -e "$STUB_DIR/ignore_writes" ] || { jq -s '.[0] * .[1]' "$live" "$body" > "$live.new" && mv "$live.new" "$live"; }
        cp "$live" "$out"; printf 200 ;;
esac
"#,
    );
    stub
}

#[derive(Default)]
struct Fixture {
    live: Option<serde_json::Value>,
    fail: bool,
    status: Option<(&'static str, &'static str)>,
    ignore_writes: bool,
    no_auth: bool,
    summary: bool,
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    log: String,
    live: Option<serde_json::Value>,
    summary: Option<serde_json::Value>,
}

fn run(case: &str, f: Fixture) -> Run {
    let dir = boss_testing::scratch_dir(&format!("protect-main-{case}"));
    let curl = write_curl_stub(&dir);
    let state = dir.join("state");
    boss_testing::create_dir(&state);
    if let Some(live) = &f.live {
        boss_testing::write_file(&state.join("live.json"), &live.to_string());
    }
    if f.fail {
        boss_testing::write_file(&state.join("fail"), "");
    }
    if let Some((method, status)) = f.status {
        boss_testing::write_file(&state.join(format!("status_{method}")), status);
    }
    if f.ignore_writes {
        boss_testing::write_file(&state.join("ignore_writes"), "");
    }
    let auth = dir.join("auth-header");
    if !f.no_auth {
        boss_testing::write_file(&auth, &format!("Authorization: token {TOKEN}\n"));
    }
    let log = dir.join("calls.log");
    let summary = dir.join("summary.json");
    let mut cmd = Command::new("bash");
    cmd.arg(script())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_FORGE_PROTECT_CURL", &curl)
        .env("BOSS_FORGE_URL", "http://forge.test:3000")
        .env("BOSS_FORGE_AUTH_HEADER_FILE", &auth)
        .env("STUB_LOG", &log)
        .env("STUB_DIR", &state);
    if f.summary {
        cmd.env("BOSS_RUN_SUMMARY_FILE", &summary);
    }
    let out = cmd.output().expect("bash runs");
    let read_json = |p: &Path| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
    };
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        log: std::fs::read_to_string(&log).unwrap_or_default(),
        live: read_json(&state.join("live.json")),
        summary: read_json(&summary),
    }
}

/// The calls the stub saw, as `METHOD url` lines.
fn calls(log: &str) -> Vec<String> {
    log.split("=== call ===\n")
        .filter_map(|c| c.lines().next().map(str::to_string))
        .filter(|c| !c.is_empty())
        .collect()
}

const RULE_URL: &str = "http://forge.test:3000/api/v1/repos/david/boss/branch_protections/main";
const LIST_URL: &str = "http://forge.test:3000/api/v1/repos/david/boss/branch_protections";

fn no_token_anywhere(r: &Run) {
    for (what, text) in [
        ("stdout", &r.stdout),
        ("stderr", &r.stderr),
        ("argv", &r.log),
    ] {
        assert!(!text.contains(TOKEN), "the token reached {what}:\n{text}");
    }
}

/// Every declared key reads back on the forge's rule with the declared value.
fn live_matches_declaration(live: &serde_json::Value) {
    let decl = declaration();
    for (k, v) in decl.as_object().expect("the declaration is an object") {
        assert_eq!(live.get(k), Some(v), "live key {k} in {live}");
    }
}

#[test]
fn an_absent_rule_is_created_from_the_declaration_and_read_back() {
    let r = run(
        "absent",
        Fixture {
            summary: true,
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        calls(&r.log),
        vec![
            format!("GET {RULE_URL}"),
            format!("POST {LIST_URL}"),
            format!("GET {RULE_URL}"),
        ],
        "read, create, read back — in that order"
    );
    live_matches_declaration(r.live.as_ref().expect("the rule now exists"));
    assert!(
        r.stdout.contains("created") && r.stdout.contains("read back as declared"),
        "{}",
        r.stdout
    );
    let s = r
        .summary
        .as_ref()
        .expect("the verdict reaches the converge's packet");
    let verdict = s["main_protection"].as_str().unwrap_or_default();
    assert!(verdict.starts_with("created"), "{s}");
    no_token_anywhere(&r);
}

#[test]
fn a_rule_already_as_declared_is_not_written() {
    let mut live = declaration();
    live["created_at"] = serde_json::json!("2026-09-25T23:00:00Z");
    // Keys the forge carries that the declaration does not name are the
    // forge's business, not drift.
    live["push_whitelist_usernames"] = serde_json::json!([]);
    let r = run(
        "steady",
        Fixture {
            live: Some(live),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        calls(&r.log),
        vec![format!("GET {RULE_URL}")],
        "no write on a steady tick"
    );
    assert!(r.stdout.contains("already as declared"), "{}", r.stdout);
}

#[test]
fn a_rule_changed_by_hand_is_put_back_naming_the_keys() {
    // Someone turned direct push back on in the UI.
    let mut live = declaration();
    live["enable_push"] = serde_json::json!(true);
    let r = run(
        "drifted",
        Fixture {
            live: Some(live),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        calls(&r.log),
        vec![
            format!("GET {RULE_URL}"),
            format!("PATCH {RULE_URL}"),
            format!("GET {RULE_URL}"),
        ]
    );
    live_matches_declaration(r.live.as_ref().expect("the rule exists"));
    assert!(
        r.stdout.contains("edited") && r.stdout.contains("enable_push"),
        "the drifted key is named: {}",
        r.stdout
    );
    // rule_name identifies the rule in the URL; an edit body does not carry it.
    let body = r.log.lines().nth(1).unwrap_or_default();
    assert!(!body.contains("rule_name"), "PATCH body: {body}");
}

#[test]
fn a_refused_write_says_protection_is_not_in_force() {
    let r = run(
        "refused",
        Fixture {
            status: Some(("POST", "403")),
            summary: true,
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}{}", r.stdout, r.stderr);
    assert!(
        r.stderr.contains("HTTP 403") && r.stderr.contains("NOT in force"),
        "{}",
        r.stderr
    );
    let s = r.summary.as_ref().expect("the failure reaches the packet");
    assert!(
        s["main_protection"]
            .as_str()
            .unwrap_or_default()
            .starts_with("FAILED"),
        "{s}"
    );
    no_token_anywhere(&r);
}

#[test]
fn a_write_that_answers_but_does_not_take_is_a_failure() {
    let r = run(
        "ignored",
        Fixture {
            ignore_writes: true,
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(1), "{}{}", r.stdout, r.stderr);
    assert!(
        r.stderr.contains("does not read back"),
        "an answer is not an effect: {}",
        r.stderr
    );
}

#[test]
fn an_unreadable_forge_is_exit_4_and_nothing_is_written() {
    let r = run(
        "dark",
        Fixture {
            fail: true,
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(4), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        calls(&r.log),
        vec![format!("GET {RULE_URL}")],
        "no write on a guess"
    );
    assert!(
        r.stderr.contains("Failed to connect"),
        "the tool's own words: {}",
        r.stderr
    );

    let r = run(
        "unreadable",
        Fixture {
            status: Some(("GET", "500")),
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(4), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        calls(&r.log).len(),
        1,
        "no write after a 500: {:?}",
        calls(&r.log)
    );
}

#[test]
fn no_credential_is_exit_4_before_any_call() {
    let r = run(
        "no-auth",
        Fixture {
            no_auth: true,
            ..Fixture::default()
        },
    );
    assert_eq!(r.code, Some(4), "{}{}", r.stdout, r.stderr);
    assert!(calls(&r.log).is_empty(), "{:?}", calls(&r.log));
    assert!(
        r.stderr.contains("BOSS_FORGE_AUTH_HEADER_FILE"),
        "{}",
        r.stderr
    );
}

/// The declaration is what David approved (Q2): no direct push by anyone
/// (enable_push false, no push allowlist). Force push and deletion need no
/// key — Forgejo v16.0.2 refuses both on ANY protected branch before it
/// looks at the rule's settings (routers/private/hook_pre_receive.go,
/// preReceiveBranch steps 1 and 2).
///
/// And it keeps the conductor's merge open. A PR merge reaches the same
/// hook with a PullRequestID, so it passes step 5's `!canPush` into 6b:
/// IsUserAllowedToMerge (merge allowlist OFF falls back to write access
/// on the code unit — the conductor's account merges today), then
/// CheckPullBranchProtections (required approvals, status checks, the
/// review blocks and outdated-branch). Every one of those is declared
/// OFF, explicitly, so a key the forge defaults differently cannot close
/// the path, and so does changing a protected-file pattern. A squash merge
/// is a fast-forward of main (its parent is main's head), so step 2 never
/// sees it as a force push. Rehearsed against a real Forgejo 16.0.2 on
/// 2026-09-25 (the car's park record carries it).
#[test]
fn the_declaration_turns_off_direct_push_and_keeps_the_merge_path_open() {
    let d = declaration();
    let get = |k: &str| d.get(k).cloned().unwrap_or(serde_json::Value::Null);
    assert_eq!(get("rule_name"), serde_json::json!("main"));
    assert_eq!(
        get("enable_push"),
        serde_json::json!(false),
        "direct push off"
    );
    assert_eq!(
        get("enable_push_whitelist"),
        serde_json::json!(false),
        "for everyone"
    );
    for off in [
        "enable_merge_whitelist",
        "enable_status_check",
        "block_on_rejected_reviews",
        "block_on_official_review_requests",
        "block_on_outdated_branch",
        "require_signed_commits",
        "apply_to_admins",
    ] {
        assert_eq!(
            get(off),
            serde_json::json!(false),
            "{off} must be declared false: it could refuse the conductor's merge"
        );
    }
    assert_eq!(get("required_approvals"), serde_json::json!(0));
    assert_eq!(get("protected_file_patterns"), serde_json::json!(""));
}

#[test]
fn the_forge_converge_applies_it_and_reports_it() {
    let converge = std::fs::read_to_string(repo_root().join("infra/forge/forge-converge.sh"))
        .expect("forge-converge.sh");
    assert!(
        converge.contains("infra/forge/protect-main.sh"),
        "forge-converge.sh must run protect-main.sh on every tick"
    );
    let script = std::fs::read_to_string(script()).expect("protect-main.sh");
    assert!(
        script.contains("main-protection.json"),
        "protect-main.sh reads the tree's declaration"
    );
}
