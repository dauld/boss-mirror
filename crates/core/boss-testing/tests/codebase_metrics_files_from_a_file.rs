//! `codebase-metrics.sh file` hands the jobs API its measurement from a
//! FILE, never as one command-line argument.
//!
//! Measured 2026-09-13 and 2026-09-14, 05:14Z on boss-gcp: the daily
//! run computed its row — 497 landings back to June, the whole first-
//! parent history, because no measured packet existed yet to start the
//! window from — and died at the PATCH with `boss-api-curl.sh: Argument
//! list too long`: the JSON body was passed as `-d "$body"`, one argv
//! string, and jq's pretty-printed row is past Linux's 128 KiB cap on a
//! single argument. Two days of `exit_status: 1` with the measurement
//! dumped to a journal nobody read, an estate alarm, and a cadence
//! alarm — for a body that should have gone through `-d @file`.
//!
//! The stub API below records the PATCH it receives; the fixture repo
//! carries enough landings that the row is well past 128 KiB.

use boss_testing::repo_root;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

const STUB: &str = r#"
import http.server, json, sys, socket
log = sys.argv[1]; started = sys.argv[2]
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.bind(("127.0.0.1", 0))
port = sock.getsockname()[1]
with open(started, "w") as f:
    f.write(str(port))
OPEN = {"data": [{"id": "0123456789abcdef", "kind": "maintenance-codebase-metrics", "status": "open", "created_at": "2026-09-14T05:14:35Z", "metadata": {}}], "total": 1}
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _reply(self, code, body=b"{}"):
        self.send_response(code); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        self._reply(200, json.dumps(OPEN).encode())
    def do_PATCH(self):
        n = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(n)
        with open(log, "ab") as f:
            f.write(self.path.encode() + b"\n" + body + b"\n")
        self._reply(204, b"")
srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H, bind_and_activate=False)
srv.socket.close(); srv.socket = sock; srv.server_address = sock.getsockname(); srv.server_activate()
srv.serve_forever()
"#;

fn git(dir: &PathBuf, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", "2026-06-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-06-01T00:00:00Z")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repo whose first-parent history yields a row past 128 KiB: 900
/// empty landings with long subjects (each landing is one row of the
/// series, ~250 bytes pretty-printed).
fn big_repo(case: &str) -> PathBuf {
    let dir = boss_testing::scratch_dir(&format!("codebase-metrics-body-{case}"));
    git(&dir, &["init", "-q", "-b", "main", "."]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::create_dir_all(dir.join("crates/core/boss-x/src")).unwrap();
    std::fs::write(
        dir.join("crates/core/boss-x/src/lib.rs"),
        "pub fn one() -> u32 {\n    1\n}\n",
    )
    .unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    let subject = "train: 2026-06-01 00:00 (3 changes) — a landing whose subject is long enough to matter (#0000)";
    for i in 0..900 {
        git(
            &dir,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                &format!("{subject} {i}"),
            ],
        );
    }
    dir
}

struct Stub {
    child: Child,
    port: u16,
    log: PathBuf,
}
impl Drop for Stub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_stub(case: &str) -> Stub {
    let dir = boss_testing::scratch_dir(&format!("codebase-metrics-stub-{case}"));
    let script = dir.join("stub.py");
    let started = dir.join("started");
    let log = dir.join("patches.log");
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_file(&started);
    boss_testing::write_exec(&script, STUB);
    let child = Command::new("python3")
        .arg(&script)
        .arg(&log)
        .arg(&started)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("python3");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !started.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the stub never announced its port"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let port: u16 = std::fs::read_to_string(&started)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    Stub { child, port, log }
}

#[test]
fn a_row_past_the_argv_cap_is_filed_whole_from_a_file() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipping: no python3");
        return;
    }
    let repo = big_repo("e2big");
    let stub = start_stub("e2big");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/codebase-metrics.sh"))
        .args(["file", "--repo"])
        .arg(&repo)
        .args(["--ref", "main"])
        .env("BOSS_JOBS_URL", format!("http://127.0.0.1:{}", stub.port))
        .env("BOSS_TRUNK_REF", "main")
        .env(
            "BOSS_LEAKED_POLICY_BIN",
            env!("CARGO_BIN_EXE_boss-leaked-policy"),
        )
        .env("BOSS_API_RETRY_DEADLINE", "5")
        .output()
        .expect("bash");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("Argument list too long"),
        "the measurement still travels as one argument:\n{err}"
    );
    assert!(
        out.status.success(),
        "file exited {:?}:\n{err}",
        out.status.code()
    );
    let log = std::fs::read_to_string(&stub.log).unwrap_or_default();
    let (path, body) = log.split_once('\n').expect("one PATCH recorded");
    assert_eq!(path, "/api/jobs/0123456789abcdef/metadata");
    assert!(
        body.len() > 131_072,
        "the fixture must exceed the 128 KiB argv cap to prove anything: {} bytes",
        body.len()
    );
    let v: serde_json::Value =
        serde_json::from_str(body.trim()).expect("the PATCH body is the row's JSON, whole");
    assert_eq!(
        v["landings"].as_array().map(Vec::len),
        Some(901),
        "every landing arrived"
    );
    assert!(v["measured"].is_object());
}
