//! A gate that finishes while the system of record is rolling must not
//! lose its verdict.
//!
//! THE FAILURE THIS PINS (backlog 23188cc5, four instances on the night
//! of 2026-09-07). The runner reports its verdict to the gate-run packet
//! ONCE, at the end of a 10–40 minute run. The SoR Recreate-rolls for
//! tens of seconds on every train converge, and every merge now
//! converges within a minute of landing — so any gate whose last minute
//! overlaps a roll made its single report-back call into a dark API,
//! logged `WARN: verdict not recorded`, and exited 0. The Job read
//! Complete, the packet never closed, auto-park never fired, and each
//! green was re-proven by hand at ~10 minutes of cluster time apiece.
//!
//! WHY THIS TEST EXECUTES THE BLOCK INSTEAD OF READING IT. The sibling
//! `run_sh_verdict.rs` asserts on run.sh's TEXT, which pins wording but
//! cannot tell whether a retry loop retries. So the report-back block is
//! lifted out of `run.sh` exactly as it ships and run under bash against
//! a stub SoR that refuses N times and then accepts — the way the real
//! one behaves across a roll.
//!
//! Skips rather than fails when `python3` or `curl` is absent, so a
//! machine without them does not manufacture a red.

use boss_testing::repo_root;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PACKET: &str = "11111111-2222-4333-8444-555555555555";
const BEGIN: &str = "# --- report-back (begin) ---";
const END: &str = "# --- report-back (end) ---";

/// The report-back block, lifted out of `run.sh` verbatim. It lives
/// inline in the runner because the pod receives exactly one file (the
/// `gate-runner-script` ConfigMap is built from `run.sh` alone).
fn report_block() -> String {
    let run_sh = repo_root().join("infra/gate-runner/run.sh");
    let src = std::fs::read_to_string(&run_sh)
        .unwrap_or_else(|e| panic!("reading {}: {e}", run_sh.display()));
    let start = src.find(BEGIN).unwrap_or_else(|| {
        panic!("run.sh has no `{BEGIN}` marker — the report-back block must be bracketed so it can be tested as it ships")
    });
    let end = src
        .find(END)
        .unwrap_or_else(|| panic!("run.sh has no `{END}` marker"));
    assert!(start < end, "report-back markers are out of order");
    src[start..end].to_string()
}

fn missing(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
}

/// The stub system of record. `mode`:
///   `503:N`  — answer 503 to the first N requests (an ingress with no
///              endpoints mid-roll), then serve;
///   `never`  — 503 forever;
///   `409`    — serve the GET, refuse every PUT with 409 (a frozen step).
/// `delay` seconds pass before it LISTENS, so a curl in that window
/// gets connection refused — the bare-Service shape of a roll.
///
/// It announces two facts by writing the files it is told to, because
/// they are the two the test has to wait on and cannot see from outside:
/// `started` before the delay, and `bound` once its listener exists.
const STUB: &str = r#"
import http.server, json, sys, time
port, log, mode, delay = int(sys.argv[1]), sys.argv[2], sys.argv[3], float(sys.argv[4])
JOB, started_at, bound_at = sys.argv[5], sys.argv[6], sys.argv[7]

def announce(path):
    with open(path, "w") as f:
        f.write("ok")

# `started` goes out BEFORE the delay, so a test that wants a dark
# window gets `delay` seconds of one rather than `delay` minus however
# long python took to boot.
announce(started_at)
time.sleep(delay)
seen = {"n": 0}
refuse_first = int(mode.split(":")[1]) if mode.startswith("503:") else 0

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass
    def _reply(self, code, body=b"{}"):
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def _rolling(self):
        seen["n"] += 1
        if mode == "never" or (refuse_first and seen["n"] <= refuse_first):
            self._reply(503, b"rolling")
            return True
        return False
    def do_GET(self):
        if self._rolling():
            return
        if self.path == "/api/jobs/" + JOB:
            self._reply(200, json.dumps({"id": JOB, "status": "open", "steps": [
                {"id": "step-gate", "spec_slug": "gate"},
                {"id": "step-verdict", "spec_slug": "record-verdict"},
            ]}).encode())
        else:
            self._reply(404)
    def do_PUT(self):
        if self._rolling():
            return
        n = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(n).decode()
        with open(log, "a") as f:
            f.write(self.path + " " + body + "\n")
        if mode == "409":
            self._reply(409, b'{"error":"step is completed"}')
        else:
            self._reply(200)

# The constructor is where bind() and listen() both happen: past this
# line a connection to the port can no longer be refused, which is
# exactly the fact `start_stub` waits for.
srv = http.server.ThreadingHTTPServer(("127.0.0.1", port), H)
announce(bound_at)
srv.serve_forever()
"#;

struct Stub {
    child: Child,
    port: u16,
    dir: PathBuf,
}

impl Drop for Stub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Stub {
    /// Wait for a fact the stub announces by writing a file. It fails
    /// fast when the stub is already gone — a port taken in the window
    /// in `start_stub` dies on EADDRINUSE, and the stub's stderr is
    /// inherited — and every panic here names the FIXTURE, so "the stub
    /// never bound" can never be read as "the report never landed".
    fn await_fact(&mut self, marker: &Path, limit: Duration, fact: &str) {
        let deadline = Instant::now() + limit;
        while !marker.exists() {
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!("{fact}: the stub exited early ({status}); its stderr is above");
            }
            assert!(
                Instant::now() < deadline,
                "{fact}: no {} after {limit:?}",
                marker.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn puts(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("puts.log"))
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gate-report-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn start_stub(tag: &str, mode: &str, delay_secs: f32) -> Stub {
    let dir = scratch(tag);
    // A free port, released before the stub binds it. The window is
    // microseconds and the tests run one stub each; if something does
    // take it, python dies on EADDRINUSE and the wait below says so by
    // name rather than leaving a dark stub behind.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let script = dir.join("stub.py");
    let started = dir.join("started");
    let bound = dir.join("bound");
    std::fs::write(&script, STUB).expect("write stub");
    let child = Command::new("python3")
        .arg(&script)
        .arg(port.to_string())
        .arg(dir.join("puts.log"))
        .arg(mode)
        .arg(delay_secs.to_string())
        .arg(PACKET)
        .arg(&started)
        .arg(&bound)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("python3 runs");
    let mut stub = Stub { child, port, dir };

    // READINESS IS A FACT THE STUB STATES, NOT A CONNECT THAT SUCCEEDED.
    //
    // This used to poll `TcpStream::connect` and take the first success
    // as "up". A connect can succeed with nothing listening — a
    // loopback self-connect, when the kernel hands the probe the same
    // ephemeral port it is dialling, or a foreign listener that took the
    // port in the window above — and when it does, the test walks into a
    // dark stub. That cost train 605f2364 a red at 03:23 UTC on
    // 2026-09-10: four attempts spent the whole retry budget on `curl
    // exit 7`, the stub bound in time to serve only its three programmed
    // 503s, the verdict never landed, and three innocent cars wore it.
    // Under no load the stub wins that race, which is why every car had
    // passed this test on its own gate minutes earlier.
    //
    // So the stub says when it is up and this waits for it to say so.
    // `started` first, ALWAYS: a stub with a delay owes the test `delay`
    // seconds of darkness, and measuring that from `spawn()` hands
    // python's boot time to the race in the other direction.
    stub.await_fact(
        &started,
        Duration::from_secs(30),
        "the stub process never started",
    );
    if delay_secs == 0.0 {
        stub.await_fact(&bound, Duration::from_secs(10), "the stub never bound");
    }
    stub
}

/// Run `report <verdict> <note>` from the lifted block against the stub.
/// Returns (stdout lines, exit ok). `backoff` is the sleep schedule
/// between attempts — the production default is minutes; tests pass
/// zeros.
fn run_report(stub: &Stub, backoff: &str, verdict: &str) -> (Vec<String>, bool) {
    let harness = format!(
        "set -euo pipefail\n\
         JOBS_API=http://127.0.0.1:{port}\n\
         GATE_RUN_JOB_ID={PACKET}\n\
         ACTOR='{{\"id\":\"automation:gate-runner\",\"role\":\"platform-admin\",\"access_tier\":\"operator\"}}'\n\
         {block}\n\
         report \"$1\" \"$2\"\n",
        port = stub.port,
        block = report_block(),
    );
    let path = stub.dir.join("harness.sh");
    std::fs::write(&path, harness).expect("write harness");
    let out = Command::new("bash")
        .arg(&path)
        .arg(verdict)
        .arg("{\"verdict\":\"green\",\"head\":\"abc\"}")
        .env("GATE_REPORT_BACKOFF", backoff)
        .output()
        .expect("bash runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines = stdout
        .lines()
        .chain(stderr.lines())
        .map(str::to_string)
        .collect::<Vec<_>>();
    (lines, out.status.success())
}

fn skip() -> bool {
    if missing("python3") || missing("curl") {
        eprintln!("skipping: python3 and curl are both required");
        return true;
    }
    false
}

/// THE INCIDENT: the SoR answers 503 for a while, then comes back. The
/// verdict must land on the packet, and the log must show each attempt
/// so a reader can see the roll the runner rode out.
#[test]
fn a_report_that_meets_a_rolling_sor_retries_until_it_lands() {
    if skip() {
        return;
    }
    let stub = start_stub("roll", "503:3", 0.0);
    let (lines, ok) = run_report(&stub, "0 0 0 0 0", "green");
    let joined = lines.join("\n");
    assert!(
        ok,
        "the report must succeed once the SoR is back:\n{joined}"
    );
    let retries = lines
        .iter()
        .filter(|l| l.contains("gate-runner: report attempt") && l.contains("retrying"))
        .count();
    assert_eq!(
        retries, 3,
        "three refused attempts must each be logged as a retry:\n{joined}"
    );
    assert!(
        joined.contains("recorded on packet") && joined.contains("attempt 4"),
        "the success line must name the packet and the attempt that landed:\n{joined}"
    );
    let puts = stub.puts();
    assert_eq!(
        puts.len(),
        1,
        "exactly one verdict write must land: {puts:?}"
    );
    assert!(
        puts[0].starts_with(&format!("/api/jobs/{PACKET}/steps/step-verdict "))
            && puts[0].contains("\"verdict\": \"green\""),
        "the write goes to the record-verdict step with the verdict: {}",
        puts[0]
    );
}

/// A bare Service mid-Recreate has no endpoints at all: the connection is
/// REFUSED, not answered. That is a different curl failure from a 503
/// and must be retried the same way.
#[test]
fn a_dark_sor_is_a_connection_refusal_the_report_survives() {
    if skip() {
        return;
    }
    // The stub does not listen for ~1.5s; the schedule gives ~8s.
    let stub = start_stub("dark", "503:0", 1.5);
    let (lines, ok) = run_report(&stub, "1 1 1 1 1 1 1 1", "green");
    let joined = lines.join("\n");
    assert!(ok, "the report must land once the SoR listens:\n{joined}");
    assert!(
        joined.contains("retrying"),
        "connection refused must be logged and retried:\n{joined}"
    );
    assert_eq!(stub.puts().len(), 1, "one verdict write:\n{joined}");
}

/// When the SoR never comes back the runner must give up, say so on one
/// greppable line, and fail — silence and a clean exit were the defect.
#[test]
fn a_sor_that_never_returns_exhausts_the_retries_and_says_so() {
    if skip() {
        return;
    }
    let stub = start_stub("never", "never", 0.0);
    let (lines, ok) = run_report(&stub, "0 0", "green");
    let joined = lines.join("\n");
    assert!(!ok, "an unrecorded verdict is a failed report:\n{joined}");
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.contains("gate-runner: report attempt"))
            .count(),
        3,
        "a schedule of two sleeps is three attempts:\n{joined}"
    );
    assert!(
        joined.contains("out of retries"),
        "the last line must say the retries are exhausted:\n{joined}"
    );
    assert!(stub.puts().is_empty(), "nothing can have landed:\n{joined}");
}

/// A refusal that is ABOUT the write (a frozen step answers 409) is not a
/// roll and must not burn five minutes pretending it might be: one
/// attempt, one explanation, and the existing terminal-packet branch in
/// run.sh takes it from there.
#[test]
fn a_definitive_refusal_is_not_retried() {
    if skip() {
        return;
    }
    let stub = start_stub("frozen", "409", 0.0);
    let (lines, ok) = run_report(&stub, "0 0 0", "green");
    let joined = lines.join("\n");
    assert!(!ok, "a 409 is a failed report:\n{joined}");
    assert!(
        joined.contains("not retrying") && joined.contains("409"),
        "the refusal must be named and not retried:\n{joined}"
    );
    assert_eq!(
        stub.puts().len(),
        1,
        "exactly one write was attempted:\n{joined}"
    );
}

/// The production schedule covers a roll with room to spare: a roll is
/// 30–90s, and the runner keeps trying for about five minutes.
#[test]
fn the_default_backoff_spans_a_roll() {
    let block = report_block();
    let line = block
        .lines()
        .find(|l| l.trim_start().starts_with("GATE_REPORT_BACKOFF="))
        .expect("the block declares GATE_REPORT_BACKOFF with a default");
    let default = line
        .split(":-")
        .nth(1)
        .and_then(|s| s.split('}').next())
        .expect("default schedule in ${VAR:-...} form");
    let total: u32 = default
        .split_whitespace()
        .map(|s| s.parse::<u32>().expect("integer seconds"))
        .sum();
    assert!(
        (240..=360).contains(&total),
        "default backoff sums to {total}s; a roll is 30–90s and the budget is ~5 minutes"
    );
}

/// Sanity for the harness itself: the lifted block is the one that ships,
/// not a copy — it must still be bracketed in run.sh.
#[test]
fn the_block_is_lifted_from_run_sh_not_copied() {
    let run_sh = repo_root().join("infra/gate-runner/run.sh");
    let f = std::fs::File::open(&run_sh).expect("run.sh opens");
    let marked = BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter(|l| l.trim() == BEGIN || l.trim() == END)
        .count();
    assert_eq!(
        marked, 2,
        "run.sh carries exactly one begin and one end marker"
    );
}

/// THE REPORT-BACK MUST RECORD ITS OWN STORY.
///
/// The retry loop above prints every attempt, every wait and every
/// transient failure — to the pod log, which is reaped with the Job. So
/// the fact that a gate rode out a dark system of record survives
/// exactly as long as `kubectl logs` does, and the packet, which is the
/// record, says only "green".
///
/// That gap has already cost a diagnosis: during train #282's converge
/// two gates reported cleanly across a dark SoR and there is no record
/// that it happened. §Diagnosis: "an alarm that reports through its
/// subject dies with it" — the runner cannot fix that, but it CAN carry
/// its own account in the payload it finally lands, so the roll is
/// visible afterwards instead of only during.
#[test]
fn the_landed_receipt_records_the_report_backs_own_story() {
    if skip() {
        return;
    }
    let stub = start_stub("story", "503:3", 0.0);
    let (lines, ok) = run_report(&stub, "0 0 0 0 0", "green");
    let joined = lines.join("\n");
    assert!(ok, "the report must land once the SoR is back:\n{joined}");
    let puts = stub.puts();
    assert_eq!(puts.len(), 1, "exactly one write lands: {puts:?}");

    let body = puts[0]
        .split_once(' ')
        .map(|(_, b)| b.to_string())
        .unwrap_or_else(|| panic!("the logged PUT has a body: {}", puts[0]));
    let put: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("PUT body is JSON ({e}): {body}"));
    let raw = put
        .pointer("/metadata/receipt")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("the write carries a receipt: {body}"));
    let receipt: serde_json::Value = serde_json::from_str(raw)
        .unwrap_or_else(|e| panic!("the receipt is a JSON string ({e}): {raw}"));

    // The gate's own findings are untouched by the reporting story.
    assert_eq!(
        receipt.get("verdict").and_then(|v| v.as_str()),
        Some("green"),
        "the verdict must survive the annotation: {raw}"
    );
    let report = receipt
        .get("report")
        .unwrap_or_else(|| panic!("the receipt must carry the report-back's own story: {raw}"));
    assert_eq!(
        report.get("attempts").and_then(serde_json::Value::as_u64),
        Some(4),
        "three refusals and the write that landed is four attempts: {raw}"
    );
    assert_eq!(
        report
            .get("sor_unreachable")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "a report that had to ride out a dark SoR must say so on the packet: {raw}"
    );
    assert!(
        report
            .get("waited_s")
            .and_then(serde_json::Value::as_u64)
            .is_some(),
        "the seconds spent waiting are the size of the outage this run saw: {raw}"
    );
}

/// ...and a report that landed first time says so plainly, so `report`
/// present is not itself read as trouble.
#[test]
fn a_first_attempt_report_records_a_clean_story() {
    if skip() {
        return;
    }
    let stub = start_stub("clean", "503:0", 0.0);
    let (lines, ok) = run_report(&stub, "0 0 0", "green");
    let joined = lines.join("\n");
    assert!(ok, "the report must land:\n{joined}");
    let puts = stub.puts();
    assert_eq!(puts.len(), 1, "one write: {puts:?}");
    let body = puts[0].split_once(' ').expect("body").1.to_string();
    let put: serde_json::Value = serde_json::from_str(&body).expect("PUT body is JSON");
    let raw = put
        .pointer("/metadata/receipt")
        .and_then(|v| v.as_str())
        .expect("receipt present");
    let receipt: serde_json::Value = serde_json::from_str(raw).expect("receipt is JSON");
    assert_eq!(
        receipt.pointer("/report/attempts"),
        Some(&serde_json::json!(1)),
        "one attempt: {raw}"
    );
    assert_eq!(
        receipt.pointer("/report/sor_unreachable"),
        Some(&serde_json::json!(false)),
        "a clean report must say the SoR was reachable, not stay silent: {raw}"
    );
}
