//! `infra/forge/forge-auth-header.sh` — the forge converge's credential
//! for protect-main, read off the checkout's own `forgejo` remote URL
//! (backlog 164f38c7).
//!
//! WHY. Car 4 of design d812f1b7 filled protect-main's header file from
//! `git credential fill` run as the checkout's owner. The owner has NO
//! credential helper (measured 2026-09-19 on ops-request 3d9d5f58, and
//! recorded in publish-github-pr.sh): the converge's fetch authenticates
//! because the remote URL carries the credential as userinfo. So every
//! converge since #694 closed FAILED with protect-main's exit 4 ("names
//! no non-empty header file": runs 954387f5, b4bcb5af, 868483c1), and
//! git's prompt-disabled error names that userinfo on stderr, which is
//! the converge's journal (backlog 37497977).
//!
//! Pinned here, against a stub command standing in for
//! `runuser -l <owner> -c "git -C <repo> remote get-url forgejo"`:
//!   * `user:tok@` and `tok@` userinfo each become `Authorization: Basic`
//!     of the url-decoded userinfo (a token-only one gains the trailing
//!     colon), written into the header file, mode 0600
//!   * a URL with no userinfo, or a scheme that is not http(s), leaves
//!     the file empty and exits 3 with a message naming no value — so
//!     protect-main keeps its exit 4
//!   * a failing reader's stderr is printed REDACTED: no stderr line
//!     carries the stub token, even when the reader's own error names it
//!   * the value never reaches stdout, stderr or a child's argv
//!   * forge-converge.sh reads through it and no longer through
//!     `git credential fill`

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Short and not hex, so the tree's own secret lint has nothing to say
/// about a fixture; distinctive, so a leak anywhere is found by search.
const STUB_TOKEN: &str = "stub-tok-never-print-7";

fn script() -> PathBuf {
    repo_root().join("infra/forge/forge-auth-header.sh")
}

/// RFC 4648 base64, standard alphabet with padding — the expected value
/// computed here rather than by the tool under test.
fn b64(input: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    input
        .chunks(3)
        .flat_map(|c| {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            (0..4).map(move |i| {
                if i > c.len() {
                    '='
                } else {
                    char::from(A[((n >> (18 - 6 * i)) & 63) as usize])
                }
            })
        })
        .collect()
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    header: Option<String>,
    mode: Option<u32>,
    argv_log: String,
}

/// Run the helper with a stub reader that prints `url` on stdout (and
/// `err` on stderr, exiting `rc`). A stub `base64` first on PATH
/// records its argv and hands off to the real one, so a value that
/// reached an argv would be seen.
fn run(case: &str, url: &str, err: &str, rc: i32, precreate: bool) -> Run {
    use std::os::unix::fs::PermissionsExt;
    let dir = boss_testing::scratch_dir(&format!("forge-auth-header-{case}"));
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    let argv_log = dir.join("argv.log");
    let real_base64 = Command::new("sh")
        .args(["-c", "command -v base64"])
        .output()
        .expect("sh runs");
    let real_base64 = String::from_utf8_lossy(&real_base64.stdout)
        .trim()
        .to_string();
    assert!(!real_base64.is_empty(), "base64 is on PATH");
    boss_testing::write_exec(
        &bin.join("base64"),
        &format!(
            "#!/usr/bin/env bash\necho \"base64 $*\" >>\"$STUB_ARGV_LOG\"\nexec {real_base64} \"$@\"\n"
        ),
    );
    boss_testing::write_file(&dir.join("url"), url);
    boss_testing::write_file(&dir.join("err"), err);
    let reader = dir.join("reader");
    boss_testing::write_exec(
        &reader,
        &format!(
            "#!/usr/bin/env bash\ncat \"$STUB_DIR/url\"\ncat \"$STUB_DIR/err\" >&2\nexit {rc}\n"
        ),
    );
    let header = dir.join("auth-header");
    if precreate {
        boss_testing::write_file(&header, "");
        std::fs::set_permissions(&header, std::fs::Permissions::from_mode(0o600))
            .expect("chmod the header file");
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg(script())
        .arg(&header)
        .arg(&reader)
        .env_clear()
        .env("PATH", path)
        .env("STUB_DIR", &dir)
        .env("STUB_ARGV_LOG", &argv_log)
        .output()
        .expect("bash runs");
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        header: std::fs::read_to_string(&header).ok(),
        mode: std::fs::metadata(&header)
            .ok()
            .map(|m| m.permissions().mode() & 0o777),
        argv_log: std::fs::read_to_string(&argv_log).unwrap_or_default(),
    }
}

/// The token appears nowhere but the header file: not on stdout, not on
/// any stderr line, not on a child's argv — in plain text or encoded.
fn nowhere_but_the_file(r: &Run, secret: &str) {
    let encoded = b64(secret.as_bytes());
    for (what, text) in [
        ("stdout", &r.stdout),
        ("stderr", &r.stderr),
        ("argv", &r.argv_log),
    ] {
        for line in text.lines() {
            assert!(
                !line.contains(STUB_TOKEN) && !line.contains(&encoded),
                "the credential reached {what}: {line}"
            );
        }
    }
}

fn path(p: &Path) -> String {
    p.display().to_string()
}

#[test]
fn user_and_token_userinfo_becomes_basic_of_both() {
    let url = format!("http://david:{STUB_TOKEN}@forge.test:3000/david/boss.git\n");
    let r = run("user-token", &url, "", 0, true);
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    let want = format!(
        "Authorization: Basic {}\n",
        b64(format!("david:{STUB_TOKEN}").as_bytes())
    );
    assert_eq!(r.header.as_deref(), Some(want.as_str()));
    assert_eq!(r.mode, Some(0o600), "the header file stays root-only");
    nowhere_but_the_file(&r, &format!("david:{STUB_TOKEN}"));
}

#[test]
fn token_only_userinfo_gains_the_trailing_colon() {
    // Forgejo reads `token:` (empty password) as the token-as-username.
    let url = format!("https://{STUB_TOKEN}@forge.test/david/boss.git\n");
    let r = run("token-only", &url, "", 0, true);
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    let want = format!(
        "Authorization: Basic {}\n",
        b64(format!("{STUB_TOKEN}:").as_bytes())
    );
    assert_eq!(r.header.as_deref(), Some(want.as_str()));
    nowhere_but_the_file(&r, &format!("{STUB_TOKEN}:"));
}

#[test]
fn percent_encoded_userinfo_is_decoded_before_it_is_encoded() {
    // `%40` is `@` and `%3A` is `:` inside the password part; the cut
    // at the LAST `@` of the authority keeps the encoded ones whole.
    let url = format!("http://d%40vid:{STUB_TOKEN}%3Ax@forge.test:3000/david/boss.git\n");
    let r = run("encoded", &url, "", 0, true);
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    let want = format!(
        "Authorization: Basic {}\n",
        b64(format!("d@vid:{STUB_TOKEN}:x").as_bytes())
    );
    assert_eq!(r.header.as_deref(), Some(want.as_str()));
}

#[test]
fn a_header_file_it_creates_is_root_only() {
    let url = format!("http://{STUB_TOKEN}@forge.test/david/boss.git\n");
    let r = run("creates", &url, "", 0, false);
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(r.mode, Some(0o600));
}

#[test]
fn a_url_without_userinfo_leaves_the_file_empty_and_says_so() {
    for (case, url) in [
        ("bare", "http://forge.test:3000/david/boss.git\n"),
        // An `@` in the PATH is not userinfo.
        ("at-in-path", "http://forge.test:3000/david/b@ss.git\n"),
        // Not a credential a Basic header can carry.
        ("ssh", "ssh://git@forge.test:2222/david/boss.git\n"),
        ("scp", "git@forge.test:david/boss.git\n"),
        ("empty", ""),
    ] {
        let r = run(case, url, "", 0, true);
        assert_eq!(r.code, Some(3), "{case}: {}{}", r.stdout, r.stderr);
        assert_eq!(r.header.as_deref(), Some(""), "{case}: file left empty");
        assert!(
            r.stderr.contains("no credential"),
            "{case}: the reason is named: {}",
            r.stderr
        );
        assert!(
            !r.stderr.contains("forge.test"),
            "{case}: the message names no value from the URL: {}",
            r.stderr
        );
    }
}

/// git's own words when it cannot authenticate name the userinfo:
/// `could not read Password for 'http://<userinfo>@host'` — which is
/// exactly what reached the converge's journal (backlog 37497977).
#[test]
fn a_failing_readers_error_is_printed_redacted() {
    let url = format!("http://{STUB_TOKEN}@forge.test/david/boss.git\n");
    let err = format!(
        "fatal: could not read Password for 'http://{STUB_TOKEN}@forge.test': terminal prompts disabled\n\
         error: also https://david:{STUB_TOKEN}@forge.test/x and {STUB_TOKEN} bare\n"
    );
    let r = run("reader-fails", &url, &err, 128, true);
    assert_eq!(r.code, Some(3), "{}{}", r.stdout, r.stderr);
    assert_eq!(
        r.header.as_deref(),
        Some(""),
        "a failed read writes nothing"
    );
    assert!(
        r.stderr.contains("could not read Password") && r.stderr.contains("<redacted>"),
        "the reader's error survives, redacted: {}",
        r.stderr
    );
    assert!(r.stderr.contains("rc 128"), "{}", r.stderr);
    nowhere_but_the_file(&r, &format!("{STUB_TOKEN}:"));
}

#[test]
fn a_reader_that_answers_with_noise_on_stderr_still_redacts_it() {
    let url = format!("http://david:{STUB_TOKEN}@forge.test/david/boss.git\n");
    let err = format!("warning: remote http://david:{STUB_TOKEN}@forge.test is slow\n");
    let r = run("reader-warns", &url, &err, 0, true);
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("<redacted>"), "{}", r.stderr);
    nowhere_but_the_file(&r, &format!("david:{STUB_TOKEN}"));
}

#[test]
fn the_forge_converge_reads_the_remote_url_not_the_credential_helper() {
    let converge = std::fs::read_to_string(repo_root().join("infra/forge/forge-converge.sh"))
        .expect("forge-converge.sh");
    // Code only: the comment beside the read says why the fill is gone.
    let code: Vec<&str> = converge
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    assert!(
        !code.iter().any(|l| l.contains("credential fill")),
        "the owner has no credential helper; `git credential fill` reads nothing \
         and its error prints the userinfo"
    );
    let line = code
        .iter()
        .find(|l| l.contains("forge-auth-header.sh"))
        .expect("forge-converge.sh runs infra/forge/forge-auth-header.sh");
    assert!(
        line.contains("\"$auth_hdr\"")
            && line.contains("runuser -l \"$OWNER\"")
            && line.contains("remote get-url forgejo"),
        "it hands the helper the header file and the owner's read of the remote: {line}"
    );
    assert!(
        !converge.contains("set -x"),
        "{}: no xtrace",
        path(&repo_root().join("infra/forge/forge-converge.sh"))
    );
}
