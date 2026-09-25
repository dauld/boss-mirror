//! The ops-runner EXECUTES a passkey-approved verb, and refuses every
//! approval it cannot verify (backlog fd7090cc, design 17835005).
//!
//! THE GAP THIS CLOSES, measured on origin/main 87c2e366 (2026-09-23).
//! The design had two halves on main: a destructive verb renders a plan
//! first (`plan-a-pod-reap`, `plan-a-tenant-merge`, each printing the
//! plan on stdout and `plan-sha256:` on stderr, the write re-rendering
//! and comparing), and ops-request v2 carries an `approve` sign-off step
//! with `assurance_required = "presence"`, so a passkey stamp binds
//! `step_shape_hash(title, metadata)` — the plan bytes on that step. The
//! third half was missing: the runner refused EVERY `requires_approval`
//! verb ("nothing issues one yet"), nothing wrote a plan onto the approve
//! step, so David's passkey approved nothing.
//!
//! WHAT THE RUNNER DOES NOW, and what these cases pin:
//!
//! - an approve step that is ready with no plan gets the verb's declared
//!   `plan_verb` rendered on the host and written onto it through the step
//!   metadata door; a plan verb that refuses closes the request refused,
//!   carrying its words;
//! - `execute` runs only when the approve step is COMPLETED and carries,
//!   for every required role, a PRESENCE stamp bound to the step's
//!   current shape hash — recomputed here, pinned equal to
//!   `boss_core::job::step_shape_hash` by the happy path below (§9a);
//! - the approval is single-use (execute is claimed `active` before the
//!   argv runs, and an `active` execute is never run again) and
//!   time-boxed (`APPROVAL_TTL_S`, ten minutes);
//! - the write gets sha256 of the SIGNED plan as its `plan_sha256`, so a
//!   state that drifted since the signature re-renders to different bytes
//!   and the write's own script refuses it.
//!
//! q2, as David ruled it: the runner trusts the system of record, and a
//! writable field is never an approval. Every refusal names what it
//! refused, on the request itself.
//!
//! Run against a stubbed system of record (a `curl` on PATH that serves
//! one packet on GET and logs every write, in order), so each verdict is
//! one the runner actually made.

use boss_testing::repo_root;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// THESE CASES NEVER PASS BY NOT RUNNING (security review of this car,
/// 2026-09-24). This macro used to `return` with a SKIPPED line when jq
/// or sha256sum was missing — and a test that returns early is reported
/// as passed, so every refusal below could have gone green on a box
/// where none of them ran. The gate image carries both tools, so a box
/// without them is a box these tests cannot vouch for, and that is a
/// failure naming the tool, not a pass.
macro_rules! needs_tools {
    () => {
        for tool in ["jq", "sha256sum"] {
            assert!(
                has(tool),
                "ops_runner_approval_sh: no {tool} on this box — the gate image has it, and a \
                 security test that cannot run must fail, never pass by returning early"
            );
        }
    };
}

/// The plan the fixture plan verb renders: every byte class a real plan
/// carries (quotes, a backslash, a tab, non-ASCII, a trailing newline),
/// because the runner's shape hash must agree with the server's over
/// exactly these, and a disagreement would refuse every real approval.
const PLAN: &str = "PLAN wipe target-a\n  observed: \"quoted\" and a back\\slash\n\tindented — é\nargv: wipe target-a <plan-sha256>\n";

const APPROVE_TITLE: &str = "Approve the plan: wipe on forge";

/// The approve step's procedure, as ops-request v2 materialises it: an
/// em dash and newlines, so the canonical form is exercised on prose.
const PROCEDURE: &str = "READ THE PLAN ON THIS STEP AND NOTHING ELSE.\n\nYOUR PASSKEY SIGNS THESE EXACT BYTES — nothing else.";

/// sha256 of `bytes`, by the same `sha256sum` the runner uses. Fed on
/// stdin rather than through a file: every case hashes, the cases run
/// on parallel threads of one process, and a shared scratch file would
/// be cleared under one case by another.
fn sha256_hex(bytes: &[u8]) -> String {
    use std::io::Write;
    let mut child = Command::new("sha256sum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("sha256sum runs");
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout)[..64].to_string()
}

/// The stubbed system of record. A GET of the open-request LIST serves
/// `jobs.json`; a GET of ONE job (`/api/jobs/<id>`, the re-read the
/// runner makes immediately before it claims) serves `reread.json` when
/// a case wrote one, else that same job — so a case can move the record
/// between the runner's first read and its claim. Every write is logged
/// in order with its method, url, the `x-boss-user` it was signed as and
/// its body (`null` for a bodyless POST, which is what the claim door
/// is). `STUB_REFUSE_STATUS` answers 409 to a write whose body sets that
/// status; `STUB_REFUSE_CLAIM` answers 409 to the claim door
/// unconditionally — a race lost at the server after the runner's
/// re-read, which a static record cannot otherwise show.
///
/// THE CLAIM DOOR JUDGES THE HOLDER THE WAY THE SERVER DOES (re-review of
/// 2026-09-25). This stub used to answer every claim 200, so the happy
/// path went green while the live door would have refused every runner
/// claim: the dispatcher had nominated each execute to `agent-claude`,
/// and the compare-and-set (`claim_step_at`, both adapters) admits a
/// READY step only when it is unheld or held by the claimant, and an
/// ACTIVE one only to its holder. The stub now reads the step from the
/// record the runner would re-read and applies exactly that rule,
/// answering 409 with the server's body — so a fixture that models the
/// dispatcher wrongly fails here, not in production.
const STUB_CURL: &str = r#"#!/bin/sh
m=GET; prev=; o=; w=; url=; body=; user=
for a in "$@"; do
    [ "$prev" = -X ] && m="$a"
    [ "$prev" = -o ] && o="$a"
    [ "$prev" = -w ] && w=1
    if [ "$prev" = -H ]; then
        case "$a" in "x-boss-user: "*) user="${a#x-boss-user: }" ;; esac
    fi
    case "$a" in @*) body="${a#@}" ;; http*) url="$a" ;; esac
    prev="$a"
done
if [ "$m" = GET ]; then
    case "$url" in
        */api/jobs/*)
            if [ -f "$STUB_DIR/reread.json" ]; then cat "$STUB_DIR/reread.json"
            else jq -c '.data[0]' "$STUB_DIR/jobs.json"; fi ;;
        *) cat "$STUB_DIR/jobs.json" ;;
    esac
    exit 0
fi
if [ -n "$body" ]; then b=$(cat "$body"); else b=null; fi
printf '%s' "$b" | jq -c --arg m "$m" --arg u "$url" --arg who "$user" \
    '{method: $m, url: $u, user: (($who | fromjson?) // $who), body: .}' >> "$STUB_DIR/writes.jsonl"
code=200; said='{"error":"stub refused"}'
if [ -n "${STUB_REFUSE_STATUS:-}" ] && [ "$(printf '%s' "$b" | jq -r '.status? // ""')" = "$STUB_REFUSE_STATUS" ]; then code=409; fi
case "$url" in
    */claim)
        if [ -n "${STUB_REFUSE_CLAIM:-}" ]; then code=409
        else
            sid=${url%/claim}; sid=${sid##*/}
            if [ -f "$STUB_DIR/reread.json" ]; then rec=$(cat "$STUB_DIR/reread.json")
            else rec=$(jq -c '.data[0]' "$STUB_DIR/jobs.json"); fi
            me=$(jq -rn --arg w "$user" '(($w | fromjson?) // {id: $w}) | .id // ""')
            verdict=$(printf '%s' "$rec" | jq -c --arg sid "$sid" --arg me "$me" '
                ((.steps // []) | map(select(.id == $sid)) | .[0]) as $s
                | ($s.assignee_id // null) as $h
                | if $s == null then {code: 404, body: "step not found"}
                  elif ($s.status == "ready" and ($h == null or $h == $me))
                       or ($s.status == "active" and $h == $me)
                  then {code: 200, body: ($s + {status: "active", assignee_id: $me})}
                  else {code: 409, body: {error: "step already claimed or not claimable",
                                          holder: $h, status: $s.status}} end')
            code=$(printf '%s' "$verdict" | jq -r '.code')
            said=$(printf '%s' "$verdict" | jq -c '.body')
        fi ;;
esac
if [ -n "$o" ]; then printf '%s' "$said" > "$o"; fi
if [ -n "$w" ]; then printf '%s' "$code"; exit 0; fi
[ "$code" = 200 ] || exit 22
exit 0
"#;

/// A fixture: the stubbed SoR, a plan verb and a write verb, and the
/// scripts behind them. `plan-wipe` prints [`PLAN`] and its hash (or
/// refuses when `PLAN_REFUSE` is set, or renders `PLAN_OVERRIDE`'s
/// bytes instead — the state having moved); `wipe` records the argv it
/// was given and how many writes the runner had made before it ran.
struct Fixture {
    root: PathBuf,
    verbs: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = boss_testing::scratch_dir(&format!("ops-runner-approval-{case}"));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        boss_testing::write_exec(&bin.join("curl"), STUB_CURL);
        std::fs::write(root.join("plan.txt"), PLAN).unwrap();
        let plan_sh = root.join("plan.sh");
        boss_testing::write_exec(
            &plan_sh,
            &format!(
                "#!/bin/sh\n\
                 if [ -n \"${{PLAN_REFUSE:-}}\" ]; then echo \"plan-wipe: REFUSED — $PLAN_REFUSE\" >&2; exit 78; fi\n\
                 p=\"{plan}\"\n\
                 if [ -n \"${{PLAN_OVERRIDE:-}}\" ]; then p=\"$PLAN_OVERRIDE\"; fi\n\
                 cat \"$p\"\n\
                 echo \"plan-sha256: $(sha256sum \"$p\" | cut -d' ' -f1)\" >&2\n",
                plan = root.join("plan.txt").display()
            ),
        );
        let apply_sh = root.join("apply.sh");
        boss_testing::write_exec(
            &apply_sh,
            &format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$@\" > \"{root}/applied.args\"\n\
                 wc -l < \"{root}/writes.jsonl\" > \"{root}/writes-before-apply\"\n\
                 echo applied\n",
                root = root.display()
            ),
        );
        let verbs = root.join("verbs");
        std::fs::create_dir_all(&verbs).unwrap();
        std::fs::write(
            verbs.join("wipe.json"),
            json!({
                "about": "MUTATING — test fixture.",
                "hosts": ["forge"],
                "requires_approval": true,
                "plan_verb": "plan-wipe",
                "approvers": [APPROVER],
                "argv": [apply_sh.display().to_string(), "{1}", "{2}"],
                "params": [
                    {"name": "target", "pattern": "^[a-z-]{1,20}$"},
                    {"name": "plan_sha256", "pattern": "^[0-9a-f]{64}$"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            verbs.join("plan-wipe.json"),
            json!({
                "about": "READ-ONLY: renders what wipe would do.",
                "hosts": ["forge"],
                "argv": [plan_sh.display().to_string(), "{1}"],
                "params": [{"name": "target", "pattern": "^[a-z-]{1,20}$"}]
            })
            .to_string(),
        )
        .unwrap();
        Fixture { root, verbs }
    }

    fn verb_file(&self, name: &str, spec: Value) {
        std::fs::write(self.verbs.join(format!("{name}.json")), spec.to_string()).unwrap();
    }

    fn packet(&self, job: Value) {
        std::fs::write(
            self.root.join("jobs.json"),
            json!({"data": [job], "total": 1}).to_string(),
        )
        .unwrap();
    }

    /// What the ONE-job GET answers from here on: the record as it stands
    /// when the runner re-reads it immediately before claiming.
    fn reread(&self, job: Value) {
        std::fs::write(self.root.join("reread.json"), job.to_string()).unwrap();
    }

    /// Run the runner once. Returns its output and every write it made,
    /// in order.
    fn run(&self, env: &[(&str, &str)]) -> (String, Vec<Value>) {
        let _ = std::fs::remove_file(self.root.join("writes.jsonl"));
        std::fs::write(self.root.join("writes.jsonl"), "").unwrap();
        let _ = std::fs::remove_file(self.root.join("applied.args"));
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("sh");
        cmd.arg(repo_root().join("infra/ops/ops-runner.sh"))
            .env_clear()
            .env("PATH", path)
            .env("HOST_ID", "forge")
            .env("BOSS_JOBS_URL", "http://sor.invalid")
            .env("OPS_VERBS_DIR", &self.verbs)
            .env("STUB_DIR", &self.root);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("ops-runner.sh runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let writes = std::fs::read_to_string(self.root.join("writes.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("a logged write is JSON"))
            .collect();
        (text, writes)
    }

    /// The argv the write verb ran with, or None when it never ran.
    fn applied(&self) -> Option<Vec<String>> {
        std::fs::read_to_string(self.root.join("applied.args"))
            .ok()
            .map(|s| s.lines().map(str::to_string).collect())
    }
}

/// The one employee the fixture's write verb names as its approver.
const APPROVER: &str = "emp-david";

/// The approve step's metadata as v2 materialises it, plus — once the
/// runner has rendered — everything the runner writes there for the
/// passkey to sign: the plan, the verb, host and args it was rendered
/// for, and the hash of the bytes the runner itself rendered. And the
/// approver's `decision`, which both surfaces (sign-off.js and
/// ApprovalSurface) save BEFORE the stamp, so it sits inside the signed
/// shape: `approved` here, because a Reject completes the same step
/// through the same ceremony (see `a_rejected_approval_runs_nothing`).
fn approve_meta(plan: Option<&str>) -> Value {
    let mut m = json!({"authority_role": "platform-admin", "procedure": PROCEDURE});
    if let Some(p) = plan {
        m["plan"] = json!(p);
        m["verb"] = json!("wipe");
        m["host"] = json!("forge");
        m["args"] = json!(["target-a"]);
        m["rendered_plan_sha256"] = json!(sha256_hex(p.as_bytes()));
        m["decision"] = json!("approved");
    }
    m
}

/// The server's shape hash of an approve step carrying `meta` — the
/// value a presence stamp is bound to.
fn shape_of(meta: &Value) -> String {
    boss_core::job::step_shape_hash(APPROVE_TITLE, meta)
}

/// `secs` ago (negative: in the future), in the shape a live stamp's
/// `stamped_at` carries: RFC 3339, microseconds, `Z`.
fn iso_ago(secs: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let out = Command::new("date")
        .args([
            "-u",
            "-d",
            &format!("@{}", now - secs),
            "+%Y-%m-%dT%H:%M:%S.603230Z",
        ])
        .output()
        .expect("date runs");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A sign-off stamp in the shape `SignOffStamp` serialises to.
fn stamp(role: &str, assurance: &str, shape: &str, age_s: i64) -> Value {
    let mut s = json!({
        "authority_id": APPROVER,
        "role": role,
        "stamped_at": iso_ago(age_s),
        "shape_hash": shape,
        "assurance": assurance,
    });
    if assurance == "presence" {
        s["presence_nonce"] = json!("nonce-1");
    }
    s
}

/// The agent executor the dispatcher nominates executable
/// `platform-admin` work to on the live deployment — the holder 299 of
/// 300 ops-request executes carried on 2026-09-25.
const NOMINEE: &str = "agent-claude";

/// The `execute` step as the system of record hands it to the runner:
/// its metadata materialised from the TREE's `ops-request.toml` (the
/// step's `authority_role`, and `claimable` when it declares one — the
/// keys `merge_metadata` writes), and its holder what the dispatcher
/// makes it. The dispatcher leaves a step unheld only when the protocol
/// declares a role queue (`claimable`, or `human_only`:
/// `left_for_role_queue` in boss-dispatcher); anything else it nominates.
/// So a protocol edit that drops the declaration puts `agent-claude` on
/// every fixture's execute, the stub's claim door refuses the runner as
/// the live one would, and the happy path goes red here.
fn execute_as_dispatched() -> (Value, Value) {
    let path = repo_root().join("infra/platform/workflows/ops-request.toml");
    let doc: toml::Table =
        toml::from_str(&std::fs::read_to_string(&path).expect("ops-request.toml is in the tree"))
            .expect("ops-request.toml parses");
    let step = doc["workflow"][0]["step"]
        .as_array()
        .and_then(|s| {
            s.iter()
                .find(|s| s.get("title").and_then(|t| t.as_str()) == Some("execute"))
        })
        .expect("ops-request declares an execute step")
        .clone();
    let mut md = json!({});
    if let Some(role) = step.get("authority_role").and_then(|v| v.as_str()) {
        md["authority_role"] = json!(role);
    }
    let claimable = step.get("claimable").and_then(|v| v.as_bool());
    if let Some(c) = claimable {
        md["claimable"] = json!(c);
    }
    let human_only = step.get("human_only").and_then(|v| v.as_bool()) == Some(true);
    let holder = if claimable == Some(true) || human_only {
        Value::Null
    } else {
        json!(NOMINEE)
    };
    (md, holder)
}

/// An ops-request packet for `wipe target-a` on the forge.
fn job(approve_status: &str, meta: Value, sign_offs: Value, exec_status: &str) -> Value {
    let (exec_md, exec_holder) = execute_as_dispatched();
    json!({
        "id": "aaaaaaaa-0000-4000-8000-000000000000",
        "status": "open",
        "metadata": {"host": "forge", "verb": "wipe", "args": ["target-a"], "requires_approval": true},
        "steps": [
            {"id": "s-filed", "spec_slug": "filed", "title": "Host read requested: wipe on forge",
             "status": "completed", "metadata": {}},
            {"id": "s-approve", "spec_slug": "approve", "title": APPROVE_TITLE,
             "status": approve_status, "assurance_required": "presence",
             "sign_offs_required": ["platform-admin"], "sign_offs": sign_offs, "metadata": meta},
            {"id": "s-execute", "spec_slug": "execute", "title": "Run wipe on forge",
             "status": exec_status, "assignee_id": exec_holder, "metadata": exec_md},
            {"id": "s-answered", "spec_slug": "answered", "title": "Answered",
             "status": "pending", "metadata": {"outcome_kind": "completed"}},
            {"id": "s-refused", "spec_slug": "refused", "title": "Refused — outside the allowlist",
             "status": "pending", "metadata": {"outcome_kind": "aborted"}}
        ]
    })
}

/// A packet whose approval is exactly right: the plan on a completed
/// approve step, one presence stamp bound to its shape, a minute old.
fn approved_job() -> Value {
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    job(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    )
}

fn writes_to<'a>(writes: &'a [Value], step: &str) -> Vec<&'a Value> {
    writes
        .iter()
        .filter(|w| {
            w["url"]
                .as_str()
                .is_some_and(|u| u.contains(&format!("/steps/{step}")))
        })
        .collect()
}

/// The execute step's completion, which every execute-stage verdict
/// ends in.
fn execute_completion(writes: &[Value], out: &str) -> Value {
    writes_to(writes, "s-execute")
        .into_iter()
        .find(|w| w["body"]["status"] == "completed")
        .unwrap_or_else(|| panic!("execute was never completed: {writes:?}\n{out}"))["body"]["metadata"]
        .clone()
}

/// Every execute-stage refusal: completed `refused`, the reason on the
/// step naming what failed, and the write verb never run.
fn assert_refused(f: &Fixture, out: &str, writes: &[Value], names: &[&str]) {
    assert!(
        f.applied().is_none(),
        "the write verb RAN on a refused approval: {:?}\n{out}",
        f.applied()
    );
    let md = execute_completion(writes, out);
    assert_eq!(md["disposition"], "refused", "{md}\n{out}");
    let reason = md["reason"].as_str().unwrap_or_default();
    for n in names {
        assert!(
            reason.contains(n),
            "the refusal names {n:?}: {reason}\n{out}"
        );
    }
    assert!(
        !writes_to(writes, "s-execute")
            .iter()
            .any(|w| w["body"]["status"] == "active"),
        "a refused approval claims nothing: {writes:?}"
    );
}

// ---------------------------------------------------------------- plan

/// A READY APPROVE STEP GETS ITS PLAN. The plan verb runs on the host
/// with the request's own args, and its stdout — the exact bytes whose
/// hash it printed — lands on the approve step's `plan` field through
/// the step metadata door, where the passkey will sign it. Nothing runs
/// the write, and execute is not touched.
#[test]
fn a_ready_approve_step_gets_the_plan_verbs_bytes() {
    needs_tools!();
    let f = Fixture::new("plan-rendered");
    f.packet(job("ready", approve_meta(None), json!([]), "pending"));
    let (out, writes) = f.run(&[]);
    assert!(
        f.applied().is_none(),
        "rendering a plan runs no write: {out}"
    );
    let patch = writes_to(&writes, "s-approve")
        .into_iter()
        .find(|w| w["method"] == "PATCH")
        .unwrap_or_else(|| panic!("no plan was written onto the approve step: {writes:?}\n{out}"));
    assert!(
        patch["url"]
            .as_str()
            .unwrap()
            .ends_with("/api/jobs/aaaaaaaa-0000-4000-8000-000000000000/steps/s-approve/metadata"),
        "the plan goes through the step metadata door: {patch}"
    );
    assert_eq!(
        patch["body"]["plan"], PLAN,
        "the plan is the plan verb's stdout, byte for byte: {patch}"
    );
    // WHAT THE PASSKEY SIGNS IS THE WHOLE REQUEST, not the plan alone
    // (security review, 2026-09-24): the verb, the host and the args the
    // plan was rendered for ride the same write, so they sit inside the
    // step's shape hash — an args edit on the request after the signature
    // cannot borrow it — and the hash of the bytes THIS runner rendered
    // rides beside them, so a plan written by anyone else is detectable.
    assert_eq!(patch["body"]["verb"], "wipe", "{patch}");
    assert_eq!(patch["body"]["host"], "forge", "{patch}");
    assert_eq!(patch["body"]["args"], json!(["target-a"]), "{patch}");
    assert_eq!(
        patch["body"]["rendered_plan_sha256"],
        sha256_hex(PLAN.as_bytes()),
        "{patch}"
    );
    assert!(
        writes_to(&writes, "s-execute").is_empty() && writes_to(&writes, "s-refused").is_empty(),
        "rendering a plan completes nothing: {writes:?}"
    );
    assert!(
        out.contains(&sha256_hex(PLAN.as_bytes())),
        "the journal names the hash the passkey will be asked to sign: {out}"
    );
}

/// A plan already on the approve step is the one waiting to be signed —
/// re-rendering it would move the bytes under a reviewer.
#[test]
fn a_plan_waiting_for_its_signature_is_not_rendered_again() {
    needs_tools!();
    let f = Fixture::new("plan-waits");
    f.packet(job("ready", approve_meta(Some(PLAN)), json!([]), "pending"));
    let (out, writes) = f.run(&[]);
    assert!(
        writes.is_empty(),
        "a waiting plan is left alone: {writes:?}\n{out}"
    );
    assert!(f.applied().is_none(), "{out}");
    assert!(out.contains("waits for a passkey approval"), "{out}");
}

/// A PLAN VERB THAT REFUSES CLOSES THE REQUEST REFUSED, carrying its
/// words: a plan for something that cannot run is not a plan, and a
/// request left open on it would wait for a signature nothing can use.
/// It closes through the `refused` terminal, which completes from any
/// open state (fd0f92ae) — execute is still pending behind the approve
/// step, so it cannot be the door.
#[test]
fn a_refused_plan_closes_the_request_refused_with_the_plan_verbs_words() {
    needs_tools!();
    let f = Fixture::new("plan-refused");
    f.packet(job("ready", approve_meta(None), json!([]), "pending"));
    let (out, writes) = f.run(&[("PLAN_REFUSE", "the target is mounted")]);
    assert!(f.applied().is_none(), "{out}");
    assert!(
        !writes.iter().any(|w| w["method"] == "PATCH"),
        "a refused plan writes no plan: {writes:?}"
    );
    let close = writes_to(&writes, "s-refused")
        .into_iter()
        .find(|w| w["body"]["status"] == "completed")
        .unwrap_or_else(|| panic!("the request was not closed refused: {writes:?}\n{out}"));
    let md = &close["body"]["metadata"];
    let reason = md["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("plan-wipe") && reason.contains("the target is mounted"),
        "the refusal carries the plan verb's own words: {reason}"
    );
    assert_eq!(
        md["outcome_kind"], "aborted",
        "the stored keys ride the completion: {md}"
    );
}

/// An approval verb whose write cannot re-render and compare is
/// refused before any plan exists: its last param must be a required
/// `plan_sha256`, or an approval could outlive the state it approved.
/// `commission-a-disk` is the shipped instance, and stays inert.
#[test]
fn an_approval_verb_that_cannot_void_a_drifted_plan_is_refused() {
    needs_tools!();
    let f = Fixture::new("contract");
    f.verb_file(
        "wipe",
        json!({"about": "MUTATING — test fixture.", "hosts": ["forge"],
               "requires_approval": true, "plan_verb": "plan-wipe",
               "argv": ["true", "{1}"],
               "params": [{"name": "target", "pattern": "^[a-z-]{1,20}$"}]}),
    );
    f.packet(job("ready", approve_meta(None), json!([]), "pending"));
    let (out, writes) = f.run(&[]);
    let close = writes_to(&writes, "s-refused")
        .into_iter()
        .find(|w| w["body"]["status"] == "completed")
        .unwrap_or_else(|| panic!("not closed refused: {writes:?}\n{out}"));
    let reason = close["body"]["metadata"]["reason"]
        .as_str()
        .unwrap_or_default();
    assert!(reason.contains("plan_sha256"), "{reason}");
    assert!(
        !writes.iter().any(|w| w["method"] == "PATCH"),
        "no plan is rendered for a verb that cannot honour one: {writes:?}"
    );

    // No `plan_verb` at all: nothing to sign.
    f.verb_file(
        "wipe",
        json!({"about": "MUTATING — test fixture.", "hosts": ["forge"],
               "requires_approval": true, "argv": ["true", "{1}", "{2}"],
               "params": [{"name": "target", "pattern": "^[a-z-]{1,20}$"},
                          {"name": "plan_sha256", "pattern": "^[0-9a-f]{64}$"}]}),
    );
    let (out, writes) = f.run(&[]);
    let close = writes_to(&writes, "s-refused")
        .into_iter()
        .find(|w| w["body"]["status"] == "completed")
        .unwrap_or_else(|| panic!("not closed refused: {writes:?}\n{out}"));
    let reason = close["body"]["metadata"]["reason"]
        .as_str()
        .unwrap_or_default();
    assert!(reason.contains("plan_verb"), "{reason}");
}

/// The shipped approval verbs, through the real runner and the real
/// allowlist: the two whose scripts re-render and compare pass the
/// contract (their plan verbs are named, read-only, on the same host,
/// taking the write's params minus the hash), and `commission-a-disk` —
/// whose script takes no hash — is refused before any plan runs.
#[test]
fn the_shipped_approval_verbs_hold_the_contract_and_the_disk_verb_stays_inert() {
    needs_tools!();
    let root = repo_root();
    let read = |n: &str| -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(root.join(format!("infra/ops/verbs/{n}.json"))).unwrap(),
        )
        .unwrap()
    };
    let names = |v: &Value| -> Vec<String> {
        v["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect()
    };
    for (write, plan) in [
        ("reap-terminated-pods", "plan-a-pod-reap"),
        ("merge-tenant-main", "plan-a-tenant-merge"),
    ] {
        let w = read(write);
        let p = read(plan);
        assert_eq!(w["plan_verb"], plan, "{write} names its plan verb");
        assert_eq!(
            w["approvers"],
            json!([APPROVER]),
            "{write} names who may approve it, by employee id (design 03451237 q2)"
        );
        assert_ne!(p["requires_approval"], true, "{plan} is read-only");
        assert_eq!(w["hosts"], p["hosts"], "{write} and {plan} serve one host");
        let mut wn = names(&w);
        assert_eq!(wn.pop().as_deref(), Some("plan_sha256"), "{write}");
        assert_eq!(wn, names(&p), "{plan} renders from exactly {write}'s args");
    }
    assert_eq!(
        read("commission-a-disk")["plan_verb"],
        "plan-a-disk-commission"
    );

    // The disk verb, through the runner with the shipped allowlist.
    let f = Fixture::new("disk-inert");
    std::fs::remove_dir_all(&f.verbs).unwrap();
    std::fs::create_dir_all(&f.verbs).unwrap();
    for e in std::fs::read_dir(root.join("infra/ops/verbs")).unwrap() {
        let p = e.unwrap().path();
        std::fs::copy(&p, f.verbs.join(p.file_name().unwrap())).unwrap();
    }
    let mut j = job("ready", approve_meta(None), json!([]), "pending");
    j["metadata"]["verb"] = json!("commission-a-disk");
    j["metadata"]["args"] = json!(["/dev/disk/by-id/nvme-TEST-0000", "/srv/data"]);
    f.packet(j);
    let (out, writes) = f.run(&[]);
    let close = writes_to(&writes, "s-refused")
        .into_iter()
        .find(|w| w["body"]["status"] == "completed")
        .unwrap_or_else(|| panic!("commission-a-disk was not refused: {writes:?}\n{out}"));
    let reason = close["body"]["metadata"]["reason"]
        .as_str()
        .unwrap_or_default();
    assert!(
        reason.contains("commission-a-disk") && reason.contains("plan_sha256"),
        "{reason}"
    );
}

// ------------------------------------------------------------- execute

/// THE WHOLE POINT: a completed approve step carrying a fresh presence
/// stamp bound to the plan's shape runs the write — claimed `active`
/// BEFORE the argv runs, given sha256 of the signed plan as its last
/// arg, and completed `answered` with the hash it ran under on the step.
///
/// This is also the equality pin on the shape hash (§9a): the stamp is
/// bound with `boss_core::job::step_shape_hash` over a plan and a
/// procedure carrying quotes, a backslash, a tab and non-ASCII, and the
/// runner recomputes it in jq. If the two ever disagree, every real
/// approval is refused, and this case goes red first.
#[test]
fn a_fresh_presence_approval_bound_to_the_plan_runs_the_write_once() {
    needs_tools!();
    let f = Fixture::new("approved");
    f.packet(approved_job());
    let (out, writes) = f.run(&[]);
    let plan_sha = sha256_hex(PLAN.as_bytes());
    assert_eq!(
        f.applied(),
        Some(vec!["target-a".to_string(), plan_sha.clone()]),
        "the write runs with the request's args and the SIGNED plan's hash: {out}"
    );
    // THE CLAIM IS A COMPARE-AND-SET (security review, 2026-09-24). It
    // was a PUT of `{"status":"active"}`, which the server overlays on
    // whatever the step is — so two passes that both read `ready` both
    // "claimed". It goes through the claim door now, whose WHERE clause
    // admits exactly one ready→active, and it is signed as a claimant
    // unique to this pass: the door is idempotent for its holder, so a
    // second pass signing as the same account would be let through.
    let exec = writes_to(&writes, "s-execute");
    let claim = exec
        .first()
        .unwrap_or_else(|| panic!("execute was never claimed: {writes:?}\n{out}"));
    assert_eq!(claim["method"], "POST", "{claim}");
    assert!(
        claim["url"]
            .as_str()
            .unwrap()
            .ends_with("/api/jobs/aaaaaaaa-0000-4000-8000-000000000000/steps/s-execute/claim"),
        "execute is claimed through the claim door before anything else is written to it: {claim}"
    );
    let claimant = claim["user"]["id"].as_str().unwrap_or_default().to_string();
    assert!(
        claimant.starts_with("automation:ops-runner:forge:") && claimant.len() > 28,
        "the claimant names the runner, its host and this pass: {claim}"
    );
    assert!(
        !exec
            .iter()
            .any(|w| w["body"] == json!({"status": "active"})),
        "no PUT claims by overlay: {writes:?}"
    );
    // A second pass is a second claimant, so the door's holder-idempotence
    // cannot hand it the same claim.
    let (_, writes2) = f.run(&[]);
    let claimant2 = writes_to(&writes2, "s-execute")
        .first()
        .map(|w| w["user"]["id"].as_str().unwrap_or_default().to_string());
    assert_ne!(claimant2.as_deref(), Some(claimant.as_str()), "{writes2:?}");
    let before: usize = std::fs::read_to_string(f.root.join("writes-before-apply"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        before, 1,
        "the claim is the one write made BEFORE the argv runs: {writes:?}"
    );
    let md = execute_completion(&writes, &out);
    assert_eq!(md["disposition"], "answered", "{md}\n{out}");
    assert_eq!(md["exit_code"], "0", "{md}");
    assert_eq!(
        md["approved_plan_sha256"], plan_sha,
        "the step says which signed plan it ran under: {md}"
    );
}

/// THE d5efbb3c SHAPE (2026-09-22): the first presence-assured step in
/// the system completed with `sign_offs: []` and no ceremony, and the
/// host then ran the verb. A completed step is not an approval.
#[test]
fn a_completed_approve_step_with_no_stamp_is_refused() {
    needs_tools!();
    let f = Fixture::new("forged-no-stamp");
    f.packet(job(
        "completed",
        approve_meta(Some(PLAN)),
        json!([]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["wipe", "no platform-admin sign-off"]);

    // And the packet exactly as it was: the step required no sign-off at
    // all, so there was nothing a stamp could have been checked against.
    let f = Fixture::new("forged-d5efbb3c");
    let mut j = job("completed", approve_meta(Some(PLAN)), json!([]), "ready");
    j["steps"][1]["sign_offs_required"] = json!([]);
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["requires no sign-off"]);
}

/// A REJECTION IS NOT AN APPROVAL (adversarial re-review, 2026-09-25).
/// Reject, on either surface, runs the SAME ceremony as Approve — the
/// decision saved, a presence stamp bound to the shape that carries it,
/// the step completed — so a rejected plan arrives as a completed approve
/// step with a valid, fresh, bound, named-approver presence stamp, which
/// is every check the runner made. Reproduced on the car's own runner:
/// the write ran. The decision is inside the signed shape, so reading it
/// is reading what the approver signed; only the exact string `approved`
/// is one, and anything else — another decision, none, or a value that
/// is not a string — runs nothing and is refused naming the decision.
#[test]
fn a_rejected_approval_runs_nothing() {
    needs_tools!();
    let decisions = [
        ("rejected", Some(json!("rejected"))),
        ("changes-requested", Some(json!("changes-requested"))),
        ("pending", Some(json!("pending"))),
        ("approved-with-space", Some(json!("approved "))),
        ("absent", None),
        ("bool", Some(json!(true))),
        ("list", Some(json!(["approved"]))),
        ("object", Some(json!({"decision": "approved"}))),
        ("null", Some(Value::Null)),
    ];
    for (name, decision) in decisions {
        let f = Fixture::new(&format!("decision-{name}"));
        let mut meta = approve_meta(Some(PLAN));
        match &decision {
            Some(d) => meta["decision"] = d.clone(),
            None => {
                meta.as_object_mut().unwrap().remove("decision");
            }
        }
        // The stamp is bound to the step AS IT STANDS, decision and all:
        // every other check passes, so only the decision can refuse it.
        let shape = shape_of(&meta);
        f.packet(job(
            "completed",
            meta,
            json!([stamp("platform-admin", "presence", &shape, 60)]),
            "ready",
        ));
        let (out, writes) = f.run(&[]);
        let shown = match &decision {
            Some(d) => d.to_string(),
            None => "none".to_string(),
        };
        assert_refused(&f, &out, &writes, &["wipe", "decision", &shown]);
    }
}

/// A session-assured stamp is someone logged in; it is not a passkey.
#[test]
fn a_session_assured_stamp_is_refused() {
    needs_tools!();
    let f = Fixture::new("forged-session");
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("platform-admin", "session", &shape, 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["session", "presence"]);

    // The assurance is judged on its own: a session stamp that somehow
    // carries a nonce is still a session stamp.
    let f = Fixture::new("forged-session-nonce");
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    let mut s = stamp("platform-admin", "session", &shape, 60);
    s["presence_nonce"] = json!("nonce-1");
    f.packet(job("completed", meta, json!([s]), "ready"));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["session", "presence"]);
}

/// A declaration the runner cannot read as a plain `false` is read as
/// requiring an approval: a typo in a reviewed file (`"true"`, a
/// string) must not quietly turn the gate off.
#[test]
fn a_requires_approval_that_is_not_a_boolean_still_requires_one() {
    needs_tools!();
    let f = Fixture::new("non-boolean");
    let apply: Value =
        serde_json::from_str(&std::fs::read_to_string(f.verbs.join("wipe.json")).unwrap()).unwrap();
    let mut spec = apply.clone();
    spec["requires_approval"] = json!("true");
    f.verb_file("wipe", spec);
    let mut j = job("pending", approve_meta(None), json!([]), "ready");
    j["metadata"] = json!({"host": "forge", "verb": "wipe", "args": ["target-a"]});
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["requires a passkey approval"]);
}

/// A presence stamp for another role does not stand in for the role the
/// step requires.
#[test]
fn a_presence_stamp_for_another_role_is_refused() {
    needs_tools!();
    let f = Fixture::new("forged-role");
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("finance-admin", "presence", &shape, 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["platform-admin"]);
}

/// MISMATCHED HASH: the plan on the step is not the plan that was
/// signed. The stamp is bound to the shape of a DIFFERENT plan, so the
/// passkey never saw these bytes.
#[test]
fn a_stamp_bound_to_a_different_plan_is_refused() {
    needs_tools!();
    let f = Fixture::new("mismatched-shape");
    let signed = approve_meta(Some("PLAN wipe target-b\n"));
    let shape = shape_of(&signed);
    f.packet(job(
        "completed",
        approve_meta(Some(PLAN)),
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["not the plan that was signed", &shape]);
}

/// TIME-BOXED (q4): an approval older than the window is refused, and
/// says when it was signed and what the window is.
#[test]
fn an_approval_older_than_its_window_is_refused() {
    needs_tools!();
    let f = Fixture::new("expired");
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, 11 * 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["expires after 600s"]);
}

/// A stamp from this host's future is a clock nobody can reconcile, and
/// an approval that cannot be dated cannot be time-boxed.
#[test]
fn an_approval_stamped_in_the_future_is_refused() {
    needs_tools!();
    let f = Fixture::new("future");
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, -30 * 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["future"]);
}

/// CLAIMED, THEN SILENCE: an execute step already `active` was claimed by
/// an earlier pass that never answered — it died, or its host did.
/// Single-use means that claim was the one use, so this pass never runs
/// the write again. And it does not know whether the write ran, so it
/// must not SAY it did not: closing the request `refused` told a reader
/// "nothing ran" about a write that may have (security review,
/// 2026-09-24). The request is held open, active, looking troubled, with
/// "claimed, outcome unknown" on it — once, not every minute.
#[test]
fn an_execute_claimed_by_a_pass_that_never_answered_records_claimed_outcome_unknown() {
    needs_tools!();
    let f = Fixture::new("claimed-unknown");
    let mut j = approved_job();
    j["steps"][2]["status"] = json!("active");
    j["steps"][2]["assignee_id"] = json!("automation:ops-runner:forge:20260924T101500Z-4242");
    f.packet(j.clone());
    let (out, writes) = f.run(&[]);
    assert!(
        f.applied().is_none(),
        "a claimed execute is never run again: {out}"
    );
    assert!(
        !writes_to(&writes, "s-execute")
            .iter()
            .any(|w| w["body"]["status"] == "completed"),
        "the step is not answered — nobody knows the answer: {writes:?}\n{out}"
    );
    assert!(
        writes_to(&writes, "s-refused").is_empty(),
        "and the request is NOT closed refused, which would say nothing ran: {writes:?}"
    );
    let note = writes
        .iter()
        .find(|w| {
            w["method"] == "PATCH"
                && w["url"].as_str().is_some_and(|u| {
                    u.ends_with("/api/jobs/aaaaaaaa-0000-4000-8000-000000000000/metadata")
                })
        })
        .unwrap_or_else(|| panic!("nothing was recorded on the request: {writes:?}\n{out}"));
    let rec = &note["body"]["execute_outcome_unknown"];
    assert_eq!(rec["state"], "claimed, outcome unknown", "{note}");
    assert_eq!(
        rec["claimed_by"], "automation:ops-runner:forge:20260924T101500Z-4242",
        "it names the pass that claimed: {note}"
    );
    assert!(
        rec["reason"].as_str().unwrap_or_default().contains("wipe"),
        "{note}"
    );
    assert!(out.contains("claimed, outcome unknown"), "{out}");
    assert!(out.contains("held=1"), "{out}");

    // Recorded once: a second pass over the same record writes nothing.
    j["metadata"]["execute_outcome_unknown"] = rec.clone();
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert!(
        writes.is_empty(),
        "the note is written once: {writes:?}\n{out}"
    );
    assert!(f.applied().is_none(), "{out}");
    assert!(out.contains("held=1"), "{out}");
}

/// A METADATA FLAG IS NEVER AN APPROVAL (q2). A request filed without
/// `requires_approval` never makes its approve step ready, so execute is
/// ready at once — and a job-level `approved: true` beside it proves
/// nothing. The runner reads the approval requirement off the VERB and
/// the approval off the SoR's sign-off record, never off the packet.
#[test]
fn a_request_carrying_a_flag_instead_of_a_signature_is_refused() {
    needs_tools!();
    let f = Fixture::new("flag");
    let mut j = job("pending", approve_meta(None), json!([]), "ready");
    j["metadata"] =
        json!({"host": "forge", "verb": "wipe", "args": ["target-a"], "approved": true});
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["pending", "requires_approval"]);
}

/// A filer cannot supply the hash: the runner appends the SIGNED plan's
/// hash itself, and an arg in that position is refused rather than
/// shifted or trusted.
#[test]
fn a_request_that_names_its_own_plan_hash_is_refused() {
    needs_tools!();
    let f = Fixture::new("filer-hash");
    let mut j = approved_job();
    j["metadata"]["args"] = json!(["target-a", "0".repeat(64)]);
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["plan_sha256"]);
}

/// A claim the server refuses runs nothing: the claim is what makes the
/// approval single-use, so without it there is no run.
#[test]
fn a_refused_claim_runs_nothing() {
    needs_tools!();
    let f = Fixture::new("claim-refused");
    f.packet(approved_job());
    let (out, writes) = f.run(&[("STUB_REFUSE_CLAIM", "1")]);
    assert!(
        f.applied().is_none(),
        "the write ran without its claim: {out}"
    );
    assert!(
        !writes_to(&writes, "s-execute")
            .iter()
            .any(|w| w["body"]["status"] == "completed"),
        "an unclaimed approval is left for the next pass, not answered: {writes:?}"
    );
    assert!(out.contains("failed=1"), "and the unit goes red: {out}");
}

// ------------------------------------------- security review, 2026-09-24

/// WHO MAY APPROVE IS A NAMED LIST OF EMPLOYEES, not a role (design
/// 03451237 q2, David 2026-09-22: a role is registry data, so a role
/// gate makes the approval depend on whoever can write a policy row).
/// A presence stamp for the right role, bound to the right shape, fresh —
/// by someone the verb file does not name — approves nothing.
#[test]
fn a_presence_stamp_by_someone_not_named_as_an_approver_is_refused() {
    needs_tools!();
    let f = Fixture::new("not-an-approver");
    let meta = approve_meta(Some(PLAN));
    let shape = shape_of(&meta);
    let mut s = stamp("platform-admin", "presence", &shape, 60);
    s["authority_id"] = json!("emp-mallory");
    f.packet(job("completed", meta, json!([s]), "ready"));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["emp-mallory", "approvers", APPROVER]);
}

/// A verb that names no approver has nobody whose passkey could carry
/// it, so it is refused before any plan is rendered — the same place a
/// verb with no plan verb is.
#[test]
fn an_approval_verb_that_names_no_approver_is_refused_before_any_plan() {
    needs_tools!();
    let f = Fixture::new("no-approvers");
    let mut spec: Value =
        serde_json::from_str(&std::fs::read_to_string(f.verbs.join("wipe.json")).unwrap()).unwrap();
    for bad in [json!(null), json!([]), json!("emp-david"), json!([""])] {
        if bad.is_null() {
            spec.as_object_mut().unwrap().remove("approvers");
        } else {
            spec["approvers"] = bad.clone();
        }
        f.verb_file("wipe", spec.clone());
        f.packet(job("ready", approve_meta(None), json!([]), "pending"));
        let (out, writes) = f.run(&[]);
        let close = writes_to(&writes, "s-refused")
            .into_iter()
            .find(|w| w["body"]["status"] == "completed")
            .unwrap_or_else(|| panic!("approvers {bad} was not refused: {writes:?}\n{out}"));
        let reason = close["body"]["metadata"]["reason"]
            .as_str()
            .unwrap_or_default();
        assert!(reason.contains("approvers"), "{bad}: {reason}");
        assert!(
            !writes.iter().any(|w| w["method"] == "PATCH"),
            "{bad}: no plan is rendered for a verb nobody may approve: {writes:?}"
        );
    }
}

/// THE SHAPE HASH ENCODES ITS KEYS (security review, 2026-09-24). Keys
/// were written raw — `key:value,` — so `{"zz": 1, "zzz": 2}` and
/// `{"zz:1,zzz": 2}` both canonicalised to `{…zz:1,zzz:2,}`: a stamp over
/// one step shape was a stamp over another. Both definitions JSON-encode
/// a key now, and this case holds them to it end to end: a stamp the
/// SERVER bound to one of the pair must not verify on a step carrying
/// the other.
#[test]
fn a_stamp_does_not_carry_over_to_a_step_whose_keys_collide_with_its_shape() {
    needs_tools!();
    let f = Fixture::new("key-collision");
    let mut signed = approve_meta(Some(PLAN));
    signed["zz"] = json!(1);
    signed["zzz"] = json!(2);
    let mut carried = approve_meta(Some(PLAN));
    carried["zz:1,zzz"] = json!(2);
    let shape = shape_of(&signed);
    f.packet(job(
        "completed",
        carried,
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["not the plan that was signed"]);
}

/// The other half of the §9a pin: over keys that NEED encoding — a
/// quote, a tab, a colon, non-ASCII — the runner's jq and the server's
/// Rust still agree, so an honest approval is not refused.
#[test]
fn the_runners_shape_hash_agrees_with_the_servers_over_keys_that_need_encoding() {
    needs_tools!();
    let f = Fixture::new("key-encoding");
    let mut meta = approve_meta(Some(PLAN));
    meta["a \"quoted\"\tkey: é,"] = json!({"inner \\ key": [1, "two"]});
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    ));
    let (out, _) = f.run(&[]);
    assert_eq!(
        f.applied(),
        Some(vec!["target-a".to_string(), sha256_hex(PLAN.as_bytes())]),
        "an honest approval over encoded keys runs: {out}"
    );
}

/// THE REQUEST IS SIGNED, NOT ONLY ITS PLAN. The runner wrote the verb,
/// host and args onto the approve step when it rendered, so they are in
/// the signed shape; the request's own metadata stays writable. A
/// request whose args (or verb) moved after the signature is refused
/// before any argv is built — naming both.
#[test]
fn a_request_that_moved_after_its_signature_is_refused() {
    needs_tools!();
    let f = Fixture::new("moved-args");
    let mut j = approved_job();
    j["metadata"]["args"] = json!(["target-b"]);
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["target-b", "target-a", "signed"]);

    // A plan rendered before this car carries no verb/host/args at all:
    // there is nothing signed to compare the request against.
    let f = Fixture::new("unsigned-request");
    let mut meta = approve_meta(Some(PLAN));
    for k in ["verb", "host", "args"] {
        meta.as_object_mut().unwrap().remove(k);
    }
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &["signed"]);
}

/// A PLAN THE RUNNER DID NOT WRITE IS REFUSED. Two checks. The step must
/// carry the hash of the bytes the runner rendered, and the plan on it
/// must hash to that. And — the one a forger cannot satisfy by writing
/// both fields — the runner renders the plan AGAIN on the host, from the
/// signed args, and the signed plan must be those bytes exactly. A plan
/// typed onto the step by anyone else, however consistent, is not.
#[test]
fn a_plan_the_runner_did_not_render_is_refused() {
    needs_tools!();
    // The rendered hash is missing, or names other bytes.
    for rendered in [json!(null), json!("0".repeat(64))] {
        let f = Fixture::new("not-rendered-hash");
        let mut meta = approve_meta(Some(PLAN));
        if rendered.is_null() {
            meta.as_object_mut().unwrap().remove("rendered_plan_sha256");
        } else {
            meta["rendered_plan_sha256"] = rendered.clone();
        }
        let shape = shape_of(&meta);
        f.packet(job(
            "completed",
            meta,
            json!([stamp("platform-admin", "presence", &shape, 60)]),
            "ready",
        ));
        let (out, writes) = f.run(&[]);
        assert_refused(&f, &out, &writes, &["rendered_plan_sha256"]);
    }

    // A forged plan, internally consistent and properly signed: the
    // runner's own re-render says otherwise.
    let f = Fixture::new("forged-plan");
    let forged = "PLAN wipe nothing at all\n";
    let meta = approve_meta(Some(forged));
    let shape = shape_of(&meta);
    f.packet(job(
        "completed",
        meta,
        json!([stamp("platform-admin", "presence", &shape, 60)]),
        "ready",
    ));
    let (out, writes) = f.run(&[]);
    assert_refused(
        &f,
        &out,
        &writes,
        &[
            "plan-wipe",
            &sha256_hex(forged.as_bytes()),
            &sha256_hex(PLAN.as_bytes()),
        ],
    );
}

/// RE-READ IMMEDIATELY BEFORE THE CLAIM. The runner judged the approval
/// on the list it read at the top of the pass; by the time it reaches
/// this request another pass may have claimed it, or the approval may
/// be gone. It reads the one job again and judges it again, and a record
/// that moved is left for the next pass — no claim, no write, nothing
/// run, and nothing written that a stale read decided.
#[test]
fn an_approval_that_moved_before_the_claim_is_not_claimed() {
    needs_tools!();
    // Another pass claimed execute in between.
    let f = Fixture::new("reread-claimed");
    f.packet(approved_job());
    let mut moved = approved_job();
    moved["steps"][2]["status"] = json!("active");
    f.reread(moved);
    let (out, writes) = f.run(&[]);
    assert!(f.applied().is_none(), "{out}");
    assert!(
        writes.is_empty(),
        "a moved record is written nothing: {writes:?}\n{out}"
    );
    assert!(out.contains("moved"), "{out}");

    // The stamp was withdrawn (the step reopened) in between.
    let f = Fixture::new("reread-unsigned");
    f.packet(approved_job());
    let mut moved = approved_job();
    moved["steps"][1]["status"] = json!("ready");
    moved["steps"][1]["sign_offs"] = json!([]);
    f.reread(moved);
    let (out, writes) = f.run(&[]);
    assert!(f.applied().is_none(), "{out}");
    assert!(writes.is_empty(), "{writes:?}\n{out}");

    // Someone was nominated to execute in between: the claim door would
    // refuse this pass, so it is not attempted on a stale read.
    let f = Fixture::new("reread-nominated");
    f.packet(approved_job());
    let mut moved = approved_job();
    moved["steps"][2]["assignee_id"] = json!(NOMINEE);
    f.reread(moved);
    let (out, writes) = f.run(&[]);
    assert!(f.applied().is_none(), "{out}");
    assert!(writes.is_empty(), "{writes:?}\n{out}");
    assert!(out.contains(NOMINEE), "the move names the holder: {out}");
}

// --------------------------------------------- security re-review, 2026-09-25

/// THE BLOCKER, AS THE LIVE SYSTEM HAD IT: every ops-request execute was
/// nominated to the agent executor ~50ms after it went ready, and the
/// claim door admits a ready step only to an unheld row or its holder.
/// The fixture's execute is now dispatched by the tree's own protocol
/// (unheld, because it declares `claimable`), and this is the other
/// arm — a request whose execute someone holds, as every v2 request's
/// did. Its claim could never be taken, so the runner does not try: it
/// refuses the request by name, once, instead of failing a claim every
/// minute until the approval expires. Nothing runs.
#[test]
fn an_approved_execute_someone_else_was_assigned_is_refused_by_name() {
    needs_tools!();
    let f = Fixture::new("nominated");
    let mut j = approved_job();
    j["steps"][2]["assignee_id"] = json!(NOMINEE);
    f.packet(j);
    let (out, writes) = f.run(&[]);
    assert_refused(&f, &out, &writes, &[NOMINEE, "claim", "unheld"]);
    assert!(
        !writes
            .iter()
            .any(|w| w["url"].as_str().is_some_and(|u| u.ends_with("/claim"))),
        "a claim the door must refuse is not attempted: {writes:?}"
    );
}

/// THE STUB'S CLAIM DOOR IS THE SERVER'S RULE. Called directly, the way
/// the runner calls it: a ready step is admitted to an unheld row or its
/// holder, an active one only to its holder, and anything else is 409
/// naming the holder — `claim_step_at`'s WHERE clause. The always-200
/// door this replaced is why the blocker passed green.
#[test]
fn the_stubbed_claim_door_admits_exactly_what_the_servers_does() {
    needs_tools!();
    let f = Fixture::new("claim-door");
    let claim = |status: &str, holder: Value, claimant: &str| -> (String, String) {
        let mut j = approved_job();
        j["steps"][2]["status"] = json!(status);
        j["steps"][2]["assignee_id"] = holder;
        f.packet(j);
        std::fs::write(f.root.join("writes.jsonl"), "").unwrap();
        let body = f.root.join("claim-body");
        let out = Command::new(f.root.join("bin/curl"))
            .args([
                "-sS",
                "-o",
                body.to_str().unwrap(),
                "-w",
                "%{http_code}",
                "-X",
                "POST",
                "-H",
                &format!("x-boss-user: {}", json!({"id": claimant})),
                "http://sor.invalid/api/jobs/aaaaaaaa-0000-4000-8000-000000000000/steps/s-execute/claim",
            ])
            .env("STUB_DIR", &f.root)
            .output()
            .expect("the stub runs");
        (
            String::from_utf8_lossy(&out.stdout).to_string(),
            std::fs::read_to_string(&body).unwrap_or_default(),
        )
    };
    let me = "automation:ops-runner:forge:20260925T010203Z-77";
    assert_eq!(claim("ready", Value::Null, me).0, "200");
    assert_eq!(claim("ready", json!(me), me).0, "200");
    assert_eq!(claim("active", json!(me), me).0, "200");
    let (code, body) = claim("ready", json!(NOMINEE), me);
    assert_eq!(code, "409", "{body}");
    assert!(
        body.contains(NOMINEE),
        "the refusal names the holder: {body}"
    );
    assert_eq!(
        claim("active", json!("automation:ops-runner:forge:other"), me).0,
        "409"
    );
    assert_eq!(claim("active", Value::Null, me).0, "409");
    assert_eq!(claim("completed", Value::Null, me).0, "409");
}

/// CLAIMED, OUTCOME UNKNOWN IS A RUNNER PASS'S RECORD ONLY (re-review of
/// 2026-09-25). An `active` execute held by a pass of this runner may
/// have run its write, so it is recorded as unknown and never run again.
/// One held by anyone else was not taken by this runner's claim — the
/// only way this runner runs a write — so nothing ran here, and calling
/// it "outcome unknown" would say a write might have happened that could
/// not have. It is refused by name.
#[test]
fn an_active_execute_held_by_anyone_but_a_runner_pass_is_refused_by_name() {
    needs_tools!();
    for holder in [
        json!(NOMINEE),
        json!("emp-david"),
        // Runner-shaped for ANOTHER host, or not a pass id at all.
        json!("automation:ops-runner:boss-gcp:20260924T101500Z-4242"),
        json!("automation:ops-runner:forge:whenever"),
        json!(null),
    ] {
        let f = Fixture::new("active-other");
        let mut j = approved_job();
        j["steps"][2]["status"] = json!("active");
        j["steps"][2]["assignee_id"] = holder.clone();
        f.packet(j);
        let (out, writes) = f.run(&[]);
        let name = holder.as_str().unwrap_or("no recorded claimant");
        assert_refused(
            &f,
            &out,
            &writes,
            &[name, "not a pass of this runner", "unknown"],
        );
        // AN UNRECOGNISED HOLDER IS NOT PROOF NOTHING RAN (adversarial
        // re-review, 2026-09-25): a pass of this runner signed under an
        // earlier BOSS_OPS_ACTOR, or a runner on another host, is exactly
        // such a holder. The refusal says what it cannot know.
        let reason = execute_completion(&writes, &out)["reason"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase();
        assert!(
            !reason.contains("nothing ran"),
            "{holder}: the runner cannot know nothing ran under a claim it does not recognise: {reason}"
        );
        assert!(
            !writes
                .iter()
                .any(|w| !w["body"]["execute_outcome_unknown"].is_null()),
            "{holder}: outcome unknown is recorded only for a runner pass: {writes:?}"
        );
    }
}
