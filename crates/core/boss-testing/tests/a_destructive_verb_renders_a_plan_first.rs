//! `commission-a-disk --plan` renders what would happen and writes
//! nothing — the "plan" of plan-then-approve.
//!
//! WHY (design 17835005, answered by David 2026-09-21). The question was
//! whether a passkey could approve a destructive change that the machine
//! then executes. Q1 settled what the signature binds: **a rendered plan
//! hash, not a verb call.** A verb call authorises an intent whose target
//! can still resolve differently at execution time — which is precisely
//! how 2026-09-21 would have gone wrong. A second NVMe went into the
//! forge and the kernel RENUMBERED the devices: the new blank drive came
//! up as `nvme0n1` and the live root filesystem moved to `nvme1n1p2`.
//! Approving "format nvme0n1" would have approved destroying the system
//! disk. Approving a plan that already names the resolved target by-id,
//! with its observed facts, cannot be replayed onto a different disk.
//!
//! THE PLAN IS HASHED OVER ITS OWN BYTES, which is why these tests care
//! so much about determinism. There is no canonicalisation step to drift
//! (§9a): whoever verifies re-hashes the bytes it was handed. So the
//! document must carry nothing that varies between two renders of the
//! same true state — no timestamp, no run id — or an approval that is
//! still valid breaks on its own. And conversely, if any OBSERVED fact
//! moves, the bytes move with it, which is exactly the drift q4 says
//! must void an approval.
//!
//! WHAT IS AND IS NOT TESTED HERE, stated rather than left to be
//! discovered. The precondition EVALUATION needs a real block device
//! under `/dev/disk/by-id/`, and the gate runs as uid 65534 on a pod
//! that has no such directory — so it is not exercised here, and these
//! tests say so instead of pretending. What IS exercised: the two
//! refusals that precede any device access, the rendering itself
//! (against the template file the script runs, not a copy of it), and
//! the structural property that matters most — that `--plan` cannot
//! reach a mutating command.

use std::path::PathBuf;
use std::process::Command;

use boss_testing::repo_root;

fn script() -> PathBuf {
    repo_root().join("infra/forge/commission-a-disk.sh")
}

fn template() -> PathBuf {
    repo_root().join("infra/forge/commission-a-disk.plan.jq")
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn have(bin: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `bash commission-a-disk.sh --plan <args>`, as whatever uid runs the
/// test. Returns (exit code, stdout, stderr).
fn plan(args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new("bash");
    cmd.arg(script()).arg("--plan");
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output().expect("the script runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// THE STRUCTURAL PROPERTY, and the one that would matter most if it
/// broke: `--plan` must exit before anything that writes.
///
/// Read BY LINE and only REAL invocations — this file is more comment
/// than code, and the header talks about `parted` and `mkfs` at length
/// while describing what the write path does. A needle that can match a
/// comment tests the comment (the lesson `quick_mode_exits_before_the_
/// first_compile` learned the same way, twice).
#[test]
fn the_plan_branch_exits_before_the_first_mutating_command() {
    let body = read("infra/forge/commission-a-disk.sh");
    let lines: Vec<&str> = body.lines().collect();

    let code = |l: &str| {
        let t = l.trim_start();
        !t.is_empty() && !t.starts_with('#')
    };

    let plan_exit = lines
        .iter()
        .enumerate()
        .find(|(_, l)| code(l) && l.trim_start() == "exit 0")
        .map(|(i, _)| i)
        .expect("the --plan branch no longer exits");

    // The commands that change the disk, named rather than matched on a
    // pattern: a pattern wide enough to catch them catches prose too.
    let mutators = ["parted ", "mkfs.ext4 ", "mount -a", ">> /etc/fstab"];
    let first_mutation = lines
        .iter()
        .enumerate()
        .find(|(_, l)| code(l) && mutators.iter().any(|m| l.contains(m)))
        .map(|(i, l)| (i, l.trim().to_string()))
        .expect("the script no longer mutates anything — this pin reads the wrong file");

    assert!(
        plan_exit < first_mutation.0,
        "`--plan` exits at line {} but the first mutating command is at line {} ({}) — \
         the plan branch must come FIRST, or a plan run partitions a disk, which is the \
         one thing it promises not to do",
        plan_exit + 1,
        first_mutation.0 + 1,
        first_mutation.1
    );
}

/// EVERY PRECONDITION THE WRITE PATH CHECKS IS CHECKED BEFORE THE PLAN
/// IS RENDERED, so a plan is only ever produced for something that
/// would actually run. A plan for a target that cannot be commissioned
/// is not a plan; it is a refusal wearing a plan's clothes, and it is
/// what an approver would be signing.
#[test]
fn the_plan_is_rendered_only_after_every_precondition_holds() {
    let body = read("infra/forge/commission-a-disk.sh");
    let lines: Vec<&str> = body.lines().collect();
    let code = |l: &str| {
        let t = l.trim_start();
        !t.is_empty() && !t.starts_with('#')
    };
    let plan_at = lines
        .iter()
        .position(|l| code(l) && l.contains("if [ \"$PLAN\" -eq 1 ]"))
        .expect("the --plan branch is gone");

    // The three preconditions, by the refusal each one raises.
    let needles = [
        "is not a stable identity",  // 1: by-id, never a kernel name
        "a disk with partitions",    // 2: no partition table
        "backs the root filesystem", // 3: not root, nothing mounted
        "mounted filesystem(s)",
    ];
    for n in needles {
        let at = lines
            .iter()
            .position(|l| code(l) && l.contains(n))
            .unwrap_or_else(|| panic!("no precondition raising {n:?} — the script changed shape"));
        assert!(
            at < plan_at,
            "the precondition raising {n:?} is at line {} but the plan renders at line {} — \
             a plan must not be produced for a target the write path would refuse",
            at + 1,
            plan_at + 1
        );
    }
}

/// THE REFUSALS THAT NEED NO DEVICE, run for real.
///
/// A kernel name is the exact mistake the renumbering set up, and it is
/// refused before anything is resolved — so this runs on any box, as any
/// uid, including the gate's 65534.
#[test]
fn a_kernel_name_is_refused_and_nothing_is_rendered() {
    let (code, out, err) = plan(&["/dev/nvme0n1", "/mnt/boss-data"]);
    assert_eq!(
        code, 78,
        "a wrong request is exit 78, not a failed run: {err}"
    );
    assert!(
        err.contains("stable identity"),
        "the refusal must say why a kernel name is not a target: {err}"
    );
    assert!(
        out.trim().is_empty(),
        "a refused plan must render NOTHING — an approver must never be handed a document \
         for a target that was rejected: {out}"
    );
}

/// A by-id path that names nothing is refused too, and again renders
/// nothing. Same uid-independent reason: it fails at `[ -e ]`.
#[test]
fn a_by_id_path_that_names_nothing_is_refused() {
    let (code, out, err) = plan(&[
        "/dev/disk/by-id/nvme-THIS-DISK-DOES-NOT-EXIST-0000",
        "/mnt/boss-data",
    ]);
    assert_eq!(code, 78, "{err}");
    assert!(out.trim().is_empty(), "nothing is rendered: {out}");
    assert!(
        err.contains("no such device") || err.contains("cannot resolve"),
        "the refusal names the missing device: {err}"
    );
}

/// THE RENDERER ITSELF, run against the TEMPLATE THE SCRIPT RUNS.
///
/// The template lives in its own file precisely so this is not a copy:
/// `commission-a-disk.sh` invokes it with `jq -f`, and so does this.
/// That is what lets the rendering be tested on a box with no block
/// devices at all.
#[test]
fn the_plan_document_renders_the_resolved_target_and_the_observed_facts() {
    if !have("jq") {
        eprintln!("a_destructive_verb_renders_a_plan_first: SKIPPED — no jq");
        return;
    }
    let out = Command::new("jq")
        .args([
            "-n",
            "--arg",
            "by_id",
            "/dev/disk/by-id/nvme-SAMSUNG_X_1TB_S1234",
            "--arg",
            "dev",
            "/dev/nvme0n1",
            "--arg",
            "mount",
            "/mnt/boss-data",
            "--arg",
            "size",
            "1024209543168",
            "--arg",
            "parts",
            "0",
            "--arg",
            "mounted",
            "0",
            "-f",
        ])
        .arg(template())
        .output()
        .expect("jq runs");
    assert!(
        out.status.success(),
        "the plan template is not valid jq: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let plan: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("the plan is not JSON: {e}\n{text}"));

    // THE RESOLVED TARGET, both spellings. The by-id name is what was
    // asked for and what cannot move; the kernel name is what it
    // resolved to at plan time and is exactly the thing that renumbers.
    // An approver needs both: one to know which disk, one to see what it
    // was called when the plan was made.
    assert_eq!(
        plan["target_by_id"],
        "/dev/disk/by-id/nvme-SAMSUNG_X_1TB_S1234"
    );
    assert_eq!(plan["resolves_to"], "/dev/nvme0n1");
    assert_eq!(plan["mount_path"], "/mnt/boss-data");
    assert_eq!(plan["size_bytes"], 1024209543168_u64);

    // THE OBSERVED FACTS ARE NUMBERS, not strings: they are compared
    // against a re-observation at apply time, and "0" != 0 would make a
    // drift check that never matches.
    assert_eq!(plan["observed"]["partition_count"], 0);
    assert_eq!(plan["observed"]["mounted_filesystems"], 0);
    assert_eq!(plan["observed"]["backs_root"], false);
    assert!(
        plan["observed"]["partition_count"].is_number(),
        "observed facts must be numbers, or a drift comparison silently never fires"
    );

    // The argv names the by-id target, never the kernel name — the
    // whole point of the verb.
    let argv: Vec<&str> = plan["argv"]
        .as_array()
        .expect("argv is a list")
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect();
    assert!(
        argv.contains(&"/dev/disk/by-id/nvme-SAMSUNG_X_1TB_S1234"),
        "argv must carry the by-id target: {argv:?}"
    );
    assert!(
        !argv.contains(&"/dev/nvme0n1"),
        "argv must NOT carry the kernel name — it moves, and that is the defect this \
         verb exists to make unrepresentable: {argv:?}"
    );
}

/// THE DETERMINISM THE HASH DEPENDS ON: two renders of the same state
/// are byte-identical, and a changed observation changes the bytes.
///
/// Both halves matter. Without the first, an approval breaks on its own
/// between signing and applying. Without the second, a plan could drift
/// under an approval that still verified — which is the failure q4's
/// "voided by drift" exists to prevent.
///
/// WHAT THIS TEST CANNOT CATCH, measured rather than assumed: a COARSE
/// clock. Adding `rendered_at: (now | todate)` to the template was tried
/// here, and this test still PASSED — two renders inside the same second
/// produce the same second-resolution string. It is
/// `the_plan_template_carries_no_clock_and_no_run_identity` that caught
/// it, by reading the template rather than running it. So the pair is
/// load-bearing and neither half is redundant: one proves the render is
/// stable, the other forbids the ingredients whose instability this one
/// would miss.
#[test]
fn the_same_state_renders_the_same_bytes_and_a_moved_fact_does_not() {
    if !have("jq") {
        eprintln!("a_destructive_verb_renders_a_plan_first: SKIPPED — no jq");
        return;
    }
    let render = |parts: &str| -> String {
        let out = Command::new("jq")
            .args([
                "-n",
                "--arg",
                "by_id",
                "/dev/disk/by-id/nvme-X",
                "--arg",
                "dev",
                "/dev/nvme0n1",
                "--arg",
                "mount",
                "/mnt/boss-data",
                "--arg",
                "size",
                "1024209543168",
                "--arg",
                "parts",
                parts,
                "--arg",
                "mounted",
                "0",
                "-f",
            ])
            .arg(template())
            .output()
            .expect("jq runs");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    assert_eq!(
        render("0"),
        render("0"),
        "two renders of the same state must be byte-identical — the plan is hashed over \
         its own bytes, so anything that varies on its own breaks a valid approval"
    );
    assert_ne!(
        render("0"),
        render("1"),
        "a disk that has GAINED a partition since the plan was made must render different \
         bytes, or an approval survives the drift it exists to be voided by"
    );
}

/// NOTHING IN THE TEMPLATE MAY VARY ON ITS OWN. A timestamp is the
/// obvious way this gets broken later — it reads like provenance and it
/// silently makes every approval single-render.
#[test]
fn the_plan_template_carries_no_clock_and_no_run_identity() {
    let t = read("infra/forge/commission-a-disk.plan.jq");
    let code: String = t
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for banned in ["now", "localtime", "todate", "strftime", "$ENV", "env."] {
        assert!(
            !code.contains(banned),
            "the plan template uses {banned:?} — the plan is signed over its own bytes, so \
             anything that varies between two renders of the same state breaks an approval \
             that is still valid. The render time belongs on the packet, outside what is signed."
        );
    }
}

/// THE PLAN VERB IS READ-ONLY, and the write verb stays inert. The two
/// declarations are one word apart from each other's meaning, so they
/// are pinned rather than trusted to review.
#[test]
fn the_plan_verb_needs_no_approval_and_the_write_verb_still_does() {
    let plan_spec: serde_json::Value =
        serde_json::from_str(&read("infra/ops/verbs/plan-a-disk-commission.json"))
            .expect("the plan verb file is JSON");
    let write_spec: serde_json::Value =
        serde_json::from_str(&read("infra/ops/verbs/commission-a-disk.json"))
            .expect("the write verb file is JSON");

    assert!(
        plan_spec.get("requires_approval").is_none()
            || plan_spec["requires_approval"] == serde_json::Value::Bool(false),
        "the plan verb mutates nothing, so it must NOT require an approval — it is the half \
         that is usable before the approval channel exists"
    );
    assert_eq!(
        write_spec["requires_approval"], true,
        "the write verb stays inert until an approval can be verified"
    );

    // And the plan verb must actually pass --plan, or it IS the write
    // verb under another name.
    let argv: Vec<&str> = plan_spec["argv"]
        .as_array()
        .expect("argv")
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect();
    assert!(
        argv.contains(&"--plan"),
        "the plan verb must invoke the script with --plan, or it partitions the disk: {argv:?}"
    );
    assert_eq!(
        argv.first().copied(),
        Some("infra/forge/commission-a-disk.sh"),
        "both verbs run the same script, so the preconditions cannot differ between plan \
         and apply: {argv:?}"
    );
}
