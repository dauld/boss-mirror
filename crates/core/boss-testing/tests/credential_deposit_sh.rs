//! `infra/forge/credential-deposit.sh` — the forge host takes its
//! checkout's forge token from the broker's Secret into a FILE behind a
//! git credential helper, and records delivery so the broker may revoke
//! the old one (design 1c90d183, David 2026-09-26; backlog c4cbc6b5).
//!
//! WHY. The token lived in the userinfo of the checkout's `forgejo`
//! remote, and a git error printed that URL into the forge-converge
//! journal. A token in a URL is the shape that leaked (D2), so the value
//! moves to a 0600 file read at use time by a helper scoped to the
//! forge's URL, the remote loses its userinfo — but only once the helper
//! is PROVED to authenticate, so the cutover cannot leave the host with
//! no working credential — and every later value arrives from the
//! Secret the broker fills, verified by effect before it replaces the
//! file (D1).
//!
//! HOW THIS IS MEASURED. The script runs for real against: a checkout
//! made here; a stub `kubectl` that answers the Secret read from a file;
//! an ISOLATED git config (GIT_CONFIG_GLOBAL in scratch — nothing here
//! may reach a real ~/.gitconfig); and a tiny HTTP forge on 127.0.0.1
//! that answers git's ref advertisement ONLY to Basic auth carrying an
//! accepted token and 401s everything else — so "verified by effect"
//! is git actually asking the configured helper and the forge actually
//! checking what it sent. The same server stands in for the jobs API.
//! Fixture tokens are fake and assembled here; the property every case
//! also checks is that none of them reaches the output, the run
//! summary, or a request body.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const SCRIPT: &str = "infra/forge/credential-deposit.sh";
const RULE: &str = "infra/dispatcher/rules/broker-rotates-the-forge-host-checkout-token.toml";
const CONVERGE: &str = "infra/forge/forge-converge.sh";

// Forty-character fixture tokens, shaped like Forgejo's, fake.
const OLD: &str = "0ld00000000000000000000000000000c0ffee01";
const NEW: &str = "new11111111111111111111111111111feedbe02";
const BAD: &str = "bad22222222222222222222222222222deadba03";

fn last8(s: &str) -> &str {
    &s[s.len() - 8..]
}

fn b64(input: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[derive(Clone, Debug)]
struct Req {
    method: String,
    path: String,
    body: String,
}

/// The forge and the jobs API on one loopback port.
struct Server {
    port: u16,
    accepted: Arc<Mutex<Vec<String>>>,
    jobs: Arc<Mutex<String>>,
    requests: Arc<Mutex<Vec<Req>>>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted: Arc<Mutex<Vec<String>>> = Default::default();
        let jobs = Arc::new(Mutex::new(
            r#"{"data":[],"total":0,"limit":500,"offset":0}"#.to_string(),
        ));
        let requests: Arc<Mutex<Vec<Req>>> = Default::default();
        let (a, j, r) = (accepted.clone(), jobs.clone(), requests.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                serve(conn, &a, &j, &r);
            }
        });
        Self {
            port,
            accepted,
            jobs,
            requests,
        }
    }

    fn accept(&self, tokens: &[&str]) {
        *self.accepted.lock().unwrap() = tokens.iter().map(|t| t.to_string()).collect();
    }

    fn writes(&self) -> Vec<Req> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method != "GET")
            .cloned()
            .collect()
    }
}

fn serve(
    conn: std::net::TcpStream,
    accepted: &Mutex<Vec<String>>,
    jobs: &Mutex<String>,
    requests: &Mutex<Vec<Req>>,
) {
    let mut reader = BufReader::new(conn.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let (mut auth, mut len) = (String::new(), 0usize);
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let v = v.trim().to_string();
            match k.to_ascii_lowercase().as_str() {
                "authorization" => auth = v,
                "content-length" => len = v.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    let mut body = vec![0u8; len];
    let _ = reader.read_exact(&mut body);
    let body = String::from_utf8_lossy(&body).to_string();
    requests.lock().unwrap().push(Req {
        method: method.clone(),
        path: path.clone(),
        body,
    });
    let (status, ctype, extra, text) = if path.starts_with("/david/boss.git/") {
        let ok = accepted
            .lock()
            .unwrap()
            .iter()
            .any(|t| auth == format!("Basic {}", b64(format!("david:{t}").as_bytes())));
        if !ok {
            (
                "401 Unauthorized",
                "text/plain",
                "www-authenticate: Basic realm=\"forge\"\r\n",
                String::new(),
            )
        } else if path.contains("/info/refs") {
            (
                "200 OK",
                "text/plain",
                "",
                "1111111111111111111111111111111111111111\trefs/heads/main\n".to_string(),
            )
        } else if path.ends_with("/HEAD") {
            (
                "200 OK",
                "text/plain",
                "",
                "ref: refs/heads/main\n".to_string(),
            )
        } else {
            ("404 Not Found", "text/plain", "", String::new())
        }
    } else if path.starts_with("/api/jobs?") {
        (
            "200 OK",
            "application/json",
            "",
            jobs.lock().unwrap().clone(),
        )
    } else if path.starts_with("/api/jobs/") {
        ("200 OK", "application/json", "", "{}".to_string())
    } else {
        ("404 Not Found", "text/plain", "", String::new())
    };
    let mut conn = conn;
    let _ = write!(
        conn,
        "HTTP/1.1 {status}\r\ncontent-type: {ctype}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
}

struct Case {
    root: PathBuf,
    checkout: PathBuf,
    dest: PathBuf,
    gitconfig: PathBuf,
    secret: PathBuf,
    summary: PathBuf,
    kubectl: PathBuf,
    server: Server,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("credential-deposit-{name}"));
        std::fs::create_dir_all(root.join("home")).unwrap();
        let gitconfig = root.join("gitconfig");
        write_file(&gitconfig, "");
        write_file(&root.join("sor.env"), "");
        let kubectl = root.join("kubectl");
        write_exec(
            &kubectl,
            r#"#!/usr/bin/env bash
echo "$*" >> "$STUB_KUBECTL_LOG"
if [ -n "${STUB_FORBIDDEN:-}" ]; then
  echo 'Error from server (Forbidden): secrets "forge-host-checkout-token" is forbidden' >&2; exit 1
fi
# The broker installing a new value BETWEEN two reads of one pass: the
# first read answers the Secret as it is, every later one answers
# STUB_SECRET_LATER.
if [ -n "${STUB_SECRET_LATER:-}" ]; then
  if [ -e "$STUB_SECRET_LATER.read" ]; then cp "$STUB_SECRET_LATER" "$STUB_SECRET"; else : > "$STUB_SECRET_LATER.read"; fi
fi
if [ ! -e "$STUB_SECRET" ]; then
  echo 'Error from server (NotFound): secrets "forge-host-checkout-token" not found' >&2; exit 1
fi
base64 -w0 < "$STUB_SECRET"
"#,
        );
        let c = Self {
            checkout: root.join("checkout"),
            dest: root.join("home/.config/boss/forge-checkout.token"),
            secret: root.join("secret"),
            summary: root.join("summary.json"),
            gitconfig,
            kubectl,
            server: Server::start(),
            root,
        };
        c.git(&["init", "-q", &c.checkout.display().to_string()]);
        c
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.server.port)
    }

    fn url(&self, userinfo: &str) -> String {
        format!(
            "http://{userinfo}127.0.0.1:{}/david/boss.git",
            self.server.port
        )
    }

    /// git with the isolated config and nothing of the system's.
    fn git_cmd(&self) -> Command {
        self.git_cmd_as("git")
    }

    /// Any program under the same isolated git environment.
    fn git_cmd_as(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.root.join("home"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .env_remove("http_proxy")
            .env_remove("HTTP_PROXY")
            .env_remove("https_proxy")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY");
        cmd
    }

    fn git(&self, args: &[&str]) -> String {
        let out = self.git_cmd().args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn set_remote(&self, url: &str) {
        let c = self.checkout.display().to_string();
        self.git(&["-C", &c, "remote", "remove", "forgejo"]);
        self.git(&["-C", &c, "remote", "add", "forgejo", url]);
    }

    fn remote(&self) -> String {
        self.git(&[
            "-C",
            &self.checkout.display().to_string(),
            "remote",
            "get-url",
            "forgejo",
        ])
    }

    fn helpers(&self) -> Vec<String> {
        let key = format!("credential.{}.helper", self.base());
        let out = self
            .git_cmd()
            .args(["config", "--global", "--get-all", &key])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// What git's credential machinery answers for the forge's URL with
    /// the isolated config — the helper actually run.
    fn fill(&self) -> String {
        let mut child = self
            .git_cmd()
            .args(["credential", "fill"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        boss_testing::feed_stdin(
            &mut child,
            format!("url={}/david/boss.git\n\n", self.base()).as_bytes(),
        );
        String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).to_string()
    }

    fn put_secret(&self, value: &str) {
        write_file(&self.secret, value);
    }

    fn put_file(&self, value: &str) {
        std::fs::create_dir_all(self.dest.parent().unwrap()).unwrap();
        write_file(&self.dest, value);
    }

    fn file(&self) -> Option<String> {
        std::fs::read_to_string(&self.dest).ok()
    }

    fn run(&self, extra: &[&str], env: &[(&str, &str)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .arg("--rule")
            .arg(repo_root().join(RULE))
            .arg("--checkout")
            .arg(&self.checkout)
            .arg("--dest")
            .arg(&self.dest)
            .arg("--owner")
            .arg("")
            .args(extra)
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.root.join("home"))
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .env_remove("http_proxy")
            .env_remove("HTTP_PROXY")
            .env_remove("https_proxy")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env("BOSS_DEPOSIT_KUBECTL", &self.kubectl)
            .env("STUB_KUBECTL_LOG", self.root.join("kubectl.log"))
            .env("STUB_SECRET", &self.secret)
            .env("BOSS_JOBS_URL", self.base())
            .env("BOSS_SOR_ENV", self.root.join("sor.env"))
            .env("BOSS_RUN_SUMMARY_FILE", &self.summary)
            .env("BOSS_NODE_ID", "forge")
            .env("BOSS_API_RETRY_DEADLINE", "0");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    fn summary(&self, key: &str) -> String {
        let text = std::fs::read_to_string(&self.summary).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        v[key].as_str().unwrap_or("").to_string()
    }

    /// No fixture token anywhere a record or a reader could see it.
    fn assert_nothing_leaked(&self, out: &str) {
        let summary = std::fs::read_to_string(&self.summary).unwrap_or_default();
        let kubectl = std::fs::read_to_string(self.root.join("kubectl.log")).unwrap_or_default();
        let bodies: String = self
            .server
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| format!("{} {} {}\n", r.method, r.path, r.body))
            .collect();
        for t in [OLD, NEW, BAD] {
            for (what, text) in [
                ("output", out),
                ("run summary", &summary),
                ("kubectl argv", &kubectl),
                ("a request", &bodies),
            ] {
                assert!(!text.contains(t), "a token reached the {what}:\n{text}");
            }
        }
    }

    fn open_rotation(&self, credential: &str, delivered: &str) -> String {
        format!(
            r#"{{"id":"c4fe0001-aaaa-bbbb-cccc-{credential:0>12.12}","kind":"rotate-a-credential","status":"open",
                "subject":{{"id":"{credential}","subject_kind":"custom"}},
                "steps":[{{"id":"step-install-{credential}","spec_slug":"install","status":"completed","metadata":{{}}}},
                         {{"id":"step-delivered-{credential}","spec_slug":"delivered","status":"{delivered}","metadata":{{}}}},
                         {{"id":"step-revoke-{credential}","spec_slug":"revoke","status":"pending","metadata":{{}}}}]}}"#
        )
    }

    fn jobs(&self, rows: &[String]) {
        *self.server.jobs.lock().unwrap() = format!(
            r#"{{"data":[{}],"total":{},"limit":500,"offset":0}}"#,
            rows.join(","),
            rows.len()
        );
    }

    /// The state the first pass leaves: file, helper, stripped remote.
    fn converted(&self, value: &str) {
        self.set_remote(&self.url(&format!("david:{value}@")));
        self.server.accept(&[value]);
        let (rc, out) = self.run(&[], &[]);
        assert_eq!(rc, 0, "the cutover pass: {out}");
        assert_eq!(self.remote(), self.url(""), "{out}");
        self.server.requests.lock().unwrap().clear();
    }
}

#[test]
fn the_first_pass_moves_the_url_credential_into_a_file_and_strips_the_remote() {
    for userinfo in [format!("david:{OLD}@"), format!("{OLD}@")] {
        let c = Case::new("cutover");
        c.set_remote(&c.url(&userinfo));
        c.server.accept(&[OLD]);
        // The broker has not run: the converge created its Secret empty,
        // or has not created it yet.
        let (rc, out) = c.run(&[], &[]);
        assert_eq!(rc, 0, "{userinfo:?}: {out}");
        assert_eq!(
            c.file().as_deref(),
            Some(OLD),
            "the value moved into the file"
        );
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&c.dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the token file is the owner's alone");
        assert_eq!(
            c.remote(),
            c.url(""),
            "the remote carries no userinfo once the helper is proved: {out}"
        );
        let helpers = c.helpers();
        assert_eq!(
            helpers.len(),
            2,
            "a reset, then the one helper: {helpers:?}"
        );
        assert_eq!(
            helpers[0], "",
            "the empty entry resets any helper configured before it"
        );
        assert!(
            helpers[1].contains(&format!("cat '{}'", c.dest.display()))
                && !helpers[1].contains(OLD),
            "the helper READS the file at use time; the value never sits in config: {helpers:?}"
        );
        let fill = c.fill();
        assert!(
            fill.contains("username=david") && fill.contains(&format!("password={OLD}")),
            "git's own credential fill for the forge URL answers from the file: {fill}"
        );
        assert_eq!(c.summary("checkout_token_last_eight"), last8(OLD), "{out}");
        assert_eq!(c.summary("deposit_secret"), "absent", "{out}");
        assert!(c.summary("deposit_action").contains("cutover"), "{out}");
        assert_eq!(c.summary("deposit_remote"), "stripped", "{out}");
        assert!(
            c.server.writes().is_empty(),
            "an empty Secret has no delivery to record"
        );
        c.assert_nothing_leaked(&out);
    }
}

#[test]
fn a_url_credential_that_does_not_verify_changes_nothing() {
    let c = Case::new("cutover-refused");
    c.set_remote(&c.url(&format!("david:{OLD}@")));
    c.server.accept(&[NEW]);
    let (rc, out) = c.run(&[], &[]);
    assert_eq!(rc, 1, "an unproved credential is a failure, named: {out}");
    assert_eq!(c.file(), None, "no file is left behind");
    assert!(
        c.helpers().is_empty(),
        "no helper is configured: {:?}",
        c.helpers()
    );
    assert_eq!(
        c.remote(),
        c.url(&format!("david:{OLD}@")),
        "the remote keeps the credential it works with today"
    );
    assert!(c.summary("deposit_action").starts_with("failed"), "{out}");
    c.assert_nothing_leaked(&out);
}

#[test]
fn a_new_secret_value_is_proved_then_installed_and_its_delivery_recorded() {
    let c = Case::new("install");
    c.converted(OLD);
    c.put_secret(NEW);
    c.server.accept(&[OLD, NEW]);
    c.jobs(&[
        c.open_rotation("forge-host-checkout-token", "ready"),
        // Another credential's rotation, also waiting: not this host's.
        c.open_rotation("boss-dev-forge-token", "ready"),
    ]);
    let (rc, out) = c.run(&[], &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        c.file().as_deref(),
        Some(NEW),
        "the Secret's value is installed"
    );
    assert!(c.fill().contains(&format!("password={NEW}")));
    assert_eq!(c.summary("checkout_token_last_eight"), last8(NEW));
    assert_eq!(c.summary("deposit_secret"), format!("held:{}", last8(NEW)));
    assert!(c.summary("deposit_action").contains("installed"), "{out}");
    let kubectl = std::fs::read_to_string(c.root.join("kubectl.log")).unwrap();
    assert!(
        kubectl.contains("-n boss get secret forge-host-checkout-token")
            && kubectl.contains("{.data.token}"),
        "the Secret named by the rule, read by name: {kubectl}"
    );

    let writes = c.server.writes();
    assert_eq!(writes.len(), 2, "one merge, one completion: {writes:?}");
    assert_eq!(writes[0].method, "PATCH");
    assert!(
        writes[0]
            .path
            .ends_with("/steps/step-delivered-forge-host-checkout-token/metadata"),
        "the delivered step of THIS credential's packet: {:?}",
        writes[0]
    );
    let merged: serde_json::Value = serde_json::from_str(&writes[0].body).unwrap();
    assert_eq!(merged["delivered_last_eight"], last8(NEW));
    assert!(
        merged["delivered_to"]
            .as_str()
            .unwrap()
            .ends_with("forge-checkout.token"),
        "{merged}"
    );
    assert_eq!(writes[1].method, "PUT");
    assert!(
        writes[1]
            .path
            .ends_with("/steps/step-delivered-forge-host-checkout-token")
    );
    let put: serde_json::Value = serde_json::from_str(&writes[1].body).unwrap();
    assert_eq!(put, serde_json::json!({ "status": "completed" }));
    assert!(
        c.summary("deposit_delivery").starts_with("recorded"),
        "{out}"
    );
    c.assert_nothing_leaked(&out);
}

/// Review F5 of car 85b7b55f (2026-09-26): with no file yet and a Secret
/// value that does not authenticate (a dead or foreign value), the pass
/// tried the Secret, failed, and never tried the URL — so the leaking
/// shape stayed, pass after pass. The Secret's failure is still named;
/// the cutover happens anyway.
#[test]
fn a_dead_secret_value_does_not_stop_the_url_cutover() {
    for userinfo in [format!("david:{OLD}@"), format!("{OLD}@")] {
        let c = Case::new("dead-secret-cutover");
        c.set_remote(&c.url(&userinfo));
        c.put_secret(BAD);
        c.server.accept(&[OLD]);
        let (rc, out) = c.run(&[], &[]);
        assert_eq!(
            rc, 1,
            "{userinfo:?}: a Secret value that does not work is still a failure, named: {out}"
        );
        assert_eq!(
            c.file().as_deref(),
            Some(OLD),
            "{userinfo:?}: the URL's value moved into the file: {out}"
        );
        assert_eq!(
            c.remote(),
            c.url(""),
            "{userinfo:?}: the leaking shape is gone: {out}"
        );
        assert!(c.fill().contains(&format!("password={OLD}")));
        let action = c.summary("deposit_action");
        assert!(
            action.contains("cutover") && action.contains("did not authenticate"),
            "{userinfo:?}: the record names both — the cutover and the dead value: {action}"
        );
        assert!(action.contains(last8(BAD)), "{action}");
        assert_eq!(c.summary("deposit_remote"), "stripped", "{out}");
        assert!(c.server.writes().is_empty(), "no delivery of a dead value");
        c.assert_nothing_leaked(&out);
    }
}

/// Review F3 of car 85b7b55f (2026-09-26): the pass read the Secret,
/// the broker then installed a new value and readied the packet's
/// `delivered` step, and the pass — finding that step ready — recorded
/// the OLD value's last eight on it. A completed step cannot be recorded
/// again and the broker refuses the mismatch, so the rotation could
/// never finish. The Secret is re-read after the packet is found, and a
/// value that moved records nothing: the next pass installs the new
/// value and records IT.
#[test]
fn a_secret_that_moves_during_the_pass_records_no_delivery() {
    let c = Case::new("secret-moved");
    c.converted(OLD);
    c.put_secret(OLD);
    let later = c.root.join("secret-later");
    write_file(&later, NEW);
    c.server.accept(&[OLD, NEW]);
    c.jobs(&[c.open_rotation("forge-host-checkout-token", "ready")]);
    let (rc, out) = c.run(&[], &[("STUB_SECRET_LATER", later.to_str().unwrap())]);
    assert_eq!(
        rc, 0,
        "a race the next pass resolves is not a failure: {out}"
    );
    assert!(
        c.server.writes().is_empty(),
        "no delivery recorded for a value the Secret no longer holds: {out}"
    );
    assert!(
        c.summary("deposit_delivery").contains("moved"),
        "{}",
        c.summary("deposit_delivery")
    );
    assert_eq!(c.file().as_deref(), Some(OLD));
    c.assert_nothing_leaked(&out);

    let (rc, out) = c.run(&[], &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(c.file().as_deref(), Some(NEW), "the next pass installs it");
    let writes = c.server.writes();
    assert_eq!(writes.len(), 2, "{out}");
    let merged: serde_json::Value = serde_json::from_str(&writes[0].body).unwrap();
    assert_eq!(merged["delivered_last_eight"], last8(NEW), "{merged}");
    c.assert_nothing_leaked(&out);
}

#[test]
fn a_secret_value_that_does_not_verify_is_not_installed_and_nothing_is_recorded() {
    let c = Case::new("install-refused");
    c.converted(OLD);
    c.put_secret(BAD);
    c.server.accept(&[OLD]);
    c.jobs(&[c.open_rotation("forge-host-checkout-token", "ready")]);
    let (rc, out) = c.run(&[], &[]);
    assert_eq!(rc, 1, "{out}");
    assert_eq!(c.file().as_deref(), Some(OLD), "the working value stays");
    assert!(c.server.writes().is_empty(), "no delivery is claimed");
    assert!(c.summary("deposit_action").starts_with("failed"), "{out}");
    let leftovers: Vec<_> = std::fs::read_dir(c.dest.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers.len(), 1, "no temp file is left: {leftovers:?}");
    c.assert_nothing_leaked(&out);
}

#[test]
fn an_unchanged_value_installs_nothing_but_a_pending_delivery_is_still_recorded() {
    let c = Case::new("unchanged");
    c.converted(OLD);
    c.put_file(NEW);
    c.put_secret(NEW);
    c.server.accept(&[NEW]);
    // Nothing waiting: a pass that changes nothing writes nothing.
    let (rc, out) = c.run(&[], &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(c.summary("deposit_action"), "unchanged", "{out}");
    assert!(c.server.writes().is_empty());
    // A delivery the last pass could not record (the API was down) is
    // recorded now — after proving the installed value once more.
    c.jobs(&[c.open_rotation("forge-host-checkout-token", "ready")]);
    let (rc, out) = c.run(&[], &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(c.server.writes().len(), 2, "{out}");
    c.assert_nothing_leaked(&out);
}

#[test]
fn two_rotations_awaiting_delivery_are_not_guessed_between() {
    let c = Case::new("two-open");
    c.converted(OLD);
    c.put_secret(NEW);
    c.server.accept(&[OLD, NEW]);
    let mut second = c.open_rotation("forge-host-checkout-token", "ready");
    second = second.replace("c4fe0001", "c4fe0002");
    c.jobs(&[
        c.open_rotation("forge-host-checkout-token", "ready"),
        second,
    ]);
    let (rc, out) = c.run(&[], &[]);
    assert_eq!(rc, 1, "{out}");
    assert_eq!(c.file().as_deref(), Some(NEW), "the install itself stands");
    assert!(
        c.server.writes().is_empty(),
        "no delivery is guessed: {out}"
    );
    assert!(c.summary("deposit_delivery").contains("2 open"), "{out}");
}

#[test]
fn an_unreadable_secret_is_named_and_touches_nothing() {
    let c = Case::new("forbidden");
    c.converted(OLD);
    let (rc, out) = c.run(&[], &[("STUB_FORBIDDEN", "1")]);
    assert_eq!(
        rc, 1,
        "a read the admin credential cannot make is a failure: {out}"
    );
    assert!(
        c.summary("deposit_secret").starts_with("unreadable"),
        "{out}"
    );
    assert!(c.summary("deposit_secret").contains("Forbidden"), "{out}");
    assert_eq!(c.file().as_deref(), Some(OLD));
    assert!(c.server.writes().is_empty());
}

#[test]
fn a_remote_target_is_refused_until_the_break_glass_delivery_builds_it() {
    let c = Case::new("remote-target");
    let (rc, out) = c.run(&["--target", "boss-gcp"], &[]);
    assert_eq!(rc, 78, "{out}");
    assert!(
        out.contains("7336cb5f"),
        "the refusal names the item that builds it: {out}"
    );
}

#[test]
fn a_run_as_this_user_refuses_without_an_isolated_git_config() {
    let c = Case::new("inline-guard");
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(SCRIPT))
        .args(["--rule", &repo_root().join(RULE).display().to_string()])
        .args(["--checkout", &c.checkout.display().to_string()])
        .args(["--dest", &c.dest.display().to_string()])
        .args(["--owner", ""])
        .env_remove("GIT_CONFIG_GLOBAL")
        .env("HOME", c.root.join("home"));
    let out = cmd.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(78),
        "without --owner the script would write THIS user's global git config: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Review A1 of the re-review of 5ef6db0b (2026-09-26): the deposit runs
/// as root and read `$DEST` with `$(<"$DEST")`, but the file and the
/// directory holding it are the OWNER's. A symlink planted there — to
/// /etc/boss-ops/kubeconfig, say — was read as root, and its last eight
/// published on the converge packet as the checkout token's. A symlink
/// is now refused by name and never read, and the file is read as the
/// owner, who cannot read what they could not already; a Secret value
/// still installs, replacing the link with the deposit's own file.
#[test]
fn a_symlinked_token_file_is_never_read() {
    const CANARY: &str = "kubeconfig-canary-not-a-token-9876fedcba";
    for secret in [None, Some(NEW)] {
        let c = Case::new("symlinked-file");
        c.converted(OLD);
        let target = c.root.join("root-only-kubeconfig");
        write_file(&target, CANARY);
        std::fs::remove_file(&c.dest).unwrap();
        std::os::unix::fs::symlink(&target, &c.dest).unwrap();
        c.server.accept(&[OLD, NEW]);
        if let Some(v) = secret {
            c.put_secret(v);
        }
        let (rc, out) = c.run(&[], &[]);
        assert_eq!(rc, 1, "{secret:?}: a planted symlink reds the run: {out}");
        let summary = std::fs::read_to_string(&c.summary).unwrap_or_default();
        for text in [&out, &summary] {
            assert!(
                !text.contains(CANARY) && !text.contains(last8(CANARY)),
                "{secret:?}: nothing read through the link reaches a record: {text}"
            );
        }
        assert!(
            c.summary("deposit_action").contains("symlink"),
            "{secret:?}: the refusal names what it saw: {}",
            c.summary("deposit_action")
        );
        match secret {
            None => assert!(
                std::fs::symlink_metadata(&c.dest)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "with nothing to install the link is left for a human to see"
            ),
            Some(v) => {
                let meta = std::fs::symlink_metadata(&c.dest).unwrap();
                assert!(meta.file_type().is_file(), "the install replaced the link");
                assert_eq!(c.file().as_deref(), Some(v));
                assert_eq!(std::fs::read_to_string(&target).unwrap(), CANARY);
            }
        }
        c.assert_nothing_leaked(&out);
    }
}

#[test]
fn every_act_on_the_owners_files_runs_as_the_owner() {
    let c = Case::new("as-owner");
    c.set_remote(&c.url(&format!("david:{OLD}@")));
    c.server.accept(&[OLD]);
    let bin = c.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = c.root.join("runuser.log");
    // `runuser -u <owner> -- <argv>`: record the owner, run the argv.
    write_exec(
        &bin.join("runuser"),
        &format!(
            "#!/usr/bin/env bash\n[ \"$1\" = -u ] && [ \"$3\" = -- ] || {{ echo \"runuser shape: $*\" >&2; exit 97; }}\necho \"$2 $4\" >> '{}'\nshift 3\nexec \"$@\"\n",
            log.display()
        ),
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(SCRIPT))
        .args(["--rule", &repo_root().join(RULE).display().to_string()])
        .args(["--checkout", &c.checkout.display().to_string()])
        .args(["--dest", &c.dest.display().to_string()])
        .args(["--owner", "someone"])
        .env("PATH", path)
        .env("GIT_CONFIG_GLOBAL", &c.gitconfig)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", c.root.join("home"))
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1")
        .env_remove("http_proxy")
        .env_remove("HTTP_PROXY")
        .env("BOSS_DEPOSIT_KUBECTL", &c.kubectl)
        .env("STUB_KUBECTL_LOG", c.root.join("kubectl.log"))
        .env("STUB_SECRET", &c.secret)
        .env("BOSS_SOR_ENV", c.root.join("sor.env"))
        .env_remove("BOSS_JOBS_URL");
    let out = cmd.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0), "{text}");
    let ran = std::fs::read_to_string(&log).unwrap_or_default();
    // `cat`: the token file is READ as the owner too (review A1) — root
    // reading an owner-writable path is how a planted link became a read.
    for verb in ["git", "sh", "mkdir", "cat"] {
        assert!(
            ran.lines().any(|l| l == format!("someone {verb}")),
            "`{verb}` runs as the owner — a root-owned file or config in the owner's \
             home breaks the owner's own git: {ran}"
        );
    }
    assert!(
        ran.lines().all(|l| l.starts_with("someone ")),
        "nothing runs as anyone else: {ran}"
    );
    assert_eq!(c.file().as_deref(), Some(OLD));
    c.assert_nothing_leaked(&text);
}

/// The consumers move with the credential (D2): once the remote carries
/// no userinfo, a consumer that derives its URL from it — here the
/// tenant-source check the cluster converge runs as the owner
/// (cluster-deploy-lib.sh) — authenticates through the owner's helper
/// alone. Measured against the same forge, which 401s anything else.
#[test]
fn after_the_cutover_the_tenant_check_authenticates_through_the_helper() {
    let c = Case::new("tenant-check");
    c.converted(OLD);
    let script = format!(
        ". '{}'\ntenant_source_check '{}' david/boss main\n",
        repo_root()
            .join("infra/forge/cluster-deploy-lib.sh")
            .display(),
        c.checkout.display()
    );
    let out = c
        .git_cmd_as("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{text}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "1111111111111111111111111111111111111111",
        "the stripped URL read the tenant's ref through the file: {text}"
    );
    // Control: the same check with the helper gone is refused by the
    // forge — so the answer above came from the helper, not from an
    // unauthenticated forge.
    c.git_cmd()
        .args([
            "config",
            "--global",
            "--unset-all",
            &format!("credential.{}.helper", c.base()),
        ])
        .output()
        .unwrap();
    let out = c
        .git_cmd_as("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .unwrap();
    assert!(!out.status.success(), "no helper, no read");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains(OLD),
        "the refusal names no token"
    );
}

#[test]
fn the_forge_converge_deposits_before_it_fetches_and_reads_protect_mains_header_from_the_file() {
    let converge = std::fs::read_to_string(repo_root().join(CONVERGE)).expect("forge-converge.sh");
    let code: Vec<&str> = converge
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    let at = |needle: &str| {
        code.iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("forge-converge.sh has no line running {needle:?}"))
    };
    let deposit = at("credential-deposit.sh");
    let fetch = at("fetch -q forgejo main");
    assert!(
        deposit < fetch,
        "the deposit runs BEFORE the fetch: it owes nothing to the forge token, so a \
         revoked or broken one cannot stop the step that repairs it"
    );
    assert!(
        code[deposit..]
            .join("\n")
            .contains(RULE.trim_start_matches("infra/")),
        "the deposit is handed the broker rule, the one declaration of the Secret"
    );
    assert!(
        !code.iter().any(|l| l.contains("credential fill")),
        "no git call asks for the credential of a URL any more — that is the call whose \
         error message printed the token into the journal"
    );
    assert!(
        code.iter()
            .any(|l| l.contains("forge-checkout.token") && !l.contains("credential-deposit")),
        "protect-main's header is read from the token file"
    );
    // The header is written from the file and from nothing else. #697's
    // forge-auth-header.sh read it off the remote URL's userinfo, which
    // the deposit strips — after the cutover it had nothing to read and
    // would have exited 3 on every run (backlog 85f8a614), so this car
    // deleted it.
    // Every line naming the header file, less its creation, its mode,
    // its cleanup and its one reader.
    let header_writes: Vec<&&str> = code
        .iter()
        .filter(|l| l.contains("$auth_hdr"))
        .filter(|l| {
            ![
                "mktemp",
                "chmod 600",
                "trap ",
                "BOSS_FORGE_AUTH_HEADER_FILE=",
            ]
            .iter()
            .any(|known| l.contains(known))
        })
        .collect();
    assert_eq!(
        header_writes.len(),
        1,
        "one write of protect-main's header: {header_writes:?}"
    );
    // The token file is the owner's and this script runs as root, so root
    // never reads it (review A1 of the re-review of 5ef6db0b): a link
    // planted there to /etc/boss-ops/kubeconfig would go to the forge in
    // this header. A symlink gets no header at all, and the file is read
    // AS THE OWNER, who cannot read what they could not already.
    let reads: Vec<&&str> = code
        .iter()
        .filter(|l| l.contains("$FORGE_TOKEN_FILE"))
        .filter(|l| !l.contains("FORGE_TOKEN_FILE=") && !l.contains("--dest"))
        .collect();
    assert!(
        reads.iter().any(|l| l.contains("-L \"$FORGE_TOKEN_FILE\"")),
        "a symlinked token file is refused before any read: {reads:?}"
    );
    assert!(
        reads
            .iter()
            .any(|l| l.contains("runuser -u \"$OWNER\" -- cat -- \"$FORGE_TOKEN_FILE\"")),
        "the token file is read as its owner: {reads:?}"
    );
    assert!(
        !reads
            .iter()
            .any(|l| l.contains("$(<\"$FORGE_TOKEN_FILE\")")),
        "root never reads the owner's file itself: {reads:?}"
    );
    assert!(
        header_writes[0].contains("$FORGE_TOKEN\""),
        "the header carries the value read as the owner: {header_writes:?}"
    );
    assert!(
        !code.iter().any(|l| l.contains("forge-auth-header")),
        "nothing reads the credential off the remote URL any more"
    );
    assert!(
        !repo_root()
            .join("infra/forge/forge-auth-header.sh")
            .exists(),
        "the URL-userinfo reader is gone with its only caller"
    );
}
