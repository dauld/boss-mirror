//! The nightly backup must leave a packet — and never lie in it.
//!
//! WHAT WAS BROKEN (backlog 60095754, measured 2026-09-08). The cluster
//! CronJob `boss-pg-backup` runs at 09:10 UTC and SUCCEEDS: its three
//! most recent Jobs read Complete, the artefact verifies, both offsite
//! legs ship. The system of record had not seen a backup since
//! 2026-08-19, because the packet-filing lived on the RETIRED boss-gcp
//! host timer (`infra/boss-backup.service`, whose ExecStartPre called
//! `boss-maintenance-wrap.sh maintenance-backup`) and the CronJob that
//! replaced it at the 2026-09-04 cutover never took the wrap with it.
//! Seven of the nine cluster CronJobs call the wrap; this was the only
//! nightly maintenance job the SoR could not see. CLAUDE.md §Diagnosis:
//! a check nobody reads is a check that is not running — and once the
//! silence sweep alarms on this kind, its first alarm is
//! indistinguishable from a real backup failure.
//!
//! WHY THIS TEST EXECUTES THE MANIFEST INSTEAD OF READING IT. A textual
//! assertion can say the wrap is mentioned; it cannot say the packet
//! OPENS before the dump, CLOSES after both offsite legs, and — the
//! property that matters most — does NOT close green when the dump or
//! the upload fails. So the container scripts are lifted out of
//! `boss-backup.yaml` exactly as they ship, their container mount paths
//! rebased into a temp dir, and run under bash in the order Kubernetes
//! runs them: initContainers in sequence, STOP at the first failure,
//! then the main containers.
//!
//! WHAT IS FAKED, AND WHY ONLY THAT. `pg_dump`, `ssh`, `gcloud` and
//! `apk` come from the container images and cannot run here, so they
//! are shims that record what they were asked to do. `gzip`, `du`,
//! `find`, `grep` and `tail` are the real thing, so the artefact
//! verification this manifest is written around is genuinely exercised
//! — including the case it exists for, a `pg_dump` that dies mid-stream
//! and leaves a perfectly valid gzip.
//!
//! `boss-maintenance-wrap.sh` and `boss-step.sh` are shimmed too, and
//! that is a deliberate choice rather than a shortcut. Driving the real
//! ones needs `jq`, which the CI image's own manifest
//! (`infra/forge/boss-ci/required-tools.txt`) does not promise and the
//! dev pod does not have — two lints already say so in their output
//! ("no jq on this box") and skip a branch each. A test that skips is a
//! check that is not running, and this one must run everywhere.
//! Their HTTP behaviour is already pinned where it belongs, by
//! `infra/lint/timers-leave-a-packet.sh` check 6, and is shared
//! unchanged with seven sibling CronJobs. What is NEW here, and what
//! nothing else could see, is the POD: which leg opens the packet, when
//! it closes, what it carries, and what happens when a leg fails. The
//! shims record their argv, so this test reads the exact call the
//! manifest makes.

use std::path::PathBuf;
use std::process::Command;

const MANIFEST: &str = "infra/cluster/manifests/boss-backup.yaml";

/// The containers, in the order Kubernetes must run them. The packet
/// opens first (so a run that dies in the dump still has somewhere to
/// be seen), the dump is ordered ahead of the offsite legs, and the
/// closer is the only main container — it runs if and only if every
/// leg before it passed, which is what makes `result=ok` true when it
/// writes it.
const INIT_ORDER: &[&str] = &["open-packet", "dump", "offsite-gcs"];
const MAIN_ORDER: &[&str] = &["close-packet"];

const KIND: &str = "maintenance-backup";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn manifest() -> String {
    let path = repo_root().join(MANIFEST);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The `- name: X` entries directly under an `initContainers:` /
/// `containers:` key, in file order.
fn container_names(yaml: &str, section: &str) -> Vec<String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == format!("{section}:"))
        .unwrap_or_else(|| panic!("{MANIFEST} has no `{section}:` key"));
    let base = indent_of(lines[start]);
    let mut names = Vec::new();
    for line in &lines[start + 1..] {
        if line.trim().is_empty() {
            continue;
        }
        if indent_of(line) <= base {
            break;
        }
        if indent_of(line) != base + 2 {
            continue; // a key inside a container, not a container
        }
        if let Some(rest) = line.trim().strip_prefix("- name: ") {
            names.push(rest.trim().to_string());
        }
    }
    names
}

/// The shell body of one container's `args:` block scalar, dedented.
fn container_script(yaml: &str, name: &str) -> String {
    let lines: Vec<&str> = yaml.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == format!("- name: {name}"))
        .unwrap_or_else(|| panic!("{MANIFEST} has no container named `{name}`"));
    let base = indent_of(lines[start]);

    let mut args_at = None;
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim().is_empty() {
            continue;
        }
        if line.trim() == "args:" {
            args_at = Some(i);
            break;
        }
        if indent_of(line) <= base {
            break; // the next container, or out of this one
        }
    }
    let args_at =
        args_at.unwrap_or_else(|| panic!("container `{name}` in {MANIFEST} has no `args:` block"));
    let marker = lines
        .iter()
        .enumerate()
        .skip(args_at + 1)
        .find(|(_, l)| !l.trim().is_empty())
        .map(|(i, _)| i)
        .unwrap_or_else(|| panic!("container `{name}`'s `args:` is empty"));
    assert_eq!(
        lines[marker].trim(),
        "- |",
        "container `{name}` in {MANIFEST} must carry its script as a `- |` block scalar so this \
         test can run exactly what ships"
    );
    let marker_indent = indent_of(lines[marker]);

    let mut body: Vec<&str> = Vec::new();
    for line in &lines[marker + 1..] {
        if line.trim().is_empty() {
            body.push("");
            continue;
        }
        if indent_of(line) <= marker_indent {
            break;
        }
        body.push(line);
    }
    let min = body
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| indent_of(l))
        .min()
        .unwrap_or(0);
    body.iter()
        .map(|l| if l.len() > min { &l[min..] } else { "" })
        .collect::<Vec<_>>()
        .join("\n")
}

// ------------------------------------------------------------- the pod

/// Which leg of the run should fail.
#[derive(Clone, Copy, PartialEq)]
enum Fail {
    Nothing,
    /// The jobs API refuses to open the packet — a 400, the shape a
    /// kind the instance has never heard of produces.
    Open,
    /// pg_dump dies mid-stream: a truncated dump that is still a valid
    /// gzip. Only the trailer check catches it.
    DumpTruncated,
    /// The offsite ship to the playground box fails.
    Ship,
    /// The GCS upload fails — the dump is good, the second offsite copy
    /// is not.
    Upload,
    /// The packet will not close (the SoR is dark). The backup already
    /// happened; only its visibility is lost.
    Close,
}

impl Fail {
    fn tag(self) -> &'static str {
        match self {
            Fail::Nothing => "",
            Fail::Open => "open",
            Fail::DumpTruncated => "dump",
            Fail::Ship => "ship",
            Fail::Upload => "upload",
            Fail::Close => "close",
        }
    }
}

struct PodRun {
    /// Container name and exit status, in the order they ran.
    ran: Vec<(String, i32)>,
    /// Everything the run did, in order: `open:`, `exec:`, `close:`.
    seq: Vec<String>,
    log: String,
}

impl PodRun {
    fn succeeded(&self) -> bool {
        self.ran.iter().all(|(_, rc)| *rc == 0)
    }
    fn ran_container(&self, name: &str) -> bool {
        self.ran.iter().any(|(n, _)| n == name)
    }
    fn position(&self, prefix: &str) -> Option<usize> {
        self.seq.iter().position(|l| l.starts_with(prefix))
    }
    /// The packet-opening calls this run made.
    fn opens(&self) -> Vec<&String> {
        self.seq.iter().filter(|l| l.starts_with("open:")).collect()
    }
    /// The packet-closing calls this run made — the only writes that
    /// can put the packet on a terminal.
    fn closes(&self) -> Vec<&String> {
        self.seq
            .iter()
            .filter(|l| l.starts_with("close:"))
            .collect()
    }
}

fn write_exec(path: &PathBuf, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// Run the manifest's containers the way Kubernetes would.
fn run_pod(tag: &str, fail: Fail) -> PodRun {
    let yaml = manifest();
    // No thread id in the path: `ThreadId(3)` puts parentheses in a
    // directory name the container scripts then have to quote. Each
    // caller passes its own tag, which is what makes this unique.
    let dir = std::env::temp_dir().join(format!("boss-backup-pod-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["backup", "keys", "gcs", "bin"] {
        std::fs::create_dir_all(dir.join(sub)).expect("scratch dirs");
    }
    let seq = dir.join("sequence.log");
    std::fs::write(&seq, "").expect("seed sequence log");
    std::fs::write(dir.join("keys/id_ed25519"), "not-a-key\n").expect("fake ship key");
    std::fs::write(dir.join("gcs/bucket"), "boss-offsite-test").expect("fake bucket name");
    std::fs::write(dir.join("gcs/sa.json"), "{}\n").expect("fake sa key");

    // ---- what the images provide and this host does not
    let bin = dir.join("bin");
    // The dump leg installs openssh-client with apk.
    write_exec(&bin.join("apk"), "#!/usr/bin/env bash\nexit 0\n");
    // pg_dump writes a real dump; the `dump` failure mode stops
    // mid-stream and exits non-zero, which `set -e` does NOT catch on
    // the left of a pipe — the exact hole the manifest's `gzip -t` +
    // trailer check plugs.
    write_exec(
        &bin.join("pg_dump"),
        "#!/usr/bin/env bash\n\
         echo \"exec:pg_dump\" >> \"$BOSS_TEST_SEQ\"\n\
         echo '-- PostgreSQL database dump'\n\
         echo 'CREATE TABLE audit_log ();'\n\
         if [ \"$BOSS_TEST_FAIL\" = \"dump\" ]; then exit 1; fi\n\
         echo '-- PostgreSQL database dump complete'\n\
         exit 0\n",
    );
    write_exec(
        &bin.join("ssh"),
        "#!/usr/bin/env bash\n\
         cat > /dev/null\n\
         echo \"exec:ship\" >> \"$BOSS_TEST_SEQ\"\n\
         if [ \"$BOSS_TEST_FAIL\" = \"ship\" ]; then exit 255; fi\n\
         exit 0\n",
    );
    write_exec(
        &bin.join("gcloud"),
        "#!/usr/bin/env bash\n\
         case \"$1 ${2:-}\" in\n\
           'auth activate-service-account') exit 0 ;;\n\
           'storage cp')\n\
             echo \"exec:upload\" >> \"$BOSS_TEST_SEQ\"\n\
             if [ \"$BOSS_TEST_FAIL\" = \"upload\" ]; then echo 'upload failed' >&2; exit 1; fi\n\
             exit 0 ;;\n\
           'storage ls') echo \"gs://boss-offsite-test/boss-20260908-091100.sql.gz\"; exit 0 ;;\n\
           'storage rm') exit 0 ;;\n\
           *) exit 0 ;;\n\
         esac\n",
    );
    // The two maintenance helpers, recording the exact call the
    // manifest makes. Both refuse without BOSS_JOBS_URL exactly as the
    // real ones do (exit 78, EX_CONFIG), so a manifest that forgets the
    // env fails here rather than at 09:10 UTC.
    write_exec(
        &bin.join("boss-maintenance-wrap.sh"),
        "#!/usr/bin/env bash\n\
         echo \"open:$*\" >> \"$BOSS_TEST_SEQ\"\n\
         if [ -z \"${BOSS_JOBS_URL:-}\" ]; then echo 'no BOSS_JOBS_URL' >&2; exit 78; fi\n\
         if [ \"$BOSS_TEST_FAIL\" = \"open\" ]; then echo 'HTTP 400' >&2; exit 22; fi\n\
         exit 0\n",
    );
    write_exec(
        &bin.join("boss-step.sh"),
        "#!/usr/bin/env bash\n\
         echo \"close:$*\" >> \"$BOSS_TEST_SEQ\"\n\
         if [ -z \"${BOSS_JOBS_URL:-}\" ]; then echo 'no BOSS_JOBS_URL' >&2; exit 78; fi\n\
         if [ \"$BOSS_TEST_FAIL\" = \"close\" ]; then echo 'jobs-api unreachable' >&2; exit 1; fi\n\
         exit 0\n",
    );

    // ---- run the containers
    let mut ran: Vec<(String, i32)> = Vec::new();
    let mut log = String::new();
    let mut aborted = false;
    for (phase, names) in [("init", INIT_ORDER), ("main", MAIN_ORDER)] {
        for name in names {
            if aborted {
                break;
            }
            // The container mount paths, rebased into the scratch dir.
            // /usr/local/bin is where the image puts the maintenance
            // helpers; here it holds the recording shims.
            let script = container_script(&yaml, name)
                .replace("/usr/local/bin/", &format!("{}/", bin.display()))
                .replace("/backup", &format!("{}/backup", dir.display()))
                .replace("/keys", &format!("{}/keys", dir.display()))
                .replace("/gcs", &format!("{}/gcs", dir.display()))
                .replace("/tmp/k", &format!("{}/k", dir.display()));
            let path = dir.join(format!("{name}.sh"));
            std::fs::write(&path, &script).expect("write container script");
            let out = Command::new("bash")
                .arg(&path)
                .current_dir(&dir)
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        bin.display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                )
                .env("BOSS_JOBS_URL", "http://boss-jobs-internal.test:7900")
                .env("BOSS_TEST_SEQ", &seq)
                .env("BOSS_TEST_FAIL", fail.tag())
                .env("DATABASE_URL", "postgres://stub/boss")
                .output()
                .expect("bash runs the container script");
            let rc = out.status.code().unwrap_or(-1);
            log.push_str(&format!(
                "== {phase}/{name} (exit {rc})\n{}{}\n",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ));
            ran.push(((*name).to_string(), rc));
            // Kubernetes: an initContainer that fails stops the pod —
            // no later init container and no main container runs.
            if rc != 0 {
                aborted = true;
            }
        }
    }

    let seq_lines = std::fs::read_to_string(&seq)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    PodRun {
        ran,
        seq: seq_lines,
        log,
    }
}

// --------------------------------------------------------------- tests

/// THE DEFECT, DIRECTLY: a healthy nightly run must open a packet
/// before it dumps, and close it green only after both offsite legs.
#[test]
fn a_healthy_run_opens_a_packet_and_closes_it_green() {
    let r = run_pod("green", Fail::Nothing);
    assert!(
        r.succeeded(),
        "every container must pass on the happy path:\n{}",
        r.log
    );

    let opens = r.opens();
    assert_eq!(
        opens.len(),
        1,
        "the run must open exactly one packet: {opens:?}\n{}",
        r.log
    );
    assert!(
        opens[0].contains(KIND),
        "the packet must be of kind `{KIND}` — the kind the retired boss-gcp timer filed and \
         the silence sweep still watches: {}",
        opens[0]
    );

    let open = r.position("open:").expect("the open is in the sequence");
    let dump = r.position("exec:pg_dump").expect("the dump ran");
    assert!(
        open < dump,
        "the packet must open BEFORE the dump, so a run that dies in the dump still has \
         somewhere to be seen.\nsequence: {:?}",
        r.seq
    );

    let closes = r.closes();
    assert_eq!(
        closes.len(),
        1,
        "exactly one call must close the packet: {closes:?}\n{}",
        r.log
    );
    let close = closes[0];
    assert!(
        close.contains(KIND) && close.contains(" run "),
        "the close must complete the `run` step of the SAME kind the open filed: {close}"
    );
    assert!(
        close.contains("result=ok"),
        "the close must record result=ok — the maintenance protocol routes its `completed` \
         terminal on that exact word: {close}"
    );
    assert!(
        close.contains("dump=boss-") && close.contains(".sql.gz"),
        "the packet must carry the dump THIS run produced, by name: {close}"
    );
    assert!(
        close.contains("size="),
        "the packet must carry the dump's size: {close}"
    );
    assert!(
        close.contains("offsite="),
        "the packet must name where the dump went — the run already prints it: {close}"
    );

    let ship = r.position("exec:ship").expect("the offsite ship ran");
    let upload = r.position("exec:upload").expect("the GCS upload ran");
    let closed = r.position("close:").expect("the close is in the sequence");
    assert!(
        dump < ship && ship < upload && upload < closed,
        "ordering must be dump -> offsite -> close. A packet that closes before the offsite \
         legs finish is a backup that looks present.\nsequence: {:?}",
        r.seq
    );
}

/// THE PROPERTY THE MANIFEST IS WRITTEN AROUND: a dump that dies
/// mid-stream leaves a VALID gzip, so only pg_dump's own trailer
/// catches it. When it does, the packet must not close green.
#[test]
fn a_truncated_dump_never_closes_the_packet_green() {
    let r = run_pod("truncated", Fail::DumpTruncated);
    assert!(
        !r.succeeded(),
        "a truncated dump must fail the Job loudly — a backup that looks present is worse \
         than none:\n{}",
        r.log
    );
    assert!(
        !r.opens().is_empty(),
        "the failing run must still have opened its packet, or nothing records that it ran \
         at all:\n{}",
        r.log
    );
    assert!(
        !r.ran_container("close-packet"),
        "the closer ran after a failed dump. Kubernetes stops the pod at a failed \
         initContainer, so the closer must sit AFTER every leg it vouches for:\n{}",
        r.log
    );
    assert!(
        r.closes().is_empty(),
        "a failed run closed its packet: {:?}\nIt must stay open — the next successful run's \
         wrap reuses it (the recovery boss-maintenance-wrap.sh documents), and the silence \
         sweep can see it meanwhile.\n{}",
        r.closes(),
        r.log
    );
}

/// Either offsite leg failing is the same rule: the dump is good, the
/// run is not, and the packet must not say ok.
#[test]
fn a_failed_offsite_leg_never_closes_the_packet_green() {
    for (tag, fail) in [("ship", Fail::Ship), ("upload", Fail::Upload)] {
        let r = run_pod(tag, fail);
        assert!(
            !r.succeeded(),
            "a failed {tag} leg must fail the Job:\n{}",
            r.log
        );
        assert!(
            r.closes().is_empty(),
            "the packet closed despite a failed {tag} leg: {:?}\n{}",
            r.closes(),
            r.log
        );
    }
}

/// CLAUDE.md §Diagnosis, "an arm that needs the patient is not an arm".
/// The packet is visibility, never a precondition: a jobs API that
/// refuses the open (400, the shape an unseeded kind produces — it cost
/// boss-ml-inference-batch 23 nights) must not stop the backup.
#[test]
fn a_refused_packet_does_not_stop_the_backup() {
    let r = run_pod("refused", Fail::Open);
    assert!(
        r.succeeded(),
        "the backup must run when its packet cannot be opened. A visibility fault that stops \
         the executor is the outage:\n{}",
        r.log
    );
    assert!(
        r.position("exec:pg_dump").is_some()
            && r.position("exec:ship").is_some()
            && r.position("exec:upload").is_some(),
        "every leg must still have run:\nsequence: {:?}\n{}",
        r.seq,
        r.log
    );
}

/// The mirror image, and the reason the closer is LAST rather than
/// hard: a jobs API that is dark when the run finishes must not fail a
/// Job whose backup succeeded. `backoffLimit: 1` would otherwise re-run
/// the whole dump-and-ship for a lost HTTP call.
#[test]
fn a_packet_that_will_not_close_does_not_fail_a_good_backup() {
    let r = run_pod("dark", Fail::Close);
    assert!(
        r.succeeded(),
        "the closer must exit 0 when the SoR is dark: the dump ran, shipped and uploaded, and \
         only this run's visibility is lost. Failing here re-runs a good backup:\n{}",
        r.log
    );
    assert_eq!(
        r.closes().len(),
        1,
        "the close must still have been attempted, and its failure logged:\n{}",
        r.log
    );
}

/// The ordering guarantee, stated in the manifest's own shape. The dump
/// must be an initContainer ahead of the offsite legs — sibling
/// containers in a Pod start together, so a main-container upload could
/// run while pg_dump was still writing.
#[test]
fn the_manifest_orders_the_legs_and_closes_last() {
    let yaml = manifest();
    assert_eq!(
        container_names(&yaml, "initContainers"),
        INIT_ORDER,
        "the initContainers must be exactly {INIT_ORDER:?}, in that order: the packet opens \
         first, the dump is ordered ahead of the offsite legs, and everything the closer \
         vouches for happens before it"
    );
    assert_eq!(
        container_names(&yaml, "containers"),
        MAIN_ORDER,
        "the only main container must be the closer. It runs if and only if every \
         initContainer passed, which is what makes `result=ok` true when it writes it"
    );
}

/// The artefact verification is the reason this file is careful. It
/// must survive every change to the packet wiring.
#[test]
fn the_artefact_verification_survives() {
    let dump = container_script(&manifest(), "dump");
    assert!(
        dump.contains("gzip -t \"$OUT\""),
        "the dump must still prove the gzip stream is whole — `set -e` does not fire for the \
         LEFT side of a pipe, so a pg_dump that died mid-stream left a truncated .gz and a Job \
         that reported success"
    );
    assert!(
        dump.contains("PostgreSQL database dump complete"),
        "the dump must still check pg_dump's own trailer: a truncated dump is a VALID gzip, so \
         `gzip -t` alone cannot see it"
    );
    assert!(
        dump.contains("echo \"$OUT\" > /backup/.latest"),
        "the dump must still name the file THIS run produced, so the upload leg cannot quietly \
         ship yesterday's"
    );
}

/// Both boss-image containers must name the system of record. The
/// helpers refuse without it (deliberately — 21 nightly packets once
/// landed on a non-authoritative instance behind a localhost default),
/// and a CronJob inherits no systemd drop-in.
#[test]
fn the_packet_containers_name_the_system_of_record() {
    let yaml = manifest();
    let count = yaml.matches("name: BOSS_JOBS_URL").count();
    assert!(
        count >= 2,
        "both the opener and the closer must declare BOSS_JOBS_URL (found {count}). \
         boss-maintenance-wrap.sh and boss-step.sh have no default and exit 78 without it."
    );
    assert!(
        yaml.contains("boss-jobs-internal.boss.svc.cluster.local:7900"),
        "the packet containers must point at the in-cluster jobs API by service DNS, the way \
         every sibling CronJob does"
    );
}
