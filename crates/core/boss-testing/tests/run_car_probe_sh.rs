//! `infra/forge/run-car-probe.sh` runs a car's recorded probe on the
//! FORGE HOST. The probe was written on the dev pod, which is a
//! different machine with different tools — so a probe can be correct
//! and unrunnable, and this test pins the mechanism that tells those
//! two apart.
//!
//! THE DEFECT (backlog f9304366, measured 2026-09-09). The auto-proof
//! loop's first live run: a train arrived, the rule filed one
//! ops-request per probed car, the forge's ops-runner answered both,
//! and NEITHER car proved. Both probes were
//! `kubectl -n boss-dev exec deploy/boss-conductor -- …`, which is
//! right from the pod and impossible from the forge (outside the
//! cluster, no kubeconfig). The recorded evidence was
//! `{"exit": 1, "output": ""}` — an exit code with empty streams, which
//! reads exactly like a false claim.
//!
//! The reason the streams were empty is the whole lesson: the probes
//! swallowed their own diagnostics. `kubectl … 2>&1 | grep -q …` sends
//! the shell's own `kubectl: command not found` INTO the pipe, where
//! grep eats it. Nothing run-car-probe.sh does with stderr can recover
//! a message the probe redirected away from it.
//!
//! So the script hands the probe's shell a `command_not_found_handle`
//! and a file descriptor the probe's redirections cannot reach (fd 9,
//! opened before the probe's own text runs). Every command bash could
//! not find is recorded there, and the attempt says `unrunnable` with
//! the tool named — CLAUDE.md §Diagnosis, a verdict must name what
//! failed.
//!
//! This test RUNS that prelude, extracted verbatim from the script, so
//! the mechanism cannot rot into a comment. It needs bash and nothing
//! else — the script's own jq/curl path is covered by the record-shape
//! pin in boss-cli's prove.rs.
//!
//! The second half of the file covers the OTHER way a probe can be
//! correct and worthless: reading the system of record unidentified, so
//! an absence assertion passes against a page it was never allowed to
//! see (61085a9e). Both are the same family — a probe whose evidence
//! cannot be told apart from a false claim.

use boss_testing::repo_root;
use std::path::PathBuf;
use std::process::Command;

const BEGIN: &str = "# PROBE-PRELUDE-BEGIN";
const END: &str = "# PROBE-PRELUDE-END";

/// The prelude the script wraps every probe in, lifted from the script
/// itself between its two markers. One definition, executed here.
fn probe_prelude() -> String {
    let sh = std::fs::read_to_string(repo_root().join("infra/forge/run-car-probe.sh"))
        .expect("run-car-probe.sh is readable");
    let after = sh
        .split_once(BEGIN)
        .unwrap_or_else(|| panic!("run-car-probe.sh has no {BEGIN} marker"))
        .1;
    let body = after
        .split_once(END)
        .unwrap_or_else(|| panic!("run-car-probe.sh has no {END} marker"))
        .0;
    assert!(
        body.contains("command_not_found_handle"),
        "the extracted prelude does not install a command_not_found_handle:\n{body}"
    );
    body.to_string()
}

fn scratch(case: &str) -> PathBuf {
    // Per-uid and per-process, and it REFUSES by name if a
    // leftover cannot be cleared — see `boss_testing::scratch`.
    boss_testing::scratch_dir(&format!("run-car-probe-sh-{case}"))
}

/// Run one probe exactly as the script runs it: the prelude, then the
/// probe's own text, in one `bash -c`, with the not-found file named in
/// the environment. Returns (exit code, stdout, stderr, not-found log).
fn run_probe(case: &str, probe: &str) -> (i32, String, String, String) {
    let dir = scratch(case);
    let notfound = dir.join("notfound");
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!("{}{probe}", probe_prelude()))
        .env("BOSS_PROBE_NOTFOUND", &notfound)
        .current_dir(&dir)
        .output()
        .expect("bash runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        std::fs::read_to_string(&notfound).unwrap_or_default(),
    )
}

/// THE MEASURED CASE, verbatim in shape: a probe that pipes its own
/// stderr into a grep. The tool is missing, the streams the script
/// captures are empty — and the not-found channel still names it.
#[test]
fn a_missing_tool_is_named_even_when_the_probe_swallows_its_own_stderr() {
    let (rc, stdout, stderr, notfound) = run_probe(
        "swallowed",
        "kubectl-no-such-tool -n boss-dev get pods 2>&1 | grep -q Running \
         && echo PROBE_OK || echo PROBE_MISSING",
    );
    // What the script would have recorded before: exit + streams that
    // say nothing about why.
    assert_eq!(rc, 0, "the probe's own `|| echo` masks the exit code");
    assert!(stdout.contains("PROBE_MISSING"), "stdout: {stdout}");
    assert!(
        !stderr.contains("kubectl-no-such-tool"),
        "the probe redirected its diagnostics into the pipe; stderr: {stderr}"
    );
    // What it records now: the tool, on a channel the probe cannot
    // redirect.
    assert!(
        notfound.contains("kubectl-no-such-tool"),
        "the not-found channel must name the missing tool, got: {notfound:?}"
    );
}

/// The other live shape: an `&&` chain whose first link is missing, so
/// the probe exits 1 with nothing on either stream. Exit 1 and silence
/// is indistinguishable from a false claim — unless the tool is named.
#[test]
fn an_and_chain_that_dies_on_a_missing_tool_still_names_it() {
    let (rc, stdout, stderr, notfound) = run_probe(
        "and-chain",
        "kubectl-no-such-tool get cm 2>&1 | grep -q x && echo PROBE_OK",
    );
    assert_eq!(rc, 1, "the chain stops at the missing link");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert!(
        notfound.contains("kubectl-no-such-tool"),
        "the not-found channel must name the missing tool, got: {notfound:?}"
    );
}

/// A probe that does NOT redirect keeps bash's own message on stderr —
/// the prelude adds a channel, it does not take one away.
#[test]
fn the_prelude_still_prints_command_not_found_on_stderr() {
    let (_, _, stderr, notfound) = run_probe("plain", "kubectl-no-such-tool version");
    assert!(
        stderr.contains("kubectl-no-such-tool") && stderr.contains("command not found"),
        "stderr must still carry the shell's message, got: {stderr:?}"
    );
    assert!(notfound.contains("kubectl-no-such-tool"), "{notfound:?}");
}

/// A probe whose tools all exist records NOTHING on the not-found
/// channel — so `unrunnable` cannot be stamped on a claim that simply
/// turned out to be false.
#[test]
fn a_runnable_probe_leaves_the_not_found_channel_empty() {
    let (rc, stdout, _, notfound) = run_probe("runnable", "echo NOT_THE_WORD | grep -q TOKEN");
    assert_eq!(rc, 1, "the probe ran and its assertion failed");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        notfound.trim().is_empty(),
        "a false claim must not look unrunnable, got: {notfound:?}"
    );
}

/// The prelude must not be able to take the probe down with it: with
/// no writable not-found file the probe still runs and still reports.
#[test]
fn an_unwritable_not_found_channel_does_not_break_the_probe() {
    let dir = scratch("unwritable");
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!("{}echo PROBE_TOKEN_OK", probe_prelude()))
        .env("BOSS_PROBE_NOTFOUND", "/proc/nonexistent/dir/notfound")
        .current_dir(&dir)
        .output()
        .expect("bash runs");
    assert!(out.status.success(), "the probe must still run");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("PROBE_TOKEN_OK"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// =====================================================================
// THE VERDICT NAMES ONE THING (4fccc595).
//
// THE DEFECT, measured 2026-09-11. Car a0ab90a5 sat unproven for 18
// hours, and what the record held was
// `{"exit": 1, "output": "", "why": "the probe RAN on
// david-asus-minipc and exited 1 — the claim it makes is not holding,
// or the probe is wrong."}`. Every word of that is true and none of it
// is actionable: the real cause was a probe that could not pass (`jq
// -e` over a filter whose success branch is `empty` exits 4, and a
// trailing `|| exit 1` rewrote the 4), and nothing in the record
// distinguished that from a regression, an unreachable service, or a
// policy-narrowed read. A verdict that lists possibilities is not a
// verdict (CLAUDE.md §Diagnosis).
//
// So the script's verdict block is lifted out between its markers and
// RUN here over all four outcomes. The block is shell, in one place, and
// this is what keeps it from drifting back into prose.
// =====================================================================

const VERDICT_BEGIN: &str = "# PROBE-VERDICT-BEGIN";
const VERDICT_END: &str = "# PROBE-VERDICT-END";

/// The verdict the script would record, for one outcome. Lifts the block
/// from the script, supplies exactly the variables the script has in
/// scope at that point, and prints `$why`.
fn verdict(
    case: &str,
    rc: i32,
    stdout: &str,
    stderr: &str,
    unrunnable: bool,
    expect: &str,
) -> String {
    verdict_and_flag(case, rc, stdout, stderr, unrunnable, expect).0
}

/// The verdict AND the `not_yet` flag the block sets beside it
/// (5461b899). The flag is what orient, the yard and the daily recheck
/// read; the sentence is what the operator reads. They are one record,
/// so they are derived in one block — and this lifts both, so a flag
/// that disagrees with its sentence fails here by name. Prints "unset"
/// for the flag when the block did not derive it.
fn verdict_and_flag(
    case: &str,
    rc: i32,
    stdout: &str,
    stderr: &str,
    unrunnable: bool,
    expect: &str,
) -> (String, String) {
    let sh = std::fs::read_to_string(repo_root().join("infra/forge/run-car-probe.sh"))
        .expect("run-car-probe.sh is readable");
    let block = sh
        .split_once(VERDICT_BEGIN)
        .unwrap_or_else(|| panic!("run-car-probe.sh has no {VERDICT_BEGIN} marker"))
        .1
        .split_once(VERDICT_END)
        .unwrap_or_else(|| panic!("run-car-probe.sh has no {VERDICT_END} marker"))
        .0;
    let dir = scratch(case);
    std::fs::write(dir.join("out"), stdout).expect("stdout fixture");
    std::fs::write(dir.join("errs"), stderr).expect("stderr fixture");
    let script = format!(
        "set -uo pipefail\n\
         workdir={dir}\n\
         rc={rc}\n\
         unrunnable={unrunnable}\n\
         missing_list='kubectl'\n\
         host='david-asus-minipc'\n\
         PROBE_USER=david\n\
         PROBE_DIR=/home/david/boss\n\
         expect={expect:?}\n\
         {block}\n\
         printf '%s\\n%s' \"$why\" \"${{not_yet-unset}}\"\n",
        dir = dir.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("bash runs the lifted verdict");
    assert!(
        out.status.success(),
        "the lifted verdict block must run: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8_lossy(&out.stdout).to_string();
    let (why, flag) = printed
        .rsplit_once('\n')
        .expect("why, then the not_yet flag");
    (why.to_string(), flag.to_string())
}

/// THE MEASURED RECORD, and the one branch that did not exist: a nonzero
/// exit with both streams empty is a MISSING RECORD, not a verdict on
/// the claim. The old wording asserted the claim might not be holding,
/// which sent a reader after a regression that was not there.
#[test]
fn a_nonzero_exit_with_nothing_printed_is_named_as_an_unreadable_failure() {
    let why = verdict("silent", 1, "", "", false, "SOME-TOKEN");
    assert!(
        why.contains("CANNOT BE READ") && why.contains("NOTHING on either stream"),
        "the verdict must name the empty record: {why}"
    );
    assert!(
        !why.contains("not holding, or the probe is wrong"),
        "the two-possibility wording is what cost 18 hours: {why}"
    );
    assert!(
        why.contains("4fccc595") && why.contains("jq -e"),
        "and point at the shape that produces it: {why}"
    );
}

/// A probe that DID speak gets its own words in the verdict — the first
/// line of stderr, because that is where jq, curl and bash put their
/// diagnosis. Nothing is guessed on top of it.
#[test]
fn a_probe_that_explained_itself_has_its_own_words_in_the_verdict() {
    let why = verdict(
        "spoke",
        5,
        "",
        "jq: error (at <stdin>:1): maintenance-backup: category=a sentence\n",
        false,
        "SOME-TOKEN",
    );
    assert!(why.contains("exited 5"), "{why}");
    assert!(
        why.contains("maintenance-backup: category=a sentence"),
        "the probe's own diagnosis is the verdict: {why}"
    );
    assert!(
        !why.contains("or the probe is wrong"),
        "one thing, not two: {why}"
    );
}

/// Exit 0 and the expectation absent is a third, different fact — and it
/// splits again on whether the probe printed anything at all.
#[test]
fn a_zero_exit_without_the_expected_token_says_what_was_printed_instead() {
    let printed = verdict("printed", 0, "dock_depth=0\n", "", false, "dock_depth=1");
    assert!(printed.contains("exited 0"), "{printed}");
    assert!(printed.contains("dock_depth=1"), "{printed}");
    assert!(
        printed.contains("dock_depth=0"),
        "what it printed instead is the evidence: {printed}"
    );

    let silent = verdict("zero-silent", 0, "", "", false, "dock_depth=1");
    assert!(
        silent.contains("printed NOTHING"),
        "a silent pass asserts nothing, and the verdict says so: {silent}"
    );
}

/// NOT YET (exit 75): the probe ran and said the world is not ready to
/// judge the claim. Four of the eight probes recorded on 2026-09-12 were
/// this shape ("no disk-report request carrying for_sweep yet — the
/// sweeps fire daily") and read as regressions. The verdict names it as
/// not-yet, carries the probe's own reason, and says the recheck re-runs
/// it — never "failed".
#[test]
fn an_exit_75_is_not_yet_and_carries_the_probes_reason() {
    let why = verdict(
        "not-yet",
        75,
        "no disk-report ops-request carrying for_sweep yet (0) — the three disk sweeps fire daily\n",
        "",
        false,
        "sweep-measured:ok",
    );
    assert!(why.starts_with("NOT YET"), "{why}");
    assert!(
        why.contains("carrying for_sweep yet"),
        "the probe's reason rides: {why}"
    );
    assert!(why.contains("recheck-failing-probes-daily"), "{why}");
    assert!(
        !why.contains("FAILED") && !why.contains("CANNOT BE READ"),
        "not a failure: {why}"
    );
    let (_, flag) = verdict_and_flag(
        "not-yet-flag",
        75,
        "not yet: no disk-report ops-request carrying for_sweep yet (0)\n",
        "",
        false,
        "sweep-measured:ok",
    );
    assert_eq!(flag, "true", "the NOT YET sentence carries not_yet=true");
}

/// AN EXIT 75 THAT SAID NOTHING IS NOT NOT-YET EITHER (backlog
/// 5461b899). The flag used to be computed after the block, from rc
/// alone — `rc -eq 75`, runnable, no crash — so an exit 75 with both
/// streams empty recorded THE FAILURE CANNOT BE READ beside
/// not_yet=true: orient and the yard read "not yet" while the sentence
/// said "missing record", and the daily recheck (scope failing /
/// not_yet) re-ran a missing record forever. `boss prove` derives the
/// flag from the sentence and cannot produce the disagreement; the
/// forge must not either. The flag is the sentence's: true in the one
/// branch that writes NOT YET, false everywhere else, derived nowhere
/// but in the block that writes the sentence.
#[test]
fn an_exit_75_with_nothing_printed_is_an_unreadable_failure_not_not_yet() {
    let (why, flag) = verdict_and_flag("silent-75", 75, "", "", false, "SOME-TOKEN");
    assert!(
        why.contains("CANNOT BE READ") && why.contains("exited 75"),
        "the verdict names the empty record: {why}"
    );
    assert!(
        !why.contains("cannot be judged"),
        "an empty record is not the not-yet sentence: {why}"
    );
    assert_eq!(
        flag, "false",
        "CANNOT BE READ must not ride with not_yet=true — that pair is what the recheck re-ran forever"
    );

    // The crash branch already refused the not-yet sentence (68081368);
    // its flag must refuse with it.
    let (why, flag) = verdict_and_flag(
        "crashed-75-flag",
        75,
        "not yet: no such packet\n",
        "bash: line 10: [: null: integer expression expected\n",
        false,
        "seen:ok",
    );
    assert!(why.starts_with("THE PROBE CRASHED"), "{why}");
    assert_eq!(flag, "false", "a crash that exited 75 is not not-yet");

    // Derived ONCE, where the sentence is: nothing after the block may
    // set the flag again from rc, or the two can disagree as they did.
    let sh = std::fs::read_to_string(repo_root().join("infra/forge/run-car-probe.sh"))
        .expect("run-car-probe.sh is readable");
    let after = sh.split_once(VERDICT_END).expect("the block ends").1;
    assert!(
        !after.contains("not_yet=true"),
        "not_yet is set only in the branch that writes the NOT YET sentence, not after the block from rc"
    );
    assert!(
        after.contains("[[ \"$not_yet\" == true ]] && exit 75"),
        "and the exit code follows the flag, so this record exits 1 (NOT PROVEN)"
    );
}

/// BUT AN EXIT 75 OVER A CRASH IS NOT NOT-YET (backlog 68081368). Cars
/// dd1d872d and 2e4d3bce recorded exactly the sentence above while
/// stderr held `bash: line 10: [: null: integer expression expected`:
/// jq printed null, `[` refused to compare it, and the `||` branch
/// meant for "no such packet yet" exited 75 on a crash. A recheck
/// re-runs a crash forever; the verdict must name it, quote the line,
/// and say which guard to add — before any not-yet wording.
#[test]
fn an_exit_75_whose_stderr_shows_a_crashed_numeric_test_names_the_crash() {
    let why = verdict(
        "crashed-not-yet",
        75,
        "not yet: no such packet\n",
        "bash: line 10: [: null: integer expression expected\n",
        false,
        "seen:ok",
    );
    assert!(why.starts_with("THE PROBE CRASHED"), "{why}");
    assert!(
        why.contains("[: null: integer expression expected"),
        "the stderr line is quoted, not pointed at: {why}"
    );
    assert!(
        why.contains("// empty") && why.contains("*[!0-9]*"),
        "the guard to add is named: {why}"
    );
    assert!(
        !why.contains("cannot be judged"),
        "the not-yet sentence is what sent two readers to stderr: {why}"
    );
}

/// AND THE BRANCH THAT ALREADY WORKED STILL WORKS. An unrunnable probe
/// names the tool and says the claim is untested (f9304366) — it is not
/// evidence against the change, and the other branches must not have
/// taken it over.
#[test]
fn an_unrunnable_probe_still_names_the_tool_and_not_the_claim() {
    let why = verdict("unrunnable", 1, "", "", true, "SOME-TOKEN");
    assert!(
        why.contains("DID NOT RUN") && why.contains("kubectl"),
        "{why}"
    );
    assert!(
        why.contains("says nothing about whether the change works"),
        "{why}"
    );
}

// =====================================================================
// A PROBE READS THE SYSTEM OF RECORD AS A NAMED READER (61085a9e).
//
// THE DEFECT, measured 2026-09-10 against one backend at one commit.
// The script builds the right `x-boss-user` header for its OWN three
// calls — read the car, write the verdict, patch the attempt — and
// hands the probe an env carrying only `BOSS_JOBS_URL`. So a probe
// doing `curl $BOSS_JOBS_URL/api/...` reads as `operator:unidentified`,
// and policy answers a NARROWER WORLD without saying so:
//
//   /api/jobs?kind=pr-train&status=open        operator 1  → 0
//   /api/jobs?kind=ship-a-change&status=open           21  → 0
//   /api/jobs?kind=backlog-item&status=open            30  → 0
//   /api/yard/status  trains/dock/recent/dock_depth 1/1/8/1 → all 0
//   /api/workflows                                     84  → 84 (same)
//
// Per-surface, with nothing in the answer to say which kind you hit.
// `/api/yard/status` comes back a confident, well-formed, completely
// idle yard: no trains, nothing on the dock, threshold not met.
//
// WHY THAT IS WORSE THAN A BROKEN READ. A PRESENCE assertion fails for
// the wrong reason and reads as a false negative about the car —
// recoverable, because someone investigates. An ABSENCE assertion
// PASSES FALSELY: "no open job of kind X remains" is green against an
// empty page the probe was never allowed to see. That is a proof that
// vouches for nothing, recorded as a proof, on a car that then closes.
// It caught the person who filed the item twice in one hour.
//
// THE FIX, and why it is not one line. Exporting the script's own
// `BOSS_USER` would hand every car's probe text a platform-admin
// credential with WRITE capability, and a probe is program text
// authored upstream and run as david on the forge. So the probe gets a
// SEPARATE, READ-SCOPED actor (`audit-readonly`: core policy grants it
// Read at Scope::All on every shipped resource and no other action at
// all) plus a GET-only reader on its PATH whose header comes from the
// runner, never from the probe's text.
// =====================================================================

const READER_BEGIN: &str = "# PROBE-READER-BEGIN";
const READER_END: &str = "# PROBE-READER-END";

fn script() -> String {
    std::fs::read_to_string(repo_root().join("infra/forge/run-car-probe.sh"))
        .expect("run-car-probe.sh is readable")
}

/// The read-scoped actor the runner builds for the probe, lifted from
/// between the script's markers. One definition, read here.
fn reader_actor_block() -> String {
    let sh = script();
    let after = sh
        .split_once(READER_BEGIN)
        .unwrap_or_else(|| panic!("run-car-probe.sh has no {READER_BEGIN} marker"))
        .1;
    after
        .split_once(READER_END)
        .unwrap_or_else(|| panic!("run-car-probe.sh has no {READER_END} marker"))
        .0
        .to_string()
}

/// The role the runner gives the probe's reader, READ OUT of the script
/// rather than asserted as a literal — so this test follows a rename
/// instead of going quiet on one.
fn reader_role() -> String {
    let block = reader_actor_block();
    let marker = "role";
    let start = block
        .find(marker)
        .unwrap_or_else(|| panic!("the reader actor names no role:\n{block}"))
        + marker.len();
    block[start..]
        .chars()
        .skip_while(|c| !c.is_ascii_alphanumeric())
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

/// The reader the probe is given, as the runner spells it on PATH.
const READER: &str = "boss-sor-read";

fn reader_path() -> PathBuf {
    repo_root().join("infra/forge/probe-bin").join(READER)
}

/// THE ENV THE PROBE GETS. It must carry a read-scoped actor, and it
/// must NOT carry the script's own platform-admin one: the probe runs
/// program text a builder wrote, and the privilege should match the job
/// — read the system of record, change nothing.
#[test]
fn the_probe_env_carries_a_read_scoped_actor_and_never_the_scripts_own() {
    let sh = script();
    // The two `env … bash -c "$probe_prelude$probe"` hand-offs: the
    // root path (runuser) and the by-hand path.
    let handoffs: Vec<&str> = sh
        .split("bash -c \"$probe_prelude$probe\"")
        .take(2)
        .collect();
    assert_eq!(
        handoffs.len(),
        2,
        "expected the two probe hand-offs (root + by-hand) in run-car-probe.sh"
    );
    for (which, segment) in ["root/runuser", "by-hand"].iter().zip(handoffs) {
        // Only the tail of each segment is the env list; take the last
        // 600 bytes so the header's prose cannot satisfy the assertion.
        let tail = &segment[segment.len().saturating_sub(600)..];
        assert!(
            tail.contains("BOSS_SOR_USER=\"$READER_USER\""),
            "the {which} hand-off does not export the read-scoped actor:\n{tail}"
        );
        assert!(
            !tail.contains(" BOSS_USER="),
            "the {which} hand-off exports the script's own platform-admin actor to the \
             probe — that is a WRITE credential handed to probe text:\n{tail}"
        );
        assert!(
            tail.contains("$PROBE_BIN"),
            "the {which} hand-off does not put the sanctioned reader on the probe's PATH:\n{tail}"
        );
    }
}

/// THE FACT THAT LIVES TWICE (CLAUDE.md §9a). The role is written in a
/// shell script and its meaning is defined in Rust; they cannot be
/// collapsed, so they are pinned equal. `audit-readonly` must grant
/// Read at Scope::All on what a probe reads — and no non-Read action
/// anywhere, which is what makes handing it to probe text safe.
#[test]
fn the_probes_reader_role_can_read_everything_and_write_nothing() {
    use boss_policy_client::{Action, Resource, Scope};

    let role = reader_role();
    let rules = boss_policy_client::defaults::default_rules();
    let mine: Vec<_> = rules.iter().filter(|r| r.role == role).collect();
    assert!(
        !mine.is_empty(),
        "run-car-probe.sh gives the probe role '{role}', which core policy does not seed at \
         all — an unseeded role reads NOTHING, which is the defect with extra steps"
    );
    for resource in [Resource::job(), Resource::step(), Resource::event()] {
        assert!(
            mine.iter().any(|r| r.resource == resource
                && r.action == Action::Read
                && r.scope == Scope::All),
            "'{role}' has no Read/All on {resource:?} — a probe carrying it would see a \
             narrower world than the operator, which is what 61085a9e measured"
        );
    }
    for rule in &mine {
        assert_eq!(
            rule.action,
            Action::Read,
            "'{role}' carries a non-Read grant ({:?} on {:?}) — it is handed to program text \
             a builder wrote, so it must not be able to change anything",
            rule.action,
            rule.resource
        );
    }
}

/// The reader the gate advises must EXIST, and be executable in the
/// checkout the forge runs probes from. A refusal that names a tool
/// nobody shipped is worse than no refusal.
#[test]
fn the_sanctioned_reader_ships_in_the_checkout_and_is_executable() {
    let p = reader_path();
    assert!(
        p.is_file(),
        "the probe's PATH points at {}, which does not exist",
        p.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&p)
            .expect("reader metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "{} is not executable (mode {mode:o}) — the probe would get `command not found`",
            p.display()
        );
    }
}

/// Run the reader with a STUB `curl` first on PATH that records its
/// argv. Returns (exit code, stdout, stderr, argv one per line).
fn run_reader(case: &str, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String, String) {
    let dir = scratch(case);
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    let log = dir.join("curl-argv");
    boss_testing::write_file(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\n\
             printf 'STUB_BODY\\n'\n",
            log.display()
        ),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("curl"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub curl");
    }
    let mut cmd = Command::new(reader_path());
    cmd.args(args)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .current_dir(&dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("the reader runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        std::fs::read_to_string(&log).unwrap_or_default(),
    )
}

/// THE READER IDENTIFIES THE PROBE. The header comes from the runner's
/// env, never from the probe's text, and the method is GET because
/// there is no method argument to give.
#[test]
fn the_reader_sends_the_runners_read_scoped_header_on_a_get() {
    let actor = format!("{{\"id\":\"automation:x\",\"role\":\"{}\"}}", reader_role());
    let (rc, stdout, stderr, argv) = run_reader(
        "reader-get",
        &["/api/yard/status"],
        &[
            ("BOSS_JOBS_URL", "http://sor.invalid:7900"),
            ("BOSS_SOR_USER", &actor),
        ],
    );
    assert_eq!(rc, 0, "stderr: {stderr}");
    assert!(stdout.contains("STUB_BODY"), "stdout: {stdout}");
    assert!(
        argv.contains(&format!("x-boss-user: {actor}")),
        "the reader did not send the runner's actor; argv:\n{argv}"
    );
    assert!(
        argv.contains("http://sor.invalid:7900/api/yard/status"),
        "the reader must read the base the runner pinned; argv:\n{argv}"
    );
    assert!(
        !argv.lines().any(|l| l == "-X"),
        "the reader must be GET-only — no method is selectable; argv:\n{argv}"
    );
}

/// AND IT REFUSES RATHER THAN READING UNIDENTIFIED. With no actor in
/// the env the reader must stop — never fall back to a bare read, which
/// is the false-absence shape this whole block is about. `curl` must not
/// even be reached.
#[test]
fn the_reader_refuses_rather_than_reading_unidentified() {
    let (rc, stdout, stderr, argv) = run_reader(
        "reader-no-actor",
        &["/api/yard/status"],
        &[("BOSS_JOBS_URL", "http://sor.invalid:7900")],
    );
    assert_ne!(
        rc, 0,
        "a reader with no identity must refuse; stdout: {stdout}"
    );
    assert!(
        stderr.contains("BOSS_SOR_USER"),
        "the refusal must name what is missing; stderr: {stderr}"
    );
    assert!(
        argv.is_empty(),
        "curl was invoked anyway — the unidentified read happened; argv:\n{argv}"
    );
}

/// A METHOD IS NOT SELECTABLE, AND NEITHER IS THE HOST. Both are the
/// "a wrong target answers instead of erroring" lesson: a probe that
/// could name its own URL could read the OTHER, older stack at boss-gcp
/// and get a well-formed answer about different data.
#[test]
fn the_reader_takes_a_path_and_refuses_anything_else() {
    let actor = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";
    for (case, args) in [
        (
            "reader-absolute-url",
            vec!["http://127.0.0.1:7900/api/jobs"],
        ),
        ("reader-method", vec!["PUT", "/api/jobs"]),
        ("reader-no-args", vec![]),
    ] {
        let (rc, _, stderr, argv) = run_reader(
            case,
            &args,
            &[
                ("BOSS_JOBS_URL", "http://sor.invalid:7900"),
                ("BOSS_SOR_USER", actor),
            ],
        );
        assert_ne!(rc, 0, "{case} must be refused");
        assert!(argv.is_empty(), "{case} reached curl anyway; argv:\n{argv}");
        assert!(!stderr.is_empty(), "{case} must say why");
    }
}

// =====================================================================
// THE READER ROUTES BY PATH (design 28d2bed9, David 2026-09-17). The
// machine door carries every read surface of the instance on one IP,
// one port per service; the reader picks the port from a `name=port`
// table the runner hands it as BOSS_SOR_PORTS, by the path's prefix,
// and keeps the HOST from BOSS_JOBS_URL. Without a table it is exactly
// the reader it was: one base, one port, the jobs API — so nothing
// changes until the runner carries the table. The equality pin between
// the table, the manifest and boss-ports is in
// the_machine_door_carries_every_read_surface.rs.
// =====================================================================

const PORTS_TABLE: &str = "jobs=7900 events=7150 people=7500 classes=7800 locations=7820 \
                           accounts=7550 ledger=7080 dispatcher=7950";

fn reader_env<'a>(actor: &'a str, table: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    let mut env = vec![
        ("BOSS_JOBS_URL", "http://sor.invalid:7900"),
        ("BOSS_SOR_USER", actor),
    ];
    if let Some(t) = table {
        env.push(("BOSS_SOR_PORTS", t));
    }
    env
}

/// The URL the stub curl was handed — the last argv line.
fn url_read(argv: &str) -> String {
    argv.lines().last().unwrap_or_default().to_string()
}

/// A path under a routed prefix goes to that service's port, on the
/// host BOSS_JOBS_URL names — the measured case first: the audit tail,
/// which 404'd on the jobs port for car 056f7bd8.
#[test]
fn the_reader_routes_a_prefixed_path_to_its_services_port() {
    let actor = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";
    for (case, path, want) in [
        (
            "route-events",
            "/api/events/tail?kind=declared&limit=500",
            "http://sor.invalid:7150/api/events/tail?kind=declared&limit=500",
        ),
        (
            "route-people",
            "/api/people/emp-david",
            "http://sor.invalid:7500/api/people/emp-david",
        ),
        (
            "route-people-bare",
            "/api/people?limit=1",
            "http://sor.invalid:7500/api/people?limit=1",
        ),
        (
            "route-classes",
            "/api/classes?subject_kind=employee",
            "http://sor.invalid:7800/api/classes?subject_kind=employee",
        ),
        (
            "route-locations",
            "/api/locations/loc-hq",
            "http://sor.invalid:7820/api/locations/loc-hq",
        ),
        // boss-accounts mounts under /api/people/ but is its own
        // service on 7550 (backlog de0989d2): the longer prefix wins
        // over the people route.
        (
            "route-accounts",
            "/api/people/accounts?limit=1",
            "http://sor.invalid:7550/api/people/accounts?limit=1",
        ),
        (
            "route-accounts-my-day",
            "/api/people/my-day/actions",
            "http://sor.invalid:7550/api/people/my-day/actions",
        ),
        // The ledger (7080) and the dispatcher's rule registry (7950)
        // joined the door on 2026-09-17 (backlog 77fd7b5a + 4145d2c1).
        (
            "route-ledger",
            "/api/ledger/trial-balance",
            "http://sor.invalid:7080/api/ledger/trial-balance",
        ),
        (
            "route-dispatcher",
            "/api/dispatcher/rules",
            "http://sor.invalid:7950/api/dispatcher/rules",
        ),
    ] {
        let (rc, _, stderr, argv) =
            run_reader(case, &[path], &reader_env(actor, Some(PORTS_TABLE)));
        assert_eq!(rc, 0, "{case}: stderr: {stderr}");
        assert_eq!(url_read(&argv), want, "{case}: argv:\n{argv}");
        assert!(
            argv.contains(&format!("x-boss-user: {actor}")),
            "{case}: the routed read lost the runner's actor; argv:\n{argv}"
        );
    }
}

/// Everything else is the jobs API, on the base exactly as the runner
/// pinned it — including a prefix that merely RESEMBLES a routed one.
#[test]
fn the_reader_defaults_to_the_jobs_api() {
    let actor = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";
    for (case, path) in [
        ("default-yard", "/api/yard/status"),
        ("default-jobs", "/api/jobs?kind=pr-train&status=open"),
        ("default-agents", "/api/agents"),
        ("default-lookalike", "/api/peoples/x"),
        ("default-eventsish", "/api/eventsource"),
    ] {
        let (rc, _, stderr, argv) =
            run_reader(case, &[path], &reader_env(actor, Some(PORTS_TABLE)));
        assert_eq!(rc, 0, "{case}: stderr: {stderr}");
        assert_eq!(
            url_read(&argv),
            format!("http://sor.invalid:7900{path}"),
            "{case}: argv:\n{argv}"
        );
    }
}

/// NO TABLE, NO CHANGE. A runner that does not carry BOSS_SOR_PORTS
/// (a checkout older than this car, a hand run) gets the reader it
/// always had: every path on the base, the jobs port. The routed read
/// then 404s exactly as it did, which is a loud failure and not a
/// guessed port.
#[test]
fn the_reader_without_a_table_reads_every_path_on_the_base() {
    let actor = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";
    for (case, path) in [
        ("no-table-events", "/api/events/tail?limit=1"),
        ("no-table-jobs", "/api/jobs"),
    ] {
        let (rc, _, stderr, argv) = run_reader(case, &[path], &reader_env(actor, None));
        assert_eq!(rc, 0, "{case}: stderr: {stderr}");
        assert_eq!(
            url_read(&argv),
            format!("http://sor.invalid:7900{path}"),
            "{case}: argv:\n{argv}"
        );
    }
}

/// A table that EXISTS but lacks the service a path routes to is a
/// defect in the table, not a reason to guess: the reader refuses and
/// names the missing entry, before curl is reached.
#[test]
fn the_reader_refuses_a_table_that_lacks_the_routed_service() {
    let actor = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";
    let (rc, _, stderr, argv) = run_reader(
        "table-missing-events",
        &["/api/events/tail"],
        &reader_env(actor, Some("jobs=7900 people=7500")),
    );
    assert_eq!(rc, 2, "stderr: {stderr}");
    assert!(
        argv.is_empty(),
        "curl was reached with a guessed port; argv:\n{argv}"
    );
    assert!(
        stderr.contains("events") && stderr.contains("BOSS_SOR_PORTS"),
        "the refusal must name the missing service and the table; stderr: {stderr}"
    );
}

/// The refusals are untouched by routing: still one argument, still a
/// path, still identified — with the table present.
#[test]
fn routing_leaves_every_refusal_in_place() {
    let actor = "{\"id\":\"automation:x\",\"role\":\"audit-readonly\"}";
    for (case, args, env) in [
        (
            "routed-url",
            vec!["http://sor.invalid:7150/api/events/tail"],
            reader_env(actor, Some(PORTS_TABLE)),
        ),
        (
            "routed-two-args",
            vec!["/api/events/tail", "/api/jobs"],
            reader_env(actor, Some(PORTS_TABLE)),
        ),
        (
            "routed-unidentified",
            vec!["/api/events/tail"],
            vec![
                ("BOSS_JOBS_URL", "http://sor.invalid:7900"),
                ("BOSS_SOR_PORTS", PORTS_TABLE),
            ],
        ),
    ] {
        let (rc, _, stderr, argv) = run_reader(case, &args, &env);
        assert_eq!(rc, 2, "{case} must be refused; stderr: {stderr}");
        assert!(argv.is_empty(), "{case} reached curl anyway; argv:\n{argv}");
    }
}

/// THE TABLE REACHES THE PROBE. run-car-probe.sh reads
/// infra/forge/sor-ports.env from the checkout the probe runs in — the
/// same checkout it takes the reader from — and exports it on BOTH
/// hand-offs as BOSS_SOR_PORTS. A checkout without the file exports an
/// empty table, which the reader treats as absent.
#[test]
fn the_runner_hands_the_probe_the_port_table_from_its_own_checkout() {
    let sh = script();
    assert!(
        sh.contains("$PROBE_DIR/infra/forge/sor-ports.env"),
        "run-car-probe.sh does not read the port table from the probe's checkout"
    );
    let handoffs: Vec<&str> = sh
        .split("bash -c \"$probe_prelude$probe\"")
        .take(2)
        .collect();
    assert_eq!(handoffs.len(), 2);
    for (which, segment) in ["root/runuser", "by-hand"].iter().zip(handoffs) {
        let tail = &segment[segment.len().saturating_sub(600)..];
        assert!(
            tail.contains("BOSS_SOR_PORTS=\"$SOR_PORTS\""),
            "the {which} hand-off does not export the port table to the probe:\n{tail}"
        );
    }
}

/// And the extraction itself, run: the file's `name=port` lines become
/// one space-separated table; comments and blank lines are not in it.
#[test]
fn the_runners_table_extraction_reads_the_file_as_data() {
    let sh = script();
    let block = sh
        .split_once("# SOR-PORTS-BEGIN")
        .expect("run-car-probe.sh has a # SOR-PORTS-BEGIN marker")
        .1
        .split_once("# SOR-PORTS-END")
        .expect("run-car-probe.sh has a # SOR-PORTS-END marker")
        .0;
    let dir = scratch("runner-ports-table");
    let forge = dir.join("infra/forge");
    boss_testing::create_dir(&forge);
    boss_testing::write_file(
        &forge.join("sor-ports.env"),
        "# a comment\n\njobs=7900\nevents=7150\n  people=7500  \n",
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "PROBE_DIR='{}'\n{block}\nprintf '%s' \"$SOR_PORTS\"",
            dir.display()
        ))
        .output()
        .expect("bash runs");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "jobs=7900 events=7150 people=7500",
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // No file: an empty table, not an error.
    let empty = scratch("runner-ports-table-absent");
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -u\nPROBE_DIR='{}'\n{block}\nprintf '[%s]' \"$SOR_PORTS\"",
            empty.display()
        ))
        .output()
        .expect("bash runs");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[]");
}
