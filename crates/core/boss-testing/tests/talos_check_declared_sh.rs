//! `infra/cluster/talos/check-declared.sh` is RUN, not read — against
//! synthetic `talosctl get machineconfig -o yaml` documents built from
//! the values David read off the LIVE nodes on 2026-09-15 (backlog
//! 08430090), so every verdict below is one the comparator actually
//! reached. Nothing here touches a cluster or needs a credential.
//!
//! WHY THIS FILE EXISTS. The gate depends on Talos machine-config entries
//! (w-1's `machine.files` + `machine.kubelet.extraMounts` for the gate
//! seed, the kubelet image-GC thresholds, the registry mirror the whole
//! cluster pulls through) that until this car lived only on a laptop.
//! `infra/cluster/talos/patches/<node>.yaml` is now the declaration and
//! the comparator reads the live config against it in the vocabulary
//! design 16115a17 decided: MATCH / DRIFT (both values printed) /
//! UNDECLARED, plus ABSENT and DOUBLED — the second is the w-1 case as
//! found: the gate-seed `files` entry applied by hand twice on
//! 2026-09-12, identical, so the live config carries it two times.
//!
//! The live document is doubled at a second level too: `talosctl get
//! machineconfig` prints the config resource more than once (two copies
//! in David's output, ~115 lines apart on w-1), so a comparator that
//! counted entries across documents would read every node as DOUBLED.
//! The fixture carries that shape on purpose.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(case: &str) -> PathBuf {
    boss_testing::scratch_dir(&format!("talos-check-declared-{case}"))
}

fn script() -> PathBuf {
    repo_root().join("infra/cluster/talos/check-declared.sh")
}

fn patches_dir() -> PathBuf {
    repo_root().join("infra/cluster/talos/patches")
}

/// What a node's live config carries, in the classes the comparator
/// reads. Defaults are "nothing", so each case says exactly what its
/// live node has.
#[derive(Clone, Default)]
struct Live {
    hostname: Option<&'static str>,
    /// How many copies of the gate-seed `machine.files` entry (w-1 live
    /// on 2026-09-15: 2).
    seed_file_copies: usize,
    seed_mount: bool,
    /// `(imageGCHighThresholdPercent, imageGCLowThresholdPercent)`.
    image_gc: Option<(u32, u32)>,
    mirror: bool,
    /// An extra mount the declaration does not name — the shape of the
    /// `- rw` David's grep showed on every control plane before its
    /// `extraConfig:`.
    other_mount: bool,
}

/// The `machine:` section of one config, indented as `talosctl` prints
/// it (4 spaces, the whole config nested under `spec:`).
fn machine_section(live: &Live) -> String {
    let mut s = String::new();
    s.push_str("    machine:\n");
    s.push_str("        type: worker\n");
    s.push_str("        token: REDACTED\n");
    s.push_str("        kubelet:\n");
    s.push_str("            image: ghcr.io/siderolabs/kubelet:v1.33.0\n");
    if live.seed_mount || live.other_mount {
        s.push_str("            extraMounts:\n");
        if live.other_mount {
            s.push_str(
                "                - destination: /var/lib/longhorn\n\
                 \x20                 type: bind\n\
                 \x20                 source: /var/lib/longhorn\n\
                 \x20                 options:\n\
                 \x20                   - bind\n\
                 \x20                   - rshared\n\
                 \x20                   - rw\n",
            );
        }
        if live.seed_mount {
            s.push_str(
                "                - destination: /var/local/gate-seed\n\
                 \x20                 type: bind\n\
                 \x20                 source: /var/local/gate-seed\n\
                 \x20                 options:\n\
                 \x20                   - bind\n\
                 \x20                   - rshared\n\
                 \x20                   - rw\n",
            );
        }
    }
    if let Some((high, low)) = live.image_gc {
        s.push_str("            extraConfig:\n");
        s.push_str(&format!(
            "                imageGCHighThresholdPercent: {high}\n"
        ));
        s.push_str(&format!(
            "                imageGCLowThresholdPercent: {low}\n"
        ));
    }
    s.push_str("            defaultRuntimeSeccompProfileEnabled: true\n");
    s.push_str("            disableManifestsDirectory: true\n");
    s.push_str("        network:\n");
    if let Some(h) = live.hostname {
        s.push_str(&format!("            hostname: {h}\n"));
    }
    s.push_str("            interfaces:\n");
    s.push_str("                - interface: eth0\n");
    s.push_str("                  dhcp: true\n");
    s.push_str("        install:\n");
    s.push_str("            disk: /dev/nvme0n1\n");
    s.push_str("            wipe: false\n");
    if live.seed_file_copies > 0 {
        s.push_str("        files:\n");
        for _ in 0..live.seed_file_copies {
            s.push_str(
                "            - content: |\n\
                 \x20               local PV gate-seed-w-1 (infra/cluster/manifests/gate-seed-local.yaml) mounts this directory\n\
                 \x20             permissions: 0o644\n\
                 \x20             path: /var/local/gate-seed/.declared-by-talos\n\
                 \x20             op: create\n",
            );
        }
    }
    if live.mirror {
        s.push_str(
            "        registries:\n\
             \x20           mirrors:\n\
             \x20               10.20.0.15:3000:\n\
             \x20                   endpoints:\n\
             \x20                       - http://10.20.0.15:3000\n",
        );
    }
    s.push_str("        features:\n");
    s.push_str("            rbac: true\n");
    s.push_str("            stableHostname: true\n");
    s
}

/// One `MachineConfigs.config.talos.dev` resource as `talosctl get
/// machineconfig -o yaml` prints it, with the `cluster:` tail a real
/// document carries after `machine:`.
fn resource(node_ip: &str, id: &str, live: &Live) -> String {
    format!(
        "node: {node_ip}\n\
         metadata:\n\
         \x20   namespace: config\n\
         \x20   type: MachineConfigs.config.talos.dev\n\
         \x20   id: {id}\n\
         \x20   version: 3\n\
         \x20   owner: config.MachineConfigController\n\
         \x20   phase: running\n\
         spec:\n\
         \x20   version: v1alpha1 # Indicates the schema used to decode the contents.\n\
         \x20   debug: false\n\
         \x20   persist: true\n\
         {machine}\
         \x20   cluster:\n\
         \x20       id: REDACTED\n\
         \x20       secret: REDACTED\n\
         \x20       controlPlane:\n\
         \x20           endpoint: https://10.20.0.10:6443\n\
         \x20       network:\n\
         \x20           dnsDomain: cluster.local\n\
         \x20           podSubnets:\n\
         \x20               - 10.244.0.0/16\n\
         \x20           serviceSubnets:\n\
         \x20               - 10.96.0.0/12\n\
         \x20       token: REDACTED\n\
         \x20       ca:\n\
         \x20           crt: REDACTED\n\
         \x20           key: \"\"\n",
        machine = machine_section(live),
    )
}

/// The whole `talosctl get machineconfig -o yaml` output: the config
/// resource printed TWICE (the shape of David's 2026-09-15 read), the
/// two copies identical, separated the way talosctl separates documents.
fn live_yaml(node_ip: &str, live: &Live) -> String {
    format!(
        "{}---\n{}",
        resource(node_ip, "v1alpha1", live),
        resource(node_ip, "persistent", live)
    )
}

/// w-1 exactly as read on 2026-09-15: the seed file entry twice, the
/// seed mount once, the mirror, another mount the declaration does not
/// name.
fn w1_as_found() -> Live {
    Live {
        hostname: Some("w-1"),
        seed_file_copies: 2,
        seed_mount: true,
        image_gc: None,
        mirror: true,
        other_mount: false,
    }
}

fn run(args: &[&str], stdin: &str, patches: Option<&Path>, path_env: Option<&Path>) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    // `/bin/bash` by absolute path: the no-python case empties PATH, and
    // the shell must still be found for the script to refuse in.
    let mut cmd = Command::new("/bin/bash");
    cmd.arg(script())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = patches {
        cmd.env("BOSS_TALOS_PATCHES", p);
    }
    if let Some(p) = path_env {
        cmd.env("PATH", p);
    }
    let mut child = cmd.spawn().expect("spawn check-declared.sh");
    // A usage refusal exits before it reads stdin, so the write races
    // the exit and loses about one run in four with EPIPE (backlog
    // 28f29f0b). A closed pipe here is the child's verdict, not the
    // test's failure: the verdict is read from the exit status below.
    match child.stdin.take().unwrap().write_all(stdin.as_bytes()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => panic!("write stdin to check-declared.sh: {e}"),
    }
    child.wait_with_output().unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// The count of lines starting with a given verdict word.
fn lines_with(out: &Output, verdict: &str) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with(verdict))
        .map(str::to_string)
        .collect()
}

#[test]
fn w1_as_found_reads_the_doubled_seed_file_as_doubled_and_exits_1() {
    let out = run(
        &["w-1"],
        &live_yaml("10.20.0.14", &w1_as_found()),
        None,
        None,
    );
    let t = text(&out);
    let doubled = lines_with(&out, "DOUBLED");
    assert_eq!(doubled.len(), 1, "one DOUBLED finding expected:\n{t}");
    assert!(
        doubled[0].contains("machine.files[/var/local/gate-seed/.declared-by-talos]")
            && doubled[0].contains("2 copies"),
        "DOUBLED names the entry and the count:\n{t}"
    );
    // The doubled DOCUMENT (two resources) must not double anything else:
    // the mount and the mirror are declared once and found once.
    assert!(
        t.contains("MATCH      machine.kubelet.extraMounts[/var/local/gate-seed]"),
        "the mount matches:\n{t}"
    );
    assert!(
        t.contains("MATCH      machine.registries.mirrors[10.20.0.15:3000]"),
        "the mirror matches:\n{t}"
    );
    assert_eq!(code(&out), 1, "any DOUBLED is exit 1:\n{t}");
}

#[test]
fn w1_deduped_equals_the_declaration_and_exits_0() {
    let live = Live {
        seed_file_copies: 1,
        ..w1_as_found()
    };
    let out = run(&["w-1"], &live_yaml("10.20.0.14", &live), None, None);
    let t = text(&out);
    assert_eq!(
        code(&out),
        0,
        "a live config equal to the declaration is exit 0:\n{t}"
    );
    assert_eq!(
        lines_with(&out, "MATCH").len(),
        3,
        "file, mount, mirror:\n{t}"
    );
    for v in ["DRIFT", "ABSENT", "DOUBLED", "UNDECLARED"] {
        assert!(lines_with(&out, v).is_empty(), "no {v} expected:\n{t}");
    }
}

#[test]
fn a_file_argument_is_read_instead_of_stdin() {
    let dir = scratch("file-arg");
    let live = Live {
        seed_file_copies: 1,
        ..w1_as_found()
    };
    let f = dir.join("w-1.live.yaml");
    std::fs::write(&f, live_yaml("10.20.0.14", &live)).unwrap();
    let out = run(&["w-1", f.to_str().unwrap()], "", None, None);
    assert_eq!(code(&out), 0, "{}", text(&out));
}

#[test]
fn every_control_plane_declaration_matches_its_live_values() {
    // The numbers David read on 2026-09-15: cp-2 40/30, cp-1 and cp-3
    // 50/40, the mirror everywhere. The declarations in the tree must
    // equal them — this is the test that pins the patch files to the
    // measurement they were written from.
    for (node, ip, gc) in [
        ("cp-1", "10.20.0.11", (50, 40)),
        ("cp-2", "10.20.0.12", (40, 30)),
        ("cp-3", "10.20.0.13", (50, 40)),
    ] {
        let live = Live {
            hostname: Some(node),
            image_gc: Some(gc),
            mirror: true,
            ..Live::default()
        };
        let out = run(&[node], &live_yaml(ip, &live), None, None);
        let t = text(&out);
        assert_eq!(code(&out), 0, "{node}: {t}");
        assert_eq!(
            lines_with(&out, "MATCH").len(),
            3,
            "{node}: two GC keys + mirror:\n{t}"
        );
    }
}

#[test]
fn cp2_live_40_30_against_a_declaration_of_50_40_is_drift_naming_both() {
    let dir = scratch("drift");
    let patches = dir.join("patches");
    std::fs::create_dir_all(&patches).unwrap();
    std::fs::write(
        patches.join("cp-2.yaml"),
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n      imageGCLowThresholdPercent: 40\n",
    )
    .unwrap();
    let live = Live {
        hostname: Some("cp-2"),
        image_gc: Some((40, 30)),
        ..Live::default()
    };
    let out = run(
        &["cp-2"],
        &live_yaml("10.20.0.12", &live),
        Some(&patches),
        None,
    );
    let t = text(&out);
    let drift = lines_with(&out, "DRIFT");
    assert_eq!(drift.len(), 2, "both GC keys drift:\n{t}");
    assert!(
        drift
            .iter()
            .any(|l| l.contains("imageGCHighThresholdPercent")
                && l.contains("declared 50")
                && l.contains("live 40")),
        "DRIFT prints both values:\n{t}"
    );
    assert!(
        drift
            .iter()
            .any(|l| l.contains("imageGCLowThresholdPercent")
                && l.contains("declared 40")
                && l.contains("live 30")),
        "DRIFT prints both values:\n{t}"
    );
    assert_eq!(code(&out), 1, "{t}");
}

#[test]
fn a_declared_entry_missing_live_is_absent_and_exits_1() {
    let live = Live {
        seed_file_copies: 1,
        seed_mount: false,
        ..w1_as_found()
    };
    let out = run(&["w-1"], &live_yaml("10.20.0.14", &live), None, None);
    let t = text(&out);
    let absent = lines_with(&out, "ABSENT");
    assert_eq!(absent.len(), 1, "{t}");
    assert!(
        absent[0].contains("machine.kubelet.extraMounts[/var/local/gate-seed]"),
        "{t}"
    );
    assert_eq!(code(&out), 1, "{t}");
}

#[test]
fn a_live_entry_no_declaration_names_is_undeclared_and_does_not_fail() {
    // The mirror is live on cp-1 but a declaration that omits it — the
    // third state: reported, never a hard finding (design 16115a17).
    let dir = scratch("undeclared");
    let patches = dir.join("patches");
    std::fs::create_dir_all(&patches).unwrap();
    std::fs::write(
        patches.join("cp-1.yaml"),
        "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n      imageGCLowThresholdPercent: 40\n",
    )
    .unwrap();
    let live = Live {
        hostname: Some("cp-1"),
        image_gc: Some((50, 40)),
        mirror: true,
        other_mount: true,
        ..Live::default()
    };
    let out = run(
        &["cp-1"],
        &live_yaml("10.20.0.11", &live),
        Some(&patches),
        None,
    );
    let t = text(&out);
    let undeclared = lines_with(&out, "UNDECLARED");
    assert_eq!(undeclared.len(), 2, "the mirror and the other mount:\n{t}");
    assert!(
        undeclared.iter().any(
            |l| l.contains("machine.registries.mirrors[10.20.0.15:3000]")
                && l.contains("http://10.20.0.15:3000")
        ),
        "UNDECLARED names the entry and prints its live value:\n{t}"
    );
    assert!(
        undeclared
            .iter()
            .any(|l| l.contains("machine.kubelet.extraMounts[/var/lib/longhorn]")),
        "{t}"
    );
    assert_eq!(code(&out), 0, "UNDECLARED alone is not a failure:\n{t}");
}

#[test]
fn a_live_config_from_another_node_is_refused_not_compared() {
    // A wrong target answers instead of erroring: the live document says
    // which host it is, so the comparator refuses to read cp-1's config
    // as cp-2's.
    let live = Live {
        hostname: Some("cp-1"),
        image_gc: Some((50, 40)),
        mirror: true,
        ..Live::default()
    };
    let out = run(&["cp-2"], &live_yaml("10.20.0.11", &live), None, None);
    let t = text(&out);
    assert_eq!(code(&out), 2, "{t}");
    assert!(
        t.contains("cp-1") && t.contains("cp-2"),
        "names both hosts:\n{t}"
    );
}

#[test]
fn usage_refusals_exit_2() {
    let out = run(&[], "", None, None);
    assert_eq!(code(&out), 2, "no node:\n{}", text(&out));
    assert!(text(&out).contains("usage"), "{}", text(&out));

    let out = run(&["no-such-node"], "machine: {}\n", None, None);
    assert_eq!(
        code(&out),
        2,
        "no declaration for the node:\n{}",
        text(&out)
    );
    assert!(text(&out).contains("no-such-node"), "{}", text(&out));

    let out = run(&["w-1", "/nonexistent/live.yaml"], "", None, None);
    assert_eq!(code(&out), 2, "missing live file:\n{}", text(&out));

    let out = run(&["w-1"], "", None, None);
    assert_eq!(
        code(&out),
        2,
        "empty input carries no machine config:\n{}",
        text(&out)
    );
}

#[test]
fn without_python3_it_refuses_with_78_not_a_verdict() {
    let dir = scratch("no-python");
    let empty = dir.join("bin");
    std::fs::create_dir_all(&empty).unwrap();
    let out = run(
        &["w-1"],
        &live_yaml("10.20.0.14", &w1_as_found()),
        None,
        Some(&empty),
    );
    let t = text(&out);
    assert_eq!(code(&out), 78, "{t}");
    assert!(t.contains("python3"), "{t}");
}

#[test]
fn the_tree_declares_every_node_the_registry_names_as_a_talos_machine() {
    // The patches directory is the declaration; every Talos node in the
    // estate registry's seed (infra/postgres/schema) has a file there.
    for node in ["cp-1", "cp-2", "cp-3", "w-1"] {
        let f = patches_dir().join(format!("{node}.yaml"));
        assert!(f.is_file(), "no declaration at {}", f.display());
        let body = std::fs::read_to_string(&f).unwrap();
        assert!(
            body.starts_with("machine:") || body.contains("\nmachine:\n"),
            "{node}: a patch is a machine-config fragment"
        );
        assert!(
            body.contains("10.20.0.15:3000"),
            "{node}: every node pulls through the forge registry mirror"
        );
        assert!(
            !body.contains("token:") && !body.contains("crt:") && !body.contains("key:"),
            "{node}: no secret lives in a patch"
        );
    }
    let w1 = std::fs::read_to_string(patches_dir().join("w-1.yaml")).unwrap();
    assert_eq!(
        w1.matches("/var/local/gate-seed/.declared-by-talos")
            .count(),
        1,
        "the seed file is declared ONCE — the live double is the drift, not the intent"
    );
    // The manifest points at the declaration instead of restating it
    // (CLAUDE.md §9a: a fact that lives twice).
    let manifest =
        std::fs::read_to_string(repo_root().join("infra/cluster/manifests/gate-seed-local.yaml"))
            .unwrap();
    assert!(
        manifest.contains("infra/cluster/talos/patches/w-1.yaml"),
        "the manifest names the declaration"
    );
    assert!(
        !manifest.contains("extraMounts:"),
        "the manifest no longer restates the fragment"
    );
}
