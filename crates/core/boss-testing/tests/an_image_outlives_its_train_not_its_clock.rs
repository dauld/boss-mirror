//! AN IMAGE OUTLIVES ITS TRAIN, NOT ITS CLOCK.
//!
//! The forge's routine CI-image pass could legally collect NOTHING.
//! Measured 2026-09-11 (backlog `9195a2a6`, ops-request `28d3599c`): the
//! system docker daemon held 13 `boss-ci:<sha>` tags, 21.18 GB with
//! 18.58 GB reclaimable, and their ages spanned fifteen minutes to five
//! hours — every one of them inside the routine pass's SIX-HOUR window.
//! The pass was not failing; it was correctly declining. The window was
//! sized when the forge ran 5-14 trains a day and the measured rate is
//! now ~2.6 an hour (~60 a day), which alone guarantees ~16 images are
//! always in-window. That is the whole pile.
//!
//! AN AGE WINDOW IS A PROXY for "this image will not be needed again",
//! and the proxy broke when the rate changed. What actually makes a
//! `boss-ci:<sha>` collectable is that ITS TRAIN HAS LANDED OR BEEN
//! ABANDONED — and the system of record knows that exactly: a
//! `pr-train` packet records the sha CI built (`assemble`'s `train_ref`,
//! `train/<window>@<sha>`) and the sha it merged (`merged`'s
//! `merge_ref`), and the packet is CLOSED when the train is done. So the
//! fix replaces a heuristic about time with the fact the record already
//! holds, and it self-tunes: it does not care whether the rate is 5 or
//! 60 trains a day. All 13 images measured above resolve to closed
//! trains (verified against the SoR while writing this).
//!
//! WHAT THIS FILE PINS, and the second group matters more than the
//! first:
//!
//! 1. THE FIX. A sha whose train has landed is collectable INSIDE the
//!    age window, which is what no mechanism could do before.
//!
//! 2. THE SAFETY PROPERTY, which is one sentence: EVERY FAILURE PATH
//!    PRUNES LESS, NEVER MORE. An unreachable system of record, a reply
//!    that does not parse, a reply that sees no trains (the
//!    unauthenticated read returns an empty page, not an error), an
//!    unset `BOSS_JOBS_URL`, a sha that resolves to no train, and a sha
//!    an OPEN train still references all land on the age window — the
//!    behaviour that shipped before this change. A reclaim that widens
//!    when it cannot see is how an outage becomes a data-loss incident,
//!    so the landed set can only ever ADD a reason to collect, and an
//!    empty one leaves the pass exactly as it was.
//!
//! The sweep is RUN, not read: `df`, `docker` and `curl` are stubs on
//! `PATH` and in the env knobs the script already exposes, so every
//! verdict below is one `infra/forge/disk-floor-sweep.sh` actually
//! reached. Nothing here touches a daemon, a disk or the SoR.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use boss_testing::scratch::{scratch_dir, write_exec, write_file};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

const SWEEP: &str = "infra/forge/disk-floor-sweep.sh";
const UNIT: &str = "infra/forge/disk-floor-sweep.service";
const REPO: &str = "10.20.0.15:3000/david/boss-ci";

// The fixture's images. A CI tag is the full 40-char sha
// (`boss-ci:${{ github.sha }}`), so these are too.
/// 1h old, and its train HAS landed — the keep-newest floor outranks the
/// landed rule, because this is what a job starting right now pulls.
const NEWEST: &str = "11ab11ab11ab11ab11ab11ab11ab11ab11ab11ab";
/// 3h old — inside the window — and its train has landed. THE FIX.
const LANDED: &str = "aa11bb22cc33dd44ee55ff66aa77bb88cc99dd00";
/// 4h old, named by a closed train's `merge_ref` AND still referenced by
/// an OPEN train. The open reference VETOES the landed one.
const CONTESTED: &str = "bb11cc22dd33ee44ff55aa66bb77cc88dd99ee00";
/// 5h old and in no train at all (a gate-run or a hand build): neither
/// immortal nor instantly collectable — the age window decides.
const ORPHAN: &str = "cc11dd22ee33ff44aa55bb66cc77dd88ee99ff00";
/// 30h old and in no train: the age window collects it, as it always did.
const AGED: &str = "dd11ee22ff33aa44bb55cc66dd77ee88ff99aa00";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// The system of record's answer to `GET /api/jobs?kind=pr-train`, in
/// the shape the live API returns (verified against the cluster SoR
/// 2026-09-11): `data` is a list of packets, each with a `status` and
/// `steps[].metadata` carrying `train_ref` / `merge_ref`.
fn trains_reply() -> String {
    format!(
        r#"{{"total":1039,"data":[
          {{"id":"7a6914d7","kind":"pr-train","status":"open","metadata":{{"outcome":null}},
            "steps":[
              {{"spec_slug":"assemble","metadata":{{"train_ref":"train/20260911-1144@{contested}","car_heads":"a95bb094 fix/a-branch@{contested_short}"}}}},
              {{"spec_slug":"ci","metadata":{{"result":null}}}}
            ]}},
          {{"id":"dcf18ed1","kind":"pr-train","status":"closed","metadata":{{"outcome":"arrived"}},
            "steps":[
              {{"spec_slug":"assemble","metadata":{{"train_ref":"train/20260911-1044@{landed}"}}}},
              {{"spec_slug":"merged","metadata":{{"merge_ref":"{contested_12}"}}}}
            ]}},
          {{"id":"4df22911","kind":"pr-train","status":"closed","metadata":{{"outcome":"abandoned"}},
            "steps":[
              {{"spec_slug":"assemble","metadata":{{"train_ref":"train/20260911-0959@{newest}"}}}}
            ]}}
        ]}}"#,
        landed = &LANDED[..7],
        newest = &NEWEST[..7],
        contested = &CONTESTED[..7],
        contested_short = &CONTESTED[..7],
        contested_12 = &CONTESTED[..12],
    )
}

struct Sweep {
    dir: PathBuf,
    reply: String,
    curl_exit: i32,
    jobs_url: Option<String>,
}

impl Sweep {
    /// A sweep whose SoR answers with the fixture above.
    fn new(name: &str) -> Self {
        Sweep {
            dir: scratch_dir(&format!("boss-image-train-{name}")),
            reply: trains_reply(),
            curl_exit: 0,
            jobs_url: Some("http://10.20.0.34:7900".to_string()),
        }
    }

    fn reply(mut self, body: &str) -> Self {
        self.reply = body.to_string();
        self
    }

    fn curl_exit(mut self, code: i32) -> Self {
        self.curl_exit = code;
        self
    }

    fn no_jobs_url(mut self) -> Self {
        self.jobs_url = None;
        self
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("docker-calls")).unwrap_or_default()
    }

    fn curl_calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("curl-calls")).unwrap_or_default()
    }

    /// Every image the pass actually removed, by tag.
    fn removed(&self) -> Vec<String> {
        self.calls()
            .lines()
            .filter_map(|l| l.strip_prefix(&format!("rmi {REPO}:")))
            .map(|t| t.to_string())
            .collect()
    }

    fn run(&self) -> Output {
        let bin = self.dir.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir bin");

        // A disk far above any floor, so the run stops after the routine
        // pass and the below-floor remediations are never reached.
        write_exec(
            &bin.join("df"),
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             echo \"/dev/fake 1 1 $((900 * 1024 * 1024)) 10% /\"\n",
        );

        // The daemon. Tags, ages and sizes come from the fixture files
        // below; every call is recorded so an assertion can name what
        // was removed and what was not.
        let listing = self.dir.join("listing");
        let meta = self.dir.join("meta");
        let docker_calls = self.dir.join("docker-calls");
        write_exec(
            &bin.join("boss-stub-docker"),
            &format!(
                "#!/usr/bin/env bash\n\
                 printf '%s\\n' \"$*\" >>{calls}\n\
                 case \"$1\" in\n\
                 info) echo /var/lib/docker; exit 0 ;;\n\
                 images) cat {listing}; exit 0 ;;\n\
                 image) shift; [ \"$1\" = inspect ] || exit 1; id=\"${{!#}}\";\n\
                   line=\"$(grep \"^$id \" {meta})\" || exit 1; printf '%s\\n' \"${{line#* }}\"; exit 0 ;;\n\
                 rmi) echo \"Untagged: $2\"; exit 0 ;;\n\
                 esac\n\
                 exit 127\n",
                calls = docker_calls.display(),
                listing = listing.display(),
                meta = meta.display(),
            ),
        );

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let gb: u64 = 1_073_741_824;
        let mut listing_body = String::new();
        let mut meta_body = String::new();
        for (tag, hours, bytes) in [
            (NEWEST, 1u64, 2 * gb),
            (LANDED, 3, 4 * gb),
            (CONTESTED, 4, 3 * gb),
            (ORPHAN, 5, 3 * gb),
            (AGED, 30, 2 * gb),
            ("rust1.96", 400, 3 * gb),
        ] {
            listing_body.push_str(&format!("{tag} {tag}\n"));
            let created = Command::new("date")
                .args([
                    "-u",
                    "-d",
                    &format!("@{}", now - hours * 3600),
                    "+%Y-%m-%dT%H:%M:%SZ",
                ])
                .output()
                .expect("date runs");
            meta_body.push_str(&format!(
                "{tag} {} {bytes}\n",
                String::from_utf8_lossy(&created.stdout).trim()
            ));
        }
        write_file(&listing, &listing_body);
        write_file(&meta, &meta_body);

        // The system of record, over a stub curl: the reply is a fixture
        // and every invocation is recorded, so the read's ACTOR can be
        // asserted — an unauthenticated read sees an empty world rather
        // than an error.
        write_exec(
            &bin.join("boss-stub-curl"),
            &format!(
                "#!/usr/bin/env bash\n\
                 printf '%s\\n' \"$*\" >>{calls}\n\
                 cat {reply}\n\
                 exit {code}\n",
                calls = self.dir.join("curl-calls").display(),
                reply = self.dir.join("reply.json").display(),
                code = self.curl_exit,
            ),
        );
        write_file(&self.dir.join("reply.json"), &self.reply);

        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SWEEP))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("BOSS_CI_IMAGE_DOCKER", bin.join("boss-stub-docker"))
            .env("BOSS_CI_IMAGE_DAEMON_ROOT", "/var/lib/docker")
            .env("BOSS_CI_IMAGE_REPO", REPO)
            .env("BOSS_CI_IMAGE_AGE_HOURS", "6")
            .env("BOSS_CI_IMAGE_KEEP_NEWEST", "1")
            .env("BOSS_SWEEP_CURL_CMD", bin.join("boss-stub-curl"))
            .env("BOSS_DISK_FLOOR_GB", "100")
            .env_remove("BOSS_JOBS_URL")
            .current_dir(repo_root());
        if let Some(url) = &self.jobs_url {
            cmd.env("BOSS_JOBS_URL", url);
        }
        cmd.output().unwrap_or_else(|e| panic!("run {SWEEP}: {e}"))
    }
}

fn say(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn log(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn require_jq() {
    // Not a skip. A lint that printed "no jq on this box" and passed is
    // why jq is a declared required tool of the CI image
    // (infra/forge/boss-ci/required-tools.txt).
    assert!(
        has("jq"),
        "jq is missing, and this suite would otherwise pass by skipping — \
         jq is declared in infra/forge/boss-ci/required-tools.txt"
    );
}

// ---------------------------------------------------------------------
// 1. The fix
// ---------------------------------------------------------------------

/// A landed train's image is collectable INSIDE the age window. Nothing
/// could do this before: at ~60 trains a day every image in the daemon
/// is inside six hours, so the routine pass declined all 13 of them.
#[test]
fn a_landed_trains_image_is_collectable_inside_the_age_window() {
    require_jq();
    let sweep = Sweep::new("landed-inside-window");
    let out = sweep.run();
    let removed = sweep.removed();
    assert!(
        removed.iter().any(|t| t == LANDED),
        "the 3h image whose train is CLOSED was not collected, so the pass is still \
         bounded by the clock rather than by the record (backlog 9195a2a6). removed: \
         {removed:?}\n{}",
        say(&out)
    );
    assert!(
        log(&out).contains("its train"),
        "the removal does not say WHY it was collectable — a journal line that \
         says THAT and not WHAT is the defect class that cost a day\n{}",
        say(&out)
    );
}

/// The keep-newest floor outranks the landed rule. The newest tags are
/// what a job starting right now pulls, and the merge-sha CI run on main
/// begins while the train that produced it is already closing.
#[test]
fn the_newest_image_survives_even_when_its_train_has_landed() {
    require_jq();
    let sweep = Sweep::new("keep-newest-wins");
    let out = sweep.run();
    assert!(
        !sweep.removed().iter().any(|t| t == NEWEST),
        "the newest image was collected because its train had landed — keep-newest \
         is the grace a starting job depends on\n{}",
        say(&out)
    );
}

// ---------------------------------------------------------------------
// 2. Every failure path prunes LESS
// ---------------------------------------------------------------------

/// What the age window alone collects: the 30h image, and nothing else.
/// This is the pass's behaviour before this change, and the floor every
/// failure path below must fall back to — never past it.
fn assert_age_window_only(sweep: &Sweep, out: &Output, why: &str) {
    let removed = sweep.removed();
    assert_eq!(
        removed,
        vec![AGED.to_string()],
        "{why}: the pass did not fall back to the age window. It must prune LESS \
         when it cannot see, never more — a reclaim that widens when blind is how \
         an outage becomes a data-loss incident.\n{}",
        say(out)
    );
}

#[test]
fn an_unreachable_system_of_record_falls_back_to_the_age_window() {
    require_jq();
    let sweep = Sweep::new("sor-unreachable").curl_exit(7);
    let out = sweep.run();
    assert_age_window_only(&sweep, &out, "curl exited 7");
    assert!(
        log(&out).contains("age window"),
        "an unreadable SoR is silent about it — the operator cannot tell a pass that \
         consulted the record from one that could not\n{}",
        say(&out)
    );
    assert!(
        out.status.success(),
        "the sweep FAILED because the system of record was unreachable. Visibility is \
         best-effort and destruction is not: the hourly pass must still defend the \
         disk during an SoR outage\n{}",
        say(&out)
    );
}

#[test]
fn a_reply_that_does_not_parse_falls_back_to_the_age_window() {
    require_jq();
    let sweep = Sweep::new("sor-garbage").reply("<html>502 Bad Gateway</html>");
    let out = sweep.run();
    assert_age_window_only(&sweep, &out, "the reply was not JSON");
}

/// An unauthenticated read does not error — it answers with a SMALLER
/// WORLD (measured: `total: 0` from raw curl against the same backend
/// boss-api reads 1 from). An empty page is therefore a FAILED read, not
/// the fact that no train exists.
#[test]
fn a_reply_that_sees_no_trains_is_a_failed_read_not_an_empty_world() {
    require_jq();
    let sweep = Sweep::new("sor-empty").reply(r#"{"total":0,"data":[]}"#);
    let out = sweep.run();
    assert_age_window_only(&sweep, &out, "the reply listed no trains");
    // The keys are empty either way, so what is pinned is that the pass
    // SAYS it could not read the record. A mechanism that reads an empty
    // page as fact can retire itself and still look healthy.
    assert!(
        log(&out).contains("listed NO pr-train packets"),
        "an empty page was taken as the fact that no train exists, rather than as \
         a failed read — a denied scope answers with a smaller world, not an \
         error\n{}",
        say(&out)
    );
}

#[test]
fn an_unset_jobs_url_falls_back_to_the_age_window() {
    require_jq();
    let sweep = Sweep::new("no-jobs-url").no_jobs_url();
    let out = sweep.run();
    assert_age_window_only(&sweep, &out, "BOSS_JOBS_URL was not set");
    assert!(
        sweep.curl_calls().is_empty(),
        "the pass asked the network without being told which system of record to \
         ask — a read against the wrong instance answers instead of erroring\n{}",
        say(&out)
    );
}

/// A sha in no train at all — a gate-run image, a rebuilt sha, a hand
/// build. Neither immortal nor instantly collectable: the age window
/// decides, exactly as it did before.
#[test]
fn a_sha_that_belongs_to_no_train_is_left_to_the_age_window() {
    require_jq();
    let sweep = Sweep::new("orphan-sha");
    let out = sweep.run();
    let removed = sweep.removed();
    assert!(
        !removed.iter().any(|t| t == ORPHAN),
        "a 5h image belonging to NO train was collected — the record cannot vouch \
         for it, so only its age may\n{}",
        say(&out)
    );
    assert!(
        removed.iter().any(|t| t == AGED),
        "a 30h image belonging to no train was NOT collected — an unresolvable sha \
         must not become immortal either\n{}",
        say(&out)
    );
}

/// A sha an OPEN train still references is never collected as landed,
/// even when a CLOSED train names it too. The open reference wins,
/// because that is the direction that prunes less.
#[test]
fn an_open_trains_sha_is_never_collected_as_landed() {
    require_jq();
    let sweep = Sweep::new("contested-sha");
    let out = sweep.run();
    assert!(
        !sweep.removed().iter().any(|t| t == CONTESTED),
        "a sha a closed train merged AND an open train still references was \
         collected — an in-flight train's image is pulled by jobs that have not \
         started yet\n{}",
        say(&out)
    );
}

/// The read is signed. An unauthenticated read is refused a scope and
/// answers with an empty page, which this pass treats as a failed read —
/// so an unnamed actor would silently retire the whole mechanism.
#[test]
fn the_read_of_the_record_is_signed_by_a_named_actor() {
    require_jq();
    let sweep = Sweep::new("signed-read");
    let out = sweep.run();
    let calls = sweep.curl_calls();
    assert!(
        calls.contains("x-boss-user"),
        "the SoR read carries no actor — an unauthenticated read sees a smaller \
         world (total 0) rather than an error, which would retire this pass \
         silently. curl calls: {calls}\n{}",
        say(&out)
    );
    assert!(
        calls.contains("kind=pr-train"),
        "the read does not ask for pr-train packets. curl calls: {calls}\n{}",
        say(&out)
    );
}

// ---------------------------------------------------------------------
// 3. The two passes stay ordered
// ---------------------------------------------------------------------

/// The below-floor pass must stay at least as aggressive as the routine
/// one on the dimension it owns: a TIGHTER age window, over EVERY unused
/// image in the daemon rather than one repo's sha tags. Otherwise the
/// backstop is just the cadence again.
#[test]
fn the_emergency_pass_stays_at_least_as_aggressive_as_the_routine_one() {
    let sweep = read(SWEEP);
    let num = |prefix: &str| -> u32 {
        sweep
            .lines()
            .find(|l| l.starts_with(prefix))
            .and_then(|l| {
                l.rsplit(|c: char| !c.is_ascii_digit())
                    .find(|s| !s.is_empty())
                    .and_then(|s| s.parse().ok())
            })
            .unwrap_or_else(|| panic!("{SWEEP} no longer names a window with {prefix}"))
    };
    let routine = num("CI_IMAGE_AGE_HOURS=");
    let emergency = num("CI_IMAGE_FLOOR_AGE_HOURS=");
    assert!(
        routine > emergency,
        "the routine window ({routine}h) is not looser than the below-floor one \
         ({emergency}h)"
    );
    assert!(
        sweep.contains(r#"image prune -af --filter "until=${CI_IMAGE_FLOOR_AGE_HOURS}h""#),
        "the below-floor pass no longer prunes EVERY unused image on its own \
         window — the routine pass covers one repo's sha tags, so the backstop \
         has to be broader"
    );
}

/// The backstop owes the system of record nothing. An SoR outage must
/// not weaken the pass that defends the disk below its floor.
#[test]
fn the_below_floor_remediations_do_not_depend_on_the_record() {
    let sweep = read(SWEEP);
    let floor_half = sweep
        .split_once("reclaiming regenerable docker caches")
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_else(|| panic!("{SWEEP} no longer logs the below-floor branch"));
    for token in ["LANDED", "landed_train_shas", "BOSS_JOBS_URL"] {
        assert!(
            !floor_half.contains(token),
            "the below-floor remediations reference `{token}` — an arm that needs \
             the system of record is not an arm when the record is what is down"
        );
    }
}

/// The installed unit has to name the system of record, or the pass runs
/// blind on the host and silently keeps the old behaviour.
#[test]
fn the_unit_names_the_system_of_record() {
    let unit = read(UNIT);
    assert!(
        unit.lines().any(|l| l
            .trim()
            .starts_with("Environment=BOSS_JOBS_URL=http://10.20.0.34:7900")),
        "{UNIT} does not pin Environment=BOSS_JOBS_URL — ExecStart inherits no \
         environment from its siblings' inline `env`, so the sweep would read no \
         trains on the host and fall back to the age window forever"
    );
}

/// One deletion loop, not two. A second copy of a loop that removes
/// images is the drifting pair CLAUDE.md §9a bans, and the landed rule
/// is a predicate inside the existing loop rather than a new pass.
#[test]
fn there_is_one_deletion_loop() {
    let sweep = read(SWEEP);
    assert!(
        sweep.contains("prune-ci-images.lib.sh") && sweep.contains("landed-train-shas.lib.sh"),
        "{SWEEP} does not source both the one prune loop and the one resolver of \
         train state"
    );
    let lib = read("infra/forge/prune-ci-images.lib.sh");
    let removals = lib
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter(|l| l.contains("$docker_cmd rmi"))
        .count();
    assert_eq!(
        removals, 1,
        "the prune library has {removals} lines that can run `rmi`, not one — a \
         second copy of a loop that deletes images is the drifting pair §9a bans"
    );
}

/// The routine pass runs BEFORE the floor early-return, which is what
/// makes it routine. (Pinned here too because this file drives the
/// script with the disk far above the floor: if the pass moved below the
/// early return, every test above would go quiet rather than red.)
#[test]
fn the_routine_pass_runs_above_the_floor() {
    require_jq();
    let sweep = Sweep::new("above-floor");
    let out = sweep.run();
    assert!(
        log(&out).contains("nothing to do"),
        "the fixture disk is far above the floor but the sweep went on to the \
         below-floor remediations\n{}",
        say(&out)
    );
    assert!(
        !sweep.removed().is_empty(),
        "the routine pass collected nothing on a run that never went below the \
         floor — it is gated behind the floor again (backlog e5dc60e4)\n{}",
        say(&out)
    );
}

/// Sanity on the harness itself: the stubs are stubs, and the fixture
/// tags are the 40-char shas CI stamps.
#[test]
fn the_fixture_tags_are_shaped_like_a_ci_tag() {
    for tag in [NEWEST, LANDED, CONTESTED, ORPHAN, AGED] {
        assert_eq!(tag.len(), 40, "{tag} is not a full sha");
        assert!(
            tag.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "{tag} is not lowercase hex"
        );
    }
    assert!(
        Path::new(&repo_root().join(SWEEP)).exists(),
        "{SWEEP} is missing"
    );
}
