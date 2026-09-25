//! The free-space floor the gate refuses below and the /work floor the
//! dev pod's reclaim acts below are ONE definition, `infra/build-floor.env`,
//! and the reclaim's floor sits above the gate's by a margin.
//!
//! WHY (backlog 99ce8744, 2026-09-24). Measured ~23:00Z: /work had
//! 12 GB free of 40 GB. `infra/gate.sh` refused below 12
//! (`BOSS_GATE_MIN_FREE_GB:-12`) and `infra/cluster/dev-scratch-reclaim.sh`
//! acted only below 6 (`BOSS_WORK_FLOOR_GB:-6`), so the automatic pass
//! could never fire before a pre-flight was refused, and two builders
//! in one hour lowered the gate's floor by hand. The same fact lived
//! twice and had drifted into the one order that disables the reclaim
//! (CLAUDE.md §9a). Collapsed: both scripts read the file, and the
//! reclaim's floor is DERIVED from the gate's, so it cannot be set
//! below it. The reclaim's half of the pin — that its floor is the
//! gate's plus the margin, and that the retired knob the live sidecar
//! still sets moves nothing — is in `dev_scratch_reclaim_sh.rs`, beside
//! the fixtures that drive that script.

use boss_testing::repo_root;
use std::process::Command;

/// The value of `name` in `infra/build-floor.env`, read the way the two
/// scripts read it: the last `NAME=<number>` line.
fn build_floor(name: &str) -> u64 {
    let text = std::fs::read_to_string(repo_root().join("infra/build-floor.env"))
        .expect("infra/build-floor.env is the one definition of the build floor");
    text.lines()
        .rev()
        .find_map(|l| l.strip_prefix(&format!("{name}=")))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| panic!("infra/build-floor.env defines {name} as a whole number"))
}

#[test]
fn the_reclaim_trigger_sits_above_the_gates_floor() {
    assert!(
        build_floor("GATE_MIN_FREE_GB") > 0,
        "a zero gate floor refuses nothing"
    );
    assert!(
        build_floor("WORK_RECLAIM_MARGIN_GB") > 0,
        "with no margin the reclaim fires at the same reading the gate \
         refuses at — the automatic pass must act BEFORE any pre-flight \
         is refused, which is the whole packet"
    );
}

/// Driven through the real script: with no `BOSS_GATE_MIN_FREE_GB` in
/// the environment, the gate refuses one GB under the file's floor and
/// names that floor. A builder's shell that lowered the knob by hand is
/// exactly what this must not inherit, so it is removed explicitly.
#[test]
fn the_gate_refuses_below_the_floor_the_file_defines() {
    let floor = build_floor("GATE_MIN_FREE_GB");
    let dir = boss_testing::scratch_dir("boss-build-floor-gate");
    let fake = dir.join("df");
    let kb = (floor - 1) * 1024 * 1024;
    boss_testing::write_exec(
        &fake,
        &format!(
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             echo '/dev/fake 1 1 {kb} 99% /'\n"
        ),
    );
    // Refused at `require_headroom "to start"`, before any git call —
    // the ordering gate_sh.rs `the_gate_refuses_to_run_without_headroom`
    // relies on for the same invocation.
    let out = Command::new("bash")
        .arg(repo_root().join("infra/gate.sh"))
        .arg("--auto")
        .env_remove("BOSS_GATE_MIN_FREE_GB")
        .env("BOSS_GATE_DF_CMD", fake.to_str().expect("utf8"))
        .current_dir(repo_root())
        .output()
        .expect("run gate.sh");
    let _ = std::fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a floor refusal is exit 2, not a failed check\nstderr: {stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "{}GB free, need {floor}GB. Refusing to start",
            floor - 1
        )),
        "the gate must apply the floor infra/build-floor.env defines ({floor}GB)\nstderr: {stderr}"
    );
}

/// No script spells a default for either floor of its own. A second
/// literal is how the two drifted apart: `${BOSS_GATE_MIN_FREE_GB:-12}`
/// and `${BOSS_WORK_FLOOR_GB:-6}` were each correct when written.
#[test]
fn neither_script_carries_a_floor_literal_of_its_own() {
    for path in ["infra/gate.sh", "infra/cluster/dev-scratch-reclaim.sh"] {
        let text = std::fs::read_to_string(repo_root().join(path)).expect(path);
        for knob in ["BOSS_GATE_MIN_FREE_GB:-", "BOSS_WORK_FLOOR_GB:-"] {
            let literal = text.match_indices(knob).any(|(at, _)| {
                text[at + knob.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit())
            });
            assert!(
                !literal,
                "{path} spells its own default for {knob}<number>; read the floor \
                 from infra/build-floor.env"
            );
        }
        assert!(
            text.contains("build-floor.env"),
            "{path} must read the floor from infra/build-floor.env"
        );
    }
}
