//! `infra/surface-usage.sh` — which surfaces each operator opened is
//! ROLLED UP daily and FILED, with its coverage stated (backlog
//! 628f182b; David, 2026-09-16: "let's measure which surfaces I open
//! for a week").
//!
//! The stub below is the jobs API as the script sees it: it serves the
//! roll-up (`/api/surface-opens/rollup`), the sweep, the open packet
//! (`/api/jobs?kind=…`), and records the PATCH — so the test reads the
//! filed row, not the script's account of it. The roll-up fixture names
//! two actors and three routes, one of them a catalogued path; the
//! never-opened list must then be the catalog minus that one path, read
//! off THIS tree's nav catalog.

use boss_testing::announce::{await_announced_port, with_announce};
use boss_testing::repo_root;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const JOB_ID: &str = "0123456789abcdef";

/// "I could not answer." `infra/lint/lib/git-answer.sh` carries the
/// argument; here it means the roll-up or the catalog was not read.
const CANNOT_ANSWER: i32 = 3;

const STUB: &str = r#"
import http.server, json, sys, socket
log, started, mode = sys.argv[1], sys.argv[2], sys.argv[3]
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.bind(("127.0.0.1", 0))
port = sock.getsockname()[1]
OPEN = {"data": [{"id": "0123456789abcdef", "kind": "maintenance-surface-usage", "status": "open", "created_at": "2026-09-17T05:30:35Z", "metadata": {}}], "total": 1}
ROLLUP = {"since": "2026-09-16T05:30:00Z", "until": "2026-09-17T05:30:00Z", "rows": [
  {"actor_id": "emp-david", "route": "/it", "opens": 7, "last_at": "2026-09-17T01:00:00Z"},
  {"actor_id": "emp-david", "route": "/ux/jobs/:jobId", "opens": 3, "last_at": "2026-09-17T00:30:00Z"},
  {"actor_id": "emp-032", "route": "/ux/me", "opens": 2, "last_at": "2026-09-16T09:00:00Z"}
]}
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _reply(self, code, body=b"{}"):
        self.send_response(code); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        with open(log, "ab") as f:
            f.write(b"GET " + self.path.encode() + b"\n")
        if self.path.startswith("/api/surface-opens/rollup"):
            if mode == "rollup-down":
                self._reply(503, b'{"error":"upstream unavailable"}')
            elif "since=" not in self.path or "until=" not in self.path:
                self._reply(400, b'{"error":"since and until are required"}')
            else:
                self._reply(200, json.dumps(ROLLUP).encode())
        elif self.path.startswith("/api/jobs"):
            self._reply(200, json.dumps(OPEN).encode())
        else:
            self._reply(404, b'{"error":"no such route"}')
    def do_POST(self):
        n = int(self.headers.get("content-length", "0"))
        self.rfile.read(n)
        with open(log, "ab") as f:
            f.write(b"POST " + self.path.encode() + b"\n")
        if self.path == "/api/surface-opens/sweep":
            if mode == "sweep-down":
                self._reply(500, b'{"error":"storage: boom"}')
            else:
                self._reply(200, b'{"deleted": 4, "before": "2026-08-18T05:30:00Z", "retention_days": 30}')
        else:
            self._reply(404, b'{"error":"no such route"}')
    def do_PATCH(self):
        n = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(n)
        try:
            line = json.dumps(json.loads(body), separators=(",", ":")).encode()
        except Exception as e:
            line = b"NOT JSON: " + repr(e).encode()
        with open(log, "ab") as f:
            f.write(b"PATCH " + self.path.encode() + b"\n" + line + b"\n")
        self._reply(204, b"")
srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H, bind_and_activate=False)
srv.socket.close(); srv.socket = sock; srv.server_address = sock.getsockname(); srv.server_activate()
# Announced once listening, and through boss_testing's announce (prepended),
# which renames the file into place: the test reads it the moment it
# exists (backlog 0d1e557e).
announce(started, str(port))
srv.serve_forever()
"#;

fn script() -> PathBuf {
    repo_root().join("infra/surface-usage.sh")
}

/// Every `path:` the tree's nav catalog declares, once each — the
/// roster the script's never-opened list is judged against, read here
/// the same way the script reads it so the two cannot drift.
fn catalog_paths() -> Vec<String> {
    let src = std::fs::read_to_string(repo_root().join("apps/web/src/shell/nav-catalog.ts"))
        .expect("apps/web/src/shell/nav-catalog.ts");
    let mut out: Vec<String> = Vec::new();
    for line in src.lines() {
        if let Some(rest) = line.split("path: '").nth(1)
            && let Some(p) = rest.split('\'').next()
            && !out.iter().any(|x| x == p)
        {
            out.push(p.to_string());
        }
    }
    out
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
impl Stub {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
    fn patches(&self) -> Vec<(String, serde_json::Value)> {
        let log = self.log();
        let mut lines = log.lines();
        let mut out = Vec::new();
        while let Some(l) = lines.next() {
            if let Some(path) = l.strip_prefix("PATCH ") {
                let body = lines.next().expect("a PATCH line is followed by its body");
                out.push((
                    path.to_string(),
                    serde_json::from_str(body).expect("the PATCH body is JSON"),
                ));
            }
        }
        out
    }
}

fn start_stub(case: &str, mode: &str) -> Stub {
    let dir = boss_testing::scratch_dir(&format!("surface-usage-stub-{case}"));
    let script = dir.join("stub.py");
    let started = dir.join("started");
    let log = dir.join("calls.log");
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_file(&started);
    boss_testing::write_exec(&script, &with_announce(STUB));
    let mut child = Command::new("python3")
        .arg(&script)
        .arg(&log)
        .arg(&started)
        .arg(mode)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("python3");
    let port = await_announced_port(&mut child, &started, Duration::from_secs(20));
    Stub { child, port, log }
}

fn run(stub: &Stub, cmd: &str, repo: &std::path::Path) -> (Option<i32>, String, String) {
    let out = Command::new("bash")
        .arg(script())
        .arg(cmd)
        .arg("--repo")
        .arg(repo)
        .env("BOSS_JOBS_URL", format!("http://127.0.0.1:{}", stub.port))
        .env("BOSS_API_RETRY_DEADLINE", "5")
        .output()
        .expect("bash");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn has_python3() -> bool {
    Command::new("python3").arg("--version").output().is_ok()
}

/// `file` reads the roll-up over a 24-hour window, sweeps retention,
/// and PATCHes ONE row — `measured` + `coverage`, the chore contract's
/// two required keys — onto the open packet: opens per actor per
/// route, the top routes, and the catalogued surfaces nobody opened.
#[test]
fn file_patches_measured_and_coverage_onto_the_open_packet() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("file", "serve");
    let (rc, out, err) = run(&stub, "file", &repo_root());
    assert_eq!(rc, Some(0), "file did not exit 0:\n{out}\n{err}");

    let patches = stub.patches();
    assert_eq!(
        patches.len(),
        1,
        "exactly one PATCH is the record; got {} in:\n{}",
        patches.len(),
        stub.log()
    );
    let (path, body) = &patches[0];
    assert_eq!(path, &format!("/api/jobs/{JOB_ID}/metadata"));
    let keys: Vec<&str> = body
        .as_object()
        .expect("the body is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["coverage", "measured"],
        "the chore contract's two keys (14c135f5), merged server-side: {body}"
    );

    let m = &body["measured"];
    assert_eq!(m["opens"].as_i64(), Some(12), "7 + 3 + 2: {m}");
    assert_eq!(m["actors"].as_i64(), Some(2), "{m}");
    assert_eq!(m["distinct_routes"].as_i64(), Some(3), "{m}");
    assert!(m["at"].as_str().is_some_and(|s| s.ends_with('Z')), "{m}");
    assert!(m["method"].is_object(), "the method rides on the row: {m}");

    // Per actor, busiest first; routes most-opened first.
    let per = m["per_actor"].as_array().expect("per_actor is an array");
    assert_eq!(per.len(), 2, "{m}");
    assert_eq!(per[0]["actor_id"], "emp-david");
    assert_eq!(per[0]["opens"].as_i64(), Some(10));
    assert_eq!(per[0]["distinct_routes"].as_i64(), Some(2));
    assert_eq!(per[0]["routes"][0]["route"], "/it");
    assert_eq!(per[0]["routes"][0]["opens"].as_i64(), Some(7));
    assert_eq!(per[0]["routes"][1]["route"], "/ux/jobs/:jobId");
    assert_eq!(per[1]["actor_id"], "emp-032");

    let top = m["top_routes"].as_array().expect("top_routes is an array");
    assert_eq!(top[0]["route"], "/it");
    assert_eq!(top[0]["opens"].as_i64(), Some(7));

    // The deletion candidates: this tree's catalog minus whatever the
    // fixture opened that the catalog lists — `/it` today; `/ux/me` is
    // not a catalog entry (My Day is the personal fallback, not a
    // sidebar row) and `/ux/jobs/:jobId` is a pattern, not a path. The
    // filter names both anyway so the test holds if either joins.
    let catalog = catalog_paths();
    assert!(
        catalog.len() > 20,
        "the catalog read {} paths",
        catalog.len()
    );
    let expected: Vec<&str> = catalog
        .iter()
        .map(String::as_str)
        .filter(|p| *p != "/it" && *p != "/ux/me")
        .collect();
    let never: Vec<&str> = m["never_opened"]
        .as_array()
        .expect("never_opened is an array")
        .iter()
        .map(|v| v.as_str().expect("a path"))
        .collect();
    assert_eq!(
        never, expected,
        "never_opened is the catalog minus what was opened: {m}"
    );
    assert_eq!(
        m["never_opened_count"].as_u64(),
        Some(expected.len() as u64),
        "{m}"
    );
    assert_eq!(
        m["catalog_routes"].as_u64(),
        Some(catalog.len() as u64),
        "{m}"
    );

    // The sweep's facts, not that one happened.
    assert_eq!(m["retention"]["swept"]["deleted"].as_i64(), Some(4), "{m}");
    assert_eq!(
        m["retention"]["swept"]["retention_days"].as_i64(),
        Some(30),
        "{m}"
    );
    assert!(m["retention"]["error"].is_null(), "{m}");

    // COVERAGE: what was examined, and that it was all of it.
    let c = &body["coverage"];
    assert_eq!(c["window"]["hours"].as_i64(), Some(24), "{c}");
    assert!(
        c["window"]["since"]
            .as_str()
            .is_some_and(|s| s.ends_with('Z')),
        "{c}"
    );
    assert!(
        c["window"]["until"]
            .as_str()
            .is_some_and(|s| s.ends_with('Z')),
        "{c}"
    );
    assert_eq!(
        c["rollup"]["source"],
        format!("http://127.0.0.1:{}/api/surface-opens/rollup", stub.port),
        "the row names the surface it read: {c}"
    );
    assert_eq!(c["rollup"]["rows"].as_i64(), Some(3), "{c}");
    assert_eq!(
        c["catalog"]["file"], "apps/web/src/shell/nav-catalog.ts",
        "{c}"
    );
    assert_eq!(
        c["catalog"]["routes"].as_u64(),
        Some(catalog.len() as u64),
        "{c}"
    );
    assert_eq!(c["complete"], true, "{c}");

    // The roll-up was asked for a bounded window and the sweep ran.
    let log = stub.log();
    assert!(
        log.contains("GET /api/surface-opens/rollup?since=") && log.contains("&until="),
        "the roll-up was not read over a stated window:\n{log}"
    );
    assert!(
        log.contains("POST /api/surface-opens/sweep"),
        "retention was not swept:\n{log}"
    );

    // THE JOURNAL GETS THE HEADLINE, not a digest that hides the counts.
    assert!(
        out.contains(&format!("filed on {}", &JOB_ID[..8])),
        "stdout does not name the packet:\n{out}"
    );
    assert!(
        out.contains("12 open(s) by 2 actor(s) across 3 route(s)"),
        "the headline does not carry the totals:\n{out}"
    );
    assert!(
        out.contains("emp-david: 10 open(s), 2 route(s)"),
        "each actor gets a line:\n{out}"
    );
    assert!(
        out.contains("swept 4 row(s)"),
        "the sweep's count is in the headline:\n{out}"
    );
}

/// `row` is the same reading printed, for an operator asking by hand —
/// it reads the roll-up, touches no packet, and SWEEPS NOTHING.
#[test]
fn row_prints_the_reading_and_neither_files_nor_sweeps() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("row", "serve");
    let (rc, out, err) = run(&stub, "row", &repo_root());
    assert_eq!(rc, Some(0), "row did not exit 0:\n{err}");
    let row: serde_json::Value = serde_json::from_str(&out).expect("row prints JSON");
    assert_eq!(row["measured"]["opens"].as_i64(), Some(12));
    assert!(
        row["measured"]["retention"].is_null(),
        "row does not sweep: {row}"
    );
    assert!(row["coverage"]["complete"] == true);
    assert!(
        stub.patches().is_empty(),
        "row filed something:\n{}",
        stub.log()
    );
    let log = stub.log();
    assert!(
        !log.contains("/api/jobs"),
        "row read the packet queue:\n{log}"
    );
    assert!(!log.contains("sweep"), "row swept rows:\n{log}");
}

/// A roll-up that cannot be read is an infrastructure refusal (exit 3)
/// and NOTHING is filed: a row from no read would say "nobody opened
/// anything", which is the confident wrong answer the chore contract
/// exists to refuse (14c135f5: a run that cannot state its coverage
/// FAILS).
#[test]
fn an_unreadable_rollup_files_nothing_and_exits_cannot_answer() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("rollup-down", "rollup-down");
    let (rc, out, err) = run(&stub, "file", &repo_root());
    assert_eq!(rc, Some(CANNOT_ANSWER), "stdout:\n{out}\nstderr:\n{err}");
    assert!(err.contains("CANNOT ANSWER"), "{err}");
    assert!(
        err.contains("rollup"),
        "the refusal names what it could not read:\n{err}"
    );
    assert!(
        stub.patches().is_empty(),
        "something was filed:\n{}",
        stub.log()
    );
    assert!(
        !stub.log().contains("sweep"),
        "a run that read nothing must not sweep:\n{}",
        stub.log()
    );
}

/// A tree whose nav catalog yields no paths has no roster to judge
/// against — refused the same way, never filed as "every surface was
/// opened".
#[test]
fn an_unreadable_catalog_is_refused_not_an_empty_never_opened_list() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("no-catalog", "serve");
    let repo = boss_testing::scratch_dir("surface-usage-no-catalog");
    std::fs::create_dir_all(repo.join("apps/web/src/shell")).unwrap();
    std::fs::write(
        repo.join("apps/web/src/shell/nav-catalog.ts"),
        "// a catalog with no entries\nexport const ROUTE_CATALOG = {};\n",
    )
    .unwrap();
    let (rc, out, err) = run(&stub, "file", &repo);
    assert_eq!(rc, Some(CANNOT_ANSWER), "stdout:\n{out}\nstderr:\n{err}");
    assert!(err.contains("no nav paths"), "{err}");
    assert!(
        stub.patches().is_empty(),
        "something was filed:\n{}",
        stub.log()
    );
}

/// A sweep that fails is loud AFTER the reading is on the packet: the
/// row carries the reason under `measured.retention.error`, the run
/// exits 1, and the measurement is never lost to the housekeeping.
#[test]
fn a_failed_sweep_still_files_the_reading_and_exits_nonzero() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("sweep-down", "sweep-down");
    let (rc, out, err) = run(&stub, "file", &repo_root());
    assert_eq!(rc, Some(1), "stdout:\n{out}\nstderr:\n{err}");
    let patches = stub.patches();
    assert_eq!(
        patches.len(),
        1,
        "the reading was not filed:\n{}",
        stub.log()
    );
    let m = &patches[0].1["measured"];
    assert_eq!(m["opens"].as_i64(), Some(12), "{m}");
    assert!(m["retention"]["swept"].is_null(), "{m}");
    assert!(
        m["retention"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("sweep")),
        "the row says why retention was not swept: {m}"
    );
    assert!(
        out.contains("NOT swept"),
        "the headline says so too:\n{out}"
    );
}
