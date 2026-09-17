//! `infra/protocol-drift.sh` — the drift between the authored protocol
//! bundle and the live registry is MEASURED daily and FILED, not thrown
//! away with a gate log (backlog 19dec171, car 1 of 8f4e9cc0).
//!
//! The comparison already existed: `the-live-protocols-are-the-authored-
//! protocols.sh` computes field-level drift between
//! `infra/platform/workflows/` and `GET /api/workflows` on every gate and
//! REPORTS it — into a log nobody keeps. The server cannot compute it (it
//! has no tree), the tree cannot serve it (it has no page), and the one
//! host with both — the converged checkout on boss-gcp — filed nothing.
//! Measured 2026-09-15 against the system of record: 85 admitted kinds,
//! 47 compared, 2 fields adrift (`maintenance-sweep.description`,
//! `ship-a-change.description`), and no record of either outside the
//! terminal that ran the lint.
//!
//! The stub below is the jobs API as the script sees it: it serves the
//! registry (`/api/workflows`), the open packet (`/api/jobs?kind=…`), and
//! records the PATCH — so the test reads the filed row, not the script's
//! account of it. The registry fixture is rendered FROM this tree's own
//! bundle with exactly two disagreements planted: one description edited
//! live, one kind admitted that no file authors. Anything else the lint
//! finds would be a fact about the fixture, not about the script.

use boss_testing::repo_root;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

const DRIFTED_KIND: &str = "backlog-item";
const LIVE_ONLY_KIND: &str = "zeta-live-only";
/// A kind a delivered tenant admitted, owned by that tenant (see registry_fixture).
const DELIVERED_TENANT_KIND: &str = "receive-a-sponsorship";
const JOB_ID: &str = "0123456789abcdef";

/// "I could not answer." `infra/lint/lib/git-answer.sh` carries the
/// argument; here it means the lint produced no report to file.
const CANNOT_ANSWER: i32 = 3;

const STUB: &str = r#"
import http.server, json, sys, socket
log, started, registry, mode = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.bind(("127.0.0.1", 0))
port = sock.getsockname()[1]
with open(started, "w") as f:
    f.write(str(port))
OPEN = {"data": [{"id": "0123456789abcdef", "kind": "maintenance-protocol-drift", "status": "open", "created_at": "2026-09-15T05:20:35Z", "metadata": {}}], "total": 1}
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _reply(self, code, body=b"{}"):
        self.send_response(code); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        with open(log, "ab") as f:
            f.write(b"GET " + self.path.encode() + b"\n")
        if self.path.startswith("/api/workflows"):
            if mode == "registry-down":
                self._reply(503, b'{"error":"upstream unavailable"}')
            else:
                self._reply(200, open(registry, "rb").read())
        elif self.path.startswith("/api/jobs"):
            self._reply(200, json.dumps(OPEN).encode())
        else:
            self._reply(404, b'{"error":"no such route"}')
    def do_PATCH(self):
        n = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(n)
        # One line per body so the test can read the log line-wise; a
        # body that is not JSON is recorded as such rather than dropped.
        try:
            line = json.dumps(json.loads(body), separators=(",", ":")).encode()
        except Exception as e:
            line = b"NOT JSON: " + repr(e).encode()
        with open(log, "ab") as f:
            f.write(b"PATCH " + self.path.encode() + b"\n" + line + b"\n")
        self._reply(204, b"")
srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H, bind_and_activate=False)
srv.socket.close(); srv.socket = sock; srv.server_address = sock.getsockname(); srv.server_activate()
srv.serve_forever()
"#;

fn script() -> PathBuf {
    repo_root().join("infra/protocol-drift.sh")
}

/// The live registry as a fixture: every kind this tree's bundle authors,
/// rendered from the files themselves at version 7, plus the two planted
/// disagreements. Returns the JSON and the number of bundle files read.
fn registry_fixture() -> (String, usize) {
    let bundle = repo_root().join("infra/platform/workflows");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&bundle)
        .expect("infra/platform/workflows")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    let mut rows = Vec::new();
    let mut saw_drifted = false;
    for f in &files {
        let text = std::fs::read_to_string(f).expect("read a bundle file");
        let doc: toml::Value = toml::from_str(&text).expect("a bundle file parses");
        let wf = &doc["workflow"][0];
        let kind = wf["kind"].as_str().expect("kind").to_string();
        // The steps ride too, in the row's shape (`steps`, each with
        // its `fields`), because the lint compares them since
        // 2026-09-15 (count, titles, required set, label — backlog
        // 0ccf23ec): a fixture without them would plant a step drift
        // on every kind and the two DELIBERATE disagreements below
        // would drown in it.
        let steps: Vec<serde_json::Value> = wf
            .get("step")
            .and_then(|s| s.as_array())
            .map(|steps| {
                steps
                    .iter()
                    .map(|s| {
                        let fields: Vec<serde_json::Value> = s
                            .get("fields")
                            .and_then(|f| f.as_array())
                            .map(|fs| {
                                fs.iter()
                                    .map(|f| {
                                        serde_json::json!({
                                            "name": f.get("name").and_then(|v| v.as_str()),
                                            "field_type": f.get("field_type").and_then(|v| v.as_str()),
                                            "required": f.get("required").and_then(|v| v.as_bool()).unwrap_or(false),
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        serde_json::json!({
                            "title": s.get("title").and_then(|v| v.as_str()),
                            "kind": s.get("kind").and_then(|v| v.as_str()),
                            "ready_when": s.get("ready_when").and_then(|v| v.as_str()),
                            "title_template": s.get("title_template").and_then(|v| v.as_str()).unwrap_or(""),
                            "fields": fields,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut row = serde_json::json!({
            "kind": kind,
            "version": 7,
            "status": "active",
            "label": wf.get("label").and_then(|v| v.as_str()),
            "category": wf.get("category").and_then(|v| v.as_str()),
            "owning_team": "platform",
            "description": wf.get("description").and_then(|v| v.as_str()),
            "steps": steps,
        });
        if kind == DRIFTED_KIND {
            saw_drifted = true;
            row["description"] = serde_json::json!(
                "OPERATOR EDIT 2026-09-14: this live sentence no longer matches its file."
            );
        }
        rows.push(row);
    }
    assert!(
        saw_drifted,
        "{DRIFTED_KIND} is not in the bundle any more; pick another kind to plant the drift on"
    );
    rows.push(serde_json::json!({
        "kind": LIVE_ONLY_KIND,
        "version": 1,
        "status": "active",
        "label": "Zeta",
        "category": "platform",
        "owning_team": "platform",
        "description": "Published live through POST /api/workflows and never written back."
    }));
    // A DELIVERED TENANT'S protocol (2026-09-17): admitted by `boss
    // tenant publish` with the tenant's own owning_team, authored in the
    // tenant's repo the tree declares as prod's source (instances.toml
    // tenant_repo). Not unauthored — a team this tree authors nothing
    // for, on a tree with a repo-sourced instance, is that tenant's.
    rows.push(serde_json::json!({
        "kind": DELIVERED_TENANT_KIND,
        "version": 1,
        "status": "active",
        "label": "Receive a sponsorship",
        "category": "sales",
        "owning_team": "algedonic",
        "description": "Authored in david/algedonic-llc seeds/workflows.toml, delivered as the boss-tenant ConfigMap."
    }));
    (serde_json::to_string(&rows).unwrap(), files.len())
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
    let dir = boss_testing::scratch_dir(&format!("protocol-drift-stub-{case}"));
    let script = dir.join("stub.py");
    let started = dir.join("started");
    let log = dir.join("calls.log");
    let registry = dir.join("workflows.json");
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_file(&started);
    std::fs::write(&registry, registry_fixture().0).unwrap();
    std::fs::write(&script, STUB).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let child = Command::new("python3")
        .arg(&script)
        .arg(&log)
        .arg(&started)
        .arg(&registry)
        .arg(mode)
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

fn run(stub: &Stub, cmd: &str) -> (Option<i32>, String, String) {
    let out = Command::new("bash")
        .arg(script())
        .arg(cmd)
        .arg("--repo")
        .arg(repo_root())
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

/// `file` measures the registry against this tree's bundle and PATCHes
/// ONE row — `measured` + `drift` — onto the open packet: the planted
/// live-only kind under `drift.unauthored`, the planted description edit
/// under `drift.fields` named by kind, field and live version.
#[test]
fn file_patches_measured_and_drift_onto_the_open_packet() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("file", "serve");
    let (_, bundle_files) = registry_fixture();
    let (rc, out, err) = run(&stub, "file");
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
        ["drift", "measured"],
        "two top-level keys, merged server-side: {body}"
    );

    let m = &body["measured"];
    assert_eq!(
        m["target"],
        format!("http://127.0.0.1:{}/api/workflows", stub.port),
        "the row names the surface it read"
    );
    assert_eq!(
        m["live_admitted"].as_u64(),
        Some(bundle_files as u64 + 2),
        "every bundle kind, the live-only one and the delivered tenant's: {m}"
    );
    assert_eq!(
        m["fields_compared"].as_u64(),
        Some(bundle_files as u64),
        "every bundle kind had a live row to compare against: {m}"
    );
    assert_eq!(
        m["lint_exit"].as_i64(),
        Some(1),
        "an unauthored live kind is the lint's exit 1, and the row says so rather than hiding it: {m}"
    );
    assert!(m["at"].as_str().is_some_and(|s| s.ends_with('Z')), "{m}");
    assert!(m["method"].is_object(), "the method rides on the row: {m}");
    // `head` is context, not the measurement: a sha when git can read the
    // checkout, otherwise null WITH the reason — never a refusal, because
    // the comparison did happen.
    let head = m["head"].as_str();
    assert!(
        head.is_some_and(|h| h.len() == 40) || m["head_why"].as_str().is_some(),
        "head is neither a sha nor explained: {m}"
    );

    let d = &body["drift"];
    assert_eq!(
        d["unauthored"],
        serde_json::json!([LIVE_ONLY_KIND]),
        "what is live that the tree does not say: {d}"
    );
    let fields = d["fields"].as_array().expect("drift.fields is an array");
    assert_eq!(fields.len(), 1, "one planted description edit: {d}");
    assert_eq!(fields[0]["kind"], DRIFTED_KIND);
    assert_eq!(fields[0]["field"], "description");
    assert_eq!(fields[0]["live_version"].as_u64(), Some(7));
    assert!(
        fields[0]["live_window"]
            .as_str()
            .is_some_and(|w| w.contains("OPERATOR EDIT 2026-09-14")),
        "the finding carries an excerpt of the live text: {}",
        fields[0]
    );
    assert!(
        fields[0]["tree_window"].as_str().is_some(),
        "and of the file's: {}",
        fields[0]
    );
    assert_eq!(d["counts"]["unauthored"].as_u64(), Some(1), "{d}");
    assert_eq!(d["counts"]["fields"].as_u64(), Some(1), "{d}");
    assert_eq!(
        d["pending"],
        serde_json::json!([]),
        "every bundle kind is admitted in this fixture, so nothing is pending: {d}"
    );
    assert!(
        d["tenants"].as_array().is_some_and(|t| !t.is_empty()),
        "the tenant bundles this deployment does not run are counted: {d}"
    );

    // THE JOURNAL GETS THE HEADLINE, not a digest that hides the counts.
    assert!(
        out.contains(&format!("filed on {}", &JOB_ID[..8])),
        "stdout does not name the packet:\n{out}"
    );
    assert!(
        out.contains("1 live kind(s) the tree does not author")
            && out.contains("1 field(s) adrift"),
        "the headline does not carry the two drift counts:\n{out}"
    );
}

/// `row` is the same measurement printed, for an operator asking the
/// question by hand — it reads the registry and touches no packet.
#[test]
fn row_prints_the_measurement_and_files_nothing() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("row", "serve");
    let (rc, out, err) = run(&stub, "row");
    assert_eq!(rc, Some(0), "row did not exit 0:\n{err}");
    let row: serde_json::Value = serde_json::from_str(&out).expect("row prints JSON");
    assert_eq!(
        row["drift"]["unauthored"],
        serde_json::json!([LIVE_ONLY_KIND]),
        "the platform kind nobody wrote down is unauthored; the delivered tenant's kind \
         ({DELIVERED_TENANT_KIND}, owning_team algedonic, prod's tenant_repo) is not"
    );
    assert_eq!(row["drift"]["fields"].as_array().map(Vec::len), Some(1));
    assert!(
        !row["drift"]["unauthored"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == DELIVERED_TENANT_KIND),
        "the delivered tenant's kind is not unauthored: {}",
        row["drift"]
    );
    assert!(
        stub.patches().is_empty(),
        "row filed something:\n{}",
        stub.log()
    );
    assert!(
        !stub.log().contains("/api/jobs"),
        "row read the packet queue it has no business with:\n{}",
        stub.log()
    );
}

/// When the lint cannot read the registry it produces NO report, and the
/// script REFUSES — exit 3, naming the lint, its exit code and what it
/// could not reach, with the lint's own stderr kept whole. A drift
/// measurement filed from no comparison would read as "no drift".
#[test]
fn a_registry_that_cannot_be_read_is_a_refusal_not_a_clean_row() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("down", "registry-down");
    let (rc, out, err) = run(&stub, "file");
    assert_eq!(
        rc,
        Some(CANNOT_ANSWER),
        "an unreadable registry must exit {CANNOT_ANSWER}, not {rc:?}:\n{out}\n{err}"
    );
    assert!(
        stub.patches().is_empty(),
        "a refusal must file nothing:\n{}",
        stub.log()
    );
    assert!(
        err.contains("CANNOT ANSWER")
            && err.contains("the-live-protocols-are-the-authored-protocols.sh"),
        "the refusal does not name the lint that produced no report:\n{err}"
    );
    assert!(
        err.contains("exit 75"),
        "the refusal does not carry the lint's exit code:\n{err}"
    );
    assert!(
        err.contains("SKIPPED the live comparison") && err.contains("HTTP 503"),
        "the lint's own stderr — the only copy of WHY — was reduced away:\n{err}"
    );
    assert!(
        err.contains("no report was written"),
        "the lint does not say, in its own words, that it wrote no report:\n{err}"
    );
}
