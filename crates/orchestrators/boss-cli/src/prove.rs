//! `boss prove <car>` — proof is a receipt, not a claim.
//!
//! WHY THIS EXISTS. The `gate` step is trustworthy because a machine
//! writes its receipt and `boss park` copies it verbatim; nobody types
//! a verdict. The `proven` step had no such thing. Its one required
//! field, `verified`, is free prose, so "proven in prod" meant only
//! that somebody wrote a sentence saying so.
//!
//! That is not a hypothetical weakness. On 2026-08-28 a change was
//! reported done on the strength of an HTTP 204 — the API accepted a
//! write, so the write was called proof — and the behaviour it claimed
//! had never been observed. Twice. David's question was the right one:
//! how is there evidence a fix is in prod before coming back to me?
//! The answer was that there wasn't, because nothing required any.
//!
//! WORSE, THE HAND-ROLLED CHECKS WERE THEMSELVES WRONG. The same day, a
//! verification loop used `grep -c ... || echo 0`; on no match that
//! prints `0` from grep AND `0` from the fallback, and the resulting
//! "0\n0" compared unequal to "0", so the loop reported success for a
//! file that had not changed. A check written fresh per change is a
//! second thing that can be broken, and it is broken silently, in the
//! direction of saying yes.
//!
//! SO THE VERB RUNS THE PROBE ITSELF. It does not accept output pasted
//! in; it executes the command, captures exit status and both streams
//! verbatim, and REFUSES to record anything unless the probe exits zero
//! and — unless the caller explicitly downgrades to `--exit-only` —
//! unless the expected string is actually present. What lands on the
//! step is what happened, not what was hoped.
//!
//! A NUMERIC EXPECTATION IS COMPARED AS A WHOLE TOKEN. `--expect "1"`
//! once accepted a probe that printed `10`, because the comparison was
//! a plain substring test (421b3032): the claim was true and the proof
//! checked nothing, which is the one failure this verb exists to
//! prevent. Numbers now have to stand alone, and recording a bare
//! number warns and names the shape that cannot lie — assert inside the
//! probe and print a unique token.
//!
//! AND THE PROOF STAYS RE-RUNNABLE. The command is recorded alongside
//! its output, so `boss prove <car> --recheck` re-executes it later and
//! says whether the claim still holds. A proof that has silently
//! decayed — a ConfigMap preview reverted by the next converge, exactly
//! the failure a hand-pushed step-plugin bundle used to warn about — becomes findable
//! instead of being a sentence in a closed packet that nobody rereads.
//!
//! WHAT IT DELIBERATELY DOES NOT DO. It does not judge whether the
//! probe is a good probe. `--probe 'true' --exit-only` will pass, and
//! it will pass legibly: the recorded proof shows a caller who asserted
//! nothing, which is a thing a reader can see and challenge. The prose
//! in `verified` stays human, because what a change MEANS is judgement.
//! Only the evidence under it is mechanised.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use boss_jobs::probe::{CAR_CONVERGED_AT_VAR, CAR_MERGE_REF_VAR};
use serde_json::{Value, json};

/// The step this verb fills. Job step titles come from the registry, so
/// a rename there is a rename here — pinned the same way `boss park`
/// pins its three: refuse rather than guess which step was meant.
const PROVEN: &str = "Proven in prod";

/// Streams are recorded verbatim up to this much. Long enough for a
/// real probe's output, short enough that a runaway `find /` does not
/// push a megabyte into job metadata.
const MAX_STREAM: usize = 4000;

fn clip(s: &str) -> String {
    let s = s.trim_end();
    if s.len() <= MAX_STREAM {
        return s.to_string();
    }
    let cut = s
        .char_indices()
        .map(|(i, _)| i)
        .nth(MAX_STREAM)
        .unwrap_or(s.len());
    format!("{}\n… [{} more bytes]", &s[..cut], s.len() - cut)
}

/// What running a probe produced. Captured, never typed.
#[derive(Debug, Clone)]
pub(crate) struct Outcome {
    pub exit: i32,
    pub stdout: String,
    pub stderr: String,
    /// The commands bash could not resolve while the probe ran — the
    /// forge's fd-9 finding, now this door's too (46f67333). Empty means
    /// the probe RAN; anything here means it did not, whatever the exit
    /// and the streams say. Sorted and deduplicated, as the runner's
    /// `sort -u` leaves it.
    pub missing_tools: Vec<String>,
}

/// Run `probe` through a shell and capture everything it did.
pub(crate) fn execute(probe: &str) -> Result<Outcome> {
    execute_with(probe, &Shell::here(None))
}

/// THE UNRUNNABLE PRELUDE (backlog 46f67333). Every probe's shell gets
/// this ahead of the probe's own text: fd 9 opened on a not-found
/// channel before the probe can redirect anything, and a
/// `command_not_found_handle` that writes every unresolved command to
/// it — so a probe that pipes its own stderr into a `grep -q` still
/// cannot hide which tool was missing (f9304366). The handler ALSO
/// prints bash's usual message, so nothing is taken away; if fd 9
/// cannot be opened the probe still runs, with an empty channel.
///
/// ONE DEFINITION, ONE DOOR. Until backlog 9f00a805 (car 2, 2026-09-18)
/// this text lived in the forge's shell twin, infra/forge/run-car-
/// probe.sh, between markers this crate `include_str!`d and lifted at
/// build time, and the hand door ran the forge's prelude so the two
/// could not disagree. The twin is retired — the arrival rule's
/// ops-request runs `boss prove <car> --from-car --unattended` on the
/// forge — so the prelude, the judge, the verdict sentences and the
/// attempt record are written here once and nowhere else.
pub(crate) const PRELUDE: &str = r#"
{ exec 9>>"${BOSS_PROBE_NOTFOUND:-/dev/null}"; } 2>/dev/null || exec 9>/dev/null
command_not_found_handle() {
    printf "%s: command not found\n" "$1" >&2
    printf "%s\n" "$1" >&9
    return 127
}
"#;

/// The channel, read as the forge's runner read it: `sort -u`, blank
/// lines dropped. Pure, so the record's `missing_tools` is testable
/// without a shell.
pub(crate) fn missing_tools(channel: &str) -> Vec<String> {
    let mut tools: Vec<String> = channel
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    tools.sort_unstable();
    tools.dedup();
    tools
}

/// The seconds `timeout` waits after its TERM before the KILL — the
/// forge runner's `timeout -k 5`, kept.
const KILL_AFTER_SECS: u64 = 5;

/// WHERE, AS WHOM AND UNDER WHAT A PROBE'S SHELL RUNS. The hand door
/// ([`Shell::here`]) is the operator's own shell: this user, this
/// directory (or the recorded one under `--recheck`), no timeout, the
/// environment as it is. The unattended door ([`Shell::unattended`]) is
/// what the forge's shell twin did and what the ops-request now asks
/// `boss prove --unattended` to do: drop from root to the probe user,
/// run in the converged checkout, under a timeout, with exactly the
/// environment a recorded probe is promised — the system of record's
/// address, the read-only reader's identity and port table, the car's
/// own converged instant ([`Shell::with_car_instant`]), and
/// `probe-bin` first on PATH — and WITHOUT the actor this verb writes
/// as, which is a write credential handed to program text a builder
/// wrote if it leaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shell {
    pub cwd: Option<std::path::PathBuf>,
    /// The user to run as, when this process is root — the forge's
    /// `runuser -u david`. A process that is not root runs the probe
    /// as itself, the twin's by-hand path: a person who is already the
    /// probe user.
    pub user: Option<String>,
    /// Seconds before the probe is killed; `None` at the hand door.
    pub timeout_secs: Option<u64>,
    /// Set on the probe's environment, after `strip`.
    pub env: Vec<(String, String)>,
    /// Removed from the probe's environment.
    pub strip: Vec<String>,
    /// Prepended to the probe's PATH, colon-separated.
    pub path_prefix: Option<std::path::PathBuf>,
}

/// THE CAR'S OWN CONVERGENCE INSTANT, read off the car: the `merge_ref`
/// the conductor wrote when its train landed. `None` when the car has
/// not merged, or when what it carries is not an object name — which is
/// also the guard on what this verb hands to `git`, since a car's
/// metadata is data from the system of record, not a literal here.
pub(crate) fn car_merge_ref(car: &Value) -> Option<&str> {
    car.pointer("/metadata/merge_ref")
        .and_then(Value::as_str)
        .and_then(resolvable_merge_ref)
}

/// An abbreviated or full object name, and nothing else.
fn resolvable_merge_ref(raw: &str) -> Option<&str> {
    let r = raw.trim();
    ((7..=40).contains(&r.len()) && r.chars().all(|c| c.is_ascii_hexdigit())).then_some(r)
}

/// The commit time of `merge_ref` in the checkout at `dir`, in epoch
/// seconds — an epoch because that is the only form that compares
/// honestly against the system of record's UTC timestamps
/// (`boss_jobs::probe::GIT_TIME_STRING_EVIDENCE`). `None` when the
/// merge is not in this checkout: the car's change has not converged
/// here, and a probe that gets no instant says NOT YET rather than
/// inventing one.
fn converged_at(dir: &Path, merge_ref: &str) -> Option<String> {
    let o = std::process::Command::new("git")
        .args([
            // The unattended door runs as root in the probe user's
            // checkout, and git refuses a repository owned by somebody
            // else ("dubious ownership") by answering nonzero rather
            // than erroring loudly — which here would silently hand
            // over NO instant and starve the probe the other way. This
            // one command is a read; say so rather than find out.
            "-c",
            &format!("safe.directory={}", dir.display()),
            "-C",
            &dir.display().to_string(),
            "show",
            "-s",
            "--format=%ct",
            merge_ref,
            "--",
        ])
        .output()
        .ok()?;
    let ct = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !ct.is_empty() && ct.chars().all(|c| c.is_ascii_digit())).then_some(ct)
}

impl Shell {
    /// THE FIXED CUTOFF A RECORDED PROBE COMPARES AGAINST (backlog
    /// a92571a6, measured 2026-09-19). A probe asking "did my
    /// qualifying event happen after my change landed?" had nothing
    /// fixed to ask it of, so the idiom that grew was `git log -1
    /// --format=%ct HEAD` — the converged checkout's CURRENT head,
    /// which advances with every train. The car's change converged
    /// ONCE; that instant is what the question means, it is derivable
    /// from the `merge_ref` already on the car, and handing it over is
    /// what makes the correct comparison available rather than merely
    /// documented.
    ///
    /// Both names are STRIPPED first, so a value left in the runner's
    /// environment can never stand in for one this door resolved.
    pub(crate) fn with_car_instant(mut self, merge_ref: Option<&str>) -> Self {
        self.strip.push(CAR_MERGE_REF_VAR.into());
        self.strip.push(CAR_CONVERGED_AT_VAR.into());
        let Some(r) = merge_ref.and_then(resolvable_merge_ref) else {
            return self;
        };
        self.env.push((CAR_MERGE_REF_VAR.into(), r.to_string()));
        let dir = self.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
        if let Some(at) = converged_at(&dir, r) {
            self.env.push((CAR_CONVERGED_AT_VAR.into(), at));
        }
        self
    }

    /// The hand door's shell: as the operator, in `cwd` when given.
    pub(crate) fn here(cwd: Option<&Path>) -> Self {
        Self {
            cwd: cwd.map(Path::to_path_buf),
            user: None,
            timeout_secs: None,
            env: Vec::new(),
            strip: Vec::new(),
            path_prefix: None,
        }
    }

    /// The argv the shell runs — `timeout`, `runuser` and `bash -c` in
    /// the forge runner's order — with the prelude ahead of the probe's
    /// text. Pure, so the test pins the words rather than a run.
    pub(crate) fn command_line(&self, probe: &str, as_root: bool) -> Vec<String> {
        let mut argv = Vec::new();
        if let Some(t) = self.timeout_secs {
            argv.extend(
                [
                    "timeout",
                    "-k",
                    &KILL_AFTER_SECS.to_string(),
                    &t.to_string(),
                ]
                .map(str::to_string),
            );
        }
        if let (Some(u), true) = (&self.user, as_root) {
            argv.extend(["runuser", "-u", u, "--"].map(str::to_string));
        }
        argv.extend(["bash", "-c", &format!("{PRELUDE}\n{probe}")].map(str::to_string));
        argv
    }
}

/// Is this process root? Asked of `id -u`, the way the forge runner
/// asked it, so no libc binding rides in for one question.
fn running_as_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}

/// Run `probe` in `shell` and capture everything it did.
///
/// `bash`, not `sh`, because the prelude's handler is a bash facility
/// and because the forge ran the same text under `bash -c` — a probe
/// means one thing at both doors. On a bash too old for the handler
/// (macOS's 3.2) the channel stays empty and the verdict falls back to
/// what it was: `command not found` on stderr, read as a red.
pub(crate) fn execute_with(probe: &str, shell: &Shell) -> Result<Outcome> {
    let as_root = shell.user.is_some() && running_as_root();
    // The channel: a file of this process's own, named so two operators
    // (or two rechecks) on one box never share it — the uid and pid by
    // `own_temp_path` (307df975), the clock within one process. It is
    // created empty and removed after the read; the prelude opens it
    // for append. As
    // on the forge, the channel must not be able to take the probe down
    // with it: a temp dir that refuses the file leaves the env unset,
    // the prelude falls back to /dev/null, and the probe still runs —
    // with an empty channel, which reads as today's verdict. When the
    // probe runs as another user the file is opened to everyone (the
    // forge's `chmod 666`): it holds command names and nothing else,
    // and a channel the probe cannot append to is a channel that never
    // names the tool.
    let channel = crate::own_temp::own_temp_path(&format!(
        "boss-prove-notfound-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let channel = std::fs::write(&channel, "").is_ok().then_some(channel);
    #[cfg(unix)]
    if let (Some(c), true) = (&channel, as_root) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(c, std::fs::Permissions::from_mode(0o666));
    }
    let argv = shell.command_line(probe, as_root);
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if let Some(dir) = &shell.cwd {
        cmd.current_dir(dir);
    }
    for name in &shell.strip {
        cmd.env_remove(name);
    }
    for (k, v) in &shell.env {
        cmd.env(k, v);
    }
    if let Some(prefix) = &shell.path_prefix {
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{path}", prefix.display()));
    }
    if let Some(c) = &channel {
        cmd.env("BOSS_PROBE_NOTFOUND", c);
    }
    let out = cmd
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| anyhow::anyhow!("could not run the probe ({}): {e}", argv[0]));
    let caught = channel
        .as_deref()
        .map(|c| {
            let caught = std::fs::read_to_string(c).unwrap_or_default();
            let _ = std::fs::remove_file(c);
            caught
        })
        .unwrap_or_default();
    let out = out?;
    // A signalled probe reports no code; -1 is recorded rather than
    // silently becoming 0, because "killed" must not read as "passed".
    let exit = out.status.code().unwrap_or(-1);
    let mut stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    // `timeout` exits 124 for a probe it had to stop; the record says
    // so where the probe's own last words are, as the forge's did.
    if let (Some(t), TIMEOUT_EXIT) = (shell.timeout_secs, exit) {
        stderr.push_str(&format!("\n[boss prove: killed at {t}s timeout]\n"));
    }
    Ok(Outcome {
        exit,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr,
        missing_tools: missing_tools(&caught),
    })
}

/// What `timeout(1)` exits when the command it ran did not finish.
const TIMEOUT_EXIT: i32 = 124;

/// Is this expectation a bare number?
///
/// `inf` and `nan` parse as floats and are not what anyone means by a
/// numeric expectation, so a digit is required as well.
pub(crate) fn is_bare_number(want: &str) -> bool {
    want.chars().any(|c| c.is_ascii_digit()) && want.parse::<f64>().is_ok()
}

/// Could this character be part of the same number token?
fn joins_a_token(c: char) -> bool {
    c.is_alphanumeric() || c == '.' || c == '_'
}

/// Did the probe print what was expected?
///
/// A token expectation is a substring test, which is what every good
/// probe relies on (`claim:ok` inside a longer line). A NUMERIC one is
/// a whole-token test, because a substring test accepted `10` as proof
/// of `1` (421b3032) — the claim was true and the proof checked
/// nothing. One definition, used by the record path and by `--recheck`.
pub(crate) fn observed(printed: &str, want: &str) -> bool {
    if !is_bare_number(want) {
        return printed.contains(want);
    }
    printed.match_indices(want).any(|(i, _)| {
        let before = printed[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !joins_a_token(c));
        let after = printed[i + want.len()..]
            .chars()
            .next()
            .is_none_or(|c| !joins_a_token(c));
        before && after
    })
}

/// A bare number is a weak expectation even when it matches, so say so
/// at record time and name the shape that cannot lie. Returns the
/// warning text, or `None` when the expectation is already a token.
pub(crate) fn bare_number_warning(want: &str) -> Option<String> {
    is_bare_number(want).then(|| {
        format!(
            "boss prove: WEAK EXPECTATION — --expect {want:?} is a bare number, so the proof \
             rests on the probe's output being read correctly by a human later.\n  \
             It is compared as a whole token (a substring test would accept 10 as proof of 1), \
             but a number still says nothing about WHICH number was meant.\n  \
             The shape that cannot lie asserts inside the probe and prints a unique token:\n    \
             $ test $({want} …) -eq {want} && echo claim:ok    --expect 'claim:ok'"
        )
    })
}

/// Decide whether an outcome is evidence, or refuse and say why.
///
/// This is the whole gate, and it is deliberately only two rules: the
/// probe must have exited zero, and — unless the caller downgraded to
/// `exit_only` — the expected string must actually appear in what it
/// printed (see `observed` for what "appear" means). Both refusals
/// quote the streams, because a refusal that hides the output makes
/// the reader go hunting for it.
pub(crate) fn judge(o: &Outcome, expect: Option<&str>) -> Result<()> {
    if o.exit != 0 {
        bail!(
            "the probe exited {}, so it is not proof of anything.\n\
             \n  stdout: {}\n  stderr: {}\n\n\
             A probe that fails is evidence AGAINST the claim. Fix the change, \
             or fix the probe if the probe is what is wrong — but nothing is \
             recorded either way.",
            o.exit,
            if o.stdout.trim().is_empty() {
                "(empty)"
            } else {
                o.stdout.trim()
            },
            if o.stderr.trim().is_empty() {
                "(empty)"
            } else {
                o.stderr.trim()
            },
        );
    }
    if let Some(want) = expect {
        // Both streams count: plenty of real probes report on stderr.
        if !observed(&o.stdout, want) && !observed(&o.stderr, want) {
            bail!(
                "the probe exited 0 but never printed {want:?}, so it did not \
                 observe what was claimed.{}\n\
                 \n  stdout: {}\n  stderr: {}\n\n\
                 An exit code alone is a weak assertion — `echo hi` exits 0 too. \
                 Either the change is not in prod, or the probe is looking in the \
                 wrong place.",
                if is_bare_number(want) {
                    format!(
                        "\n  {want:?} is a number, so it is compared as a whole token: a \
                         substring test would accept 10 as proof of 1, which is how a proof \
                         passes for the wrong reason (421b3032). If the number IS in the \
                         output as part of a longer one, assert it inside the probe instead: \
                         `test $(…) -eq {want} && echo claim:ok`."
                    )
                } else {
                    String::new()
                },
                if o.stdout.trim().is_empty() {
                    "(empty)"
                } else {
                    o.stdout.trim()
                },
                if o.stderr.trim().is_empty() {
                    "(empty)"
                } else {
                    o.stderr.trim()
                },
            );
        }
    }
    Ok(())
}

/// WHAT FAILED, NOT WHAT MIGHT HAVE — the verdict half of 4fccc595.
///
/// `judge` says the probe exited nonzero and quotes the streams. That
/// was not enough twice over on car a0ab90a5: the recorded verdict
/// offered two possibilities ("the claim is not holding, or the probe is
/// wrong"), and the evidence under it was an empty string and a
/// rewritten exit code. A reader had to re-derive what the system
/// already held, which is the defect class, not the incident.
///
/// So this names ONE thing when the record supports naming one, and
/// nothing when it does not. Order matters: the self-contradictory shape
/// is checked FIRST because a probe carrying it never had a verdict to
/// give — its nonzero exit says nothing about production either way, and
/// the measured probe carried both findings at once. A crashed numeric
/// test comes next, for the same reason and one more: its exit code is
/// whatever the `||` branch behind it chose — 75 on both measured cars
/// — so the code is not just uninformative, it points the wrong way.
pub(crate) fn failure_diagnosis(probe: &str, o: &Outcome) -> Option<String> {
    if o.exit == 0 {
        return None;
    }
    if let Some(evidence) = quoting_was_mangled(probe, o) {
        return Some(format!(
            "THE PROBE'S QUOTING WAS MANGLED, so exit {exit} is not a verdict on the claim \
             — the shell never ran the probe that was written. {evidence}\n  \
             A stored probe carrying `\\\"` was escaped on the way in: the backslash-quotes \
             reach the tool as literal characters, so the words of a pattern become \
             filenames and the guard arm meant for \"nothing to judge yet\" catches the \
             wreckage and exits {exit}. Re-store the probe with SINGLE quotes, or pass it \
             through `--park-probe-file`, where no word expansion happens at all. Then \
             re-run it: this says nothing about whether the change is in production \
             (302bc2f2 — two cars sat unprovable for days this way, hourly rechecked, \
             while both claims were already true on main).",
            exit = o.exit,
        ));
    }
    if boss_jobs::probe::asserts_its_own_negation(probe) {
        let rewritten = match boss_jobs::probe::rewrites_its_exit_status(probe) {
            Some(n) => format!(
                "\n  AND THE NUMBER THAT WOULD PROVE IT IS GONE: a bare `|| exit {n}` replaced \
                 jq's own status with {n}, so 4 (produced nothing — the claim HELD) and 5 \
                 (called `error` — the claim FAILED) read identically. Keep it: \
                 `|| {{ echo \"<what failed> (exit $?)\"; exit 1; }}`."
            ),
            None => String::new(),
        };
        return Some(format!(
            "THE PROBE IS SELF-CONTRADICTORY, so this exit code is not a verdict on the \
             claim. It runs `jq -e` over a filter whose success branch is `empty`.\
             \n  {evidence}{rewritten}\n  \
             Nothing here says whether the change is in production. Fix the probe and \
             re-run — and if the car is parked, re-park it so the arrival rule does not \
             run this text again.",
            evidence = boss_jobs::probe::SELF_CONTRADICTORY_EVIDENCE,
        ));
    }
    if let Some(line) = crashed_comparing_a_non_number(&o.stderr) {
        return Some(format!(
            "THE PROBE CRASHED comparing a non-number (jq printed null?), so exit {exit} is \
             not a verdict on the claim — it is whichever `||` branch caught the crash. \
             bash's `[` said:\n  {line}\n  \
             Guard the value before the numeric test — `// empty` in the jq filter, or \
             `case \"$n\" in ''|*[!0-9]*) echo \"not yet: no number\"; exit 75;; esac` — so a \
             missing value says not-yet BY NAME instead of crashing into the not-yet branch. \
             Fix the probe and re-park; a recheck re-runs a crash forever (68081368).",
            exit = o.exit,
        ));
    }
    if let Some(line) = quoting_mangled_its_arguments(&o.stderr) {
        return Some(format!(
            "THE PROBE'S QUOTING WAS MANGLED, so exit {exit} is not a verdict on the claim \
             — a tool was handed a filename with a quote in it, which means the argument \
             list was split somewhere the author did not intend. The tool said:\n  {line}\n  \
             This is the backtick trap's cousin: prose damaged between authoring and \
             storage. SINGLE-quote the probe when you pass it, or use the flag's `-file` \
             twin, where no word expansion happens at all. Fix the probe and re-park — a \
             recheck re-runs damaged text forever, and every run reports the probe's own \
             not-yet sentence, which reads exactly like an honest wait (302bc2f2).",
            exit = o.exit,
        ));
    }
    if o.stdout.trim().is_empty() && o.stderr.trim().is_empty() {
        let rewritten = match boss_jobs::probe::rewrites_its_exit_status(probe) {
            Some(n) => format!(
                " A bare `|| exit {n}` in the probe is the usual cause: it replaces the \
                 failing command's status with {n} and prints nothing, so the one number \
                 that named the cause is gone."
            ),
            None => String::new(),
        };
        return Some(format!(
            "THE FAILURE CANNOT BE READ: the probe exited {exit} and printed NOTHING on \
             either stream, so this is not a verdict on the claim — it is a missing \
             record.{rewritten}\n  \
             Make each outcome speak: print a named token on success, and on failure echo \
             what failed with the status that caused it before exiting. A probe whose \
             failure is silent costs its next reader the whole diagnosis (4fccc595: 18 \
             hours, on a probe that could not pass).",
            exit = o.exit,
        ));
    }
    None
}

/// WHAT A TOOL SAYS WHEN THE PROBE'S OWN QUOTING REACHED IT LITERALLY
/// (backlog 302bc2f2). A probe stored with backslash-quotes —
/// `grep -c \"boss step complete\"` — hands grep the pattern `"boss`
/// and then the FILENAMES `step` and `complete"`. grep warns about
/// each, the substitution comes back non-numeric, and the probe's own
/// guard arm catches it and exits 75. The record then says "not yet"
/// in the probe's own words, which is the one answer nobody re-reads.
///
/// Measured 2026-09-20: two shed cars sat unprovable for days this
/// way, hourly rechecked, while BOTH claims were already true on main.
///
/// THE SIGNAL IS DECIDABLE AND THE OBVIOUS ONE IS NOT. "No such file
/// or directory" alone is a false positive — a probe that greps a file
/// production has not written yet is an HONEST not-yet, and several in
/// the shed are exactly that. Two signals distinguish them, and either
/// is enough:
///
/// 1. The probe text carries a backslash-quote AND a tool reported a
///    lookup failure. A correctly quoted probe cannot produce both.
/// 2. The probe's own message arrives WRAPPED IN QUOTE CHARACTERS. A
///    working `echo 'not yet: …'` never emits them; seeing them means
///    the echo's quotes were literal, so every other quote in that
///    probe was too.
const LOOKUP_FAILURE_MARKERS: [&str; 2] = ["No such file or directory", "cannot open"];

/// A literal backslash followed by a quote, as it appears in a stored
/// probe whose quoting was escaped on the way in.
const ESCAPED_QUOTE: &str = "\\\"";

/// Did the shell read this probe as something other than what it says?
/// `None` when the probe ran as written — including every honest
/// not-yet. See [`LOOKUP_FAILURE_MARKERS`] for why the two signals are
/// what they are.
pub(crate) fn quoting_was_mangled(probe: &str, o: &Outcome) -> Option<String> {
    let lookup_failed = o
        .stderr
        .lines()
        .chain(o.stdout.lines())
        .find(|l| LOOKUP_FAILURE_MARKERS.iter().any(|m| l.contains(m)))
        .map(str::trim);
    let said_in_quotes = what_it_said(o).filter(|l| l.starts_with('"'));

    let evidence = match (lookup_failed, &said_in_quotes) {
        (Some(line), _) if probe.contains(ESCAPED_QUOTE) => {
            format!("a tool could not find what it was handed:\n  {line}")
        }
        (_, Some(line)) => format!(
            "the probe's own message arrived wrapped in quote characters, which a working \
             `echo` never emits:\n  {line}"
        ),
        _ => return None,
    };
    Some(evidence)
}

/// WHAT BASH SAYS WHEN `[` OR `((` IS HANDED A NON-NUMBER (backlog
/// 68081368). Cars dd1d872d and 2e4d3bce each recorded the not-yet
/// sentence over a stderr of `bash: line 10: [: null: integer expression
/// expected`: jq printed null, `[ "$n" -ge 1 ]` exited 2, and the `||`
/// branch meant for "no such packet yet" exited 75. Nothing was judged.
///
/// Until backlog 9f00a805 (car 2) the forge's shell twin carried this
/// list a second time, as a grep pattern, pinned equal by a test; the
/// twin is retired and this is the one copy.
pub(crate) const NUMERIC_CRASH_MARKERS: [&str; 3] = [
    "integer expression expected",
    "unary operator expected",
    "syntax error: invalid arithmetic operator",
];

/// The first stderr line that carries one of [`NUMERIC_CRASH_MARKERS`],
/// trimmed — the line the verdict quotes. `None` when the probe crashed
/// on nothing of the kind, so the other diagnoses get their turn.
pub(crate) fn crashed_comparing_a_non_number(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| NUMERIC_CRASH_MARKERS.iter().any(|m| line.contains(m)))
}

/// WHAT A TOOL SAYS WHEN THE PROBE'S QUOTING WAS MANGLED (backlog
/// 302bc2f2). Car c4c1ac17's stored probe carried backslash-escaped
/// quotes, so `grep -c \"boss step complete\"` ran with `\"boss` as the
/// pattern and `step` and `complete"` as FILENAMES. grep warned about
/// each, exited 2, and the probe's own `case` arm turned that into exit
/// 75 — the one answer nobody re-reads. The claim was already true on
/// main; the car sat unproven for days over its own text, and the
/// hourly recheck re-ran it forever, each run printing the same
/// reassuring sentence.
///
/// THE RULE IS NARROW ON PURPOSE, and the narrowness is the whole
/// design. "No such file or directory" ALONE is not evidence of damage:
/// a probe that greps a file which has not landed on main yet says
/// exactly that, and it is an honest not-yet. What is never honest is a
/// missing operand whose NAME CARRIES A DOUBLE QUOTE — no probe reads a
/// file called `complete"`, so the quote is the residue of an extra
/// escaping layer between authoring and storage.
///
/// WHY NOT REFUSE THE TEXT AT THE DOOR INSTEAD. That was the first
/// proposal and it is wrong, measured: of 254 stored probes, 71 carry a
/// backslash-quote and nearly all are CORRECT — `grep -q "name =
/// \"folded_into\""` is how sh writes a literal quote inside a quoted
/// string. Restricting to a backslash-quote in unquoted context still
/// flags two correct probes, both `case` patterns where `*\"x\"*` is the
/// idiomatic way to match a literal quote. The text cannot tell the
/// damage from the idiom; the RUN can, because only the damaged one
/// makes a tool report a filename with a quote in it.
///
/// It fails toward NotProven with the cause attached, never toward a
/// green, so the worst a false positive does is make a car look
/// troubled — which is the direction CLAUDE.md asks this to fail.
pub(crate) const MISSING_OPERAND_MARKERS: [&str; 3] =
    ["No such file or directory", "cannot open", "can't open"];

/// The first stderr line that names a missing operand whose name
/// carries a double quote — the line the verdict quotes. `None` when
/// nothing on stderr shows that shape, so an honestly-missing file
/// keeps its not-yet.
pub(crate) fn quoting_mangled_its_arguments(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| line.contains('"') && MISSING_OPERAND_MARKERS.iter().any(|m| line.contains(m)))
}

/// [`judge`], with the diagnosis attached when the record supports one.
/// Every path that runs a probe goes through here rather than `judge`,
/// so a refusal the operator reads and a refusal `--recheck` records
/// cannot say different things.
pub(crate) fn judge_probe(probe: &str, o: &Outcome, expect: Option<&str>) -> Result<()> {
    judge(o, expect).map_err(|e| match failure_diagnosis(probe, o) {
        Some(d) => anyhow::anyhow!("{e}\n\n{d}"),
        None => e,
    })
}

/// THE EXIT CODE THAT MEANS "NOT YET" — the probe's, and now this
/// verb's. 75 is EX_TEMPFAIL, the code a recorded probe exits with
/// when the world is not ready to judge the claim ("not yet: no
/// disk-report request carrying for_sweep yet"). This verb exits the
/// same number for the same reason — under `--unattended` that IS the
/// ops-request's `exit_code`, the verdict a reader of the packet sees
/// first — so a script wrapping either door tells the three answers
/// apart the same way.
pub(crate) const NOT_YET_EXIT: i32 = 75;

/// THE EXIT CODE THAT MEANS "COULD NOT RUN HERE" — the third code
/// (46f67333). 3 when fd 9 caught a tool, 75 when the probe ran and
/// said not yet, 1 when it ran and did not prove: three different
/// things to do about it, so three numbers for the ops-request to
/// carry. The forge's shell twin exited the same three until it was
/// retired (9f00a805 car 2); `the_unattended_door_exits_one_of_three_codes`
/// pins them here.
pub(crate) const UNRUNNABLE_EXIT: i32 = 3;

/// THE EXIT CODE THAT MEANS "REFUSED" at the unattended door — the
/// forge runner's `refuse`, kept: the car is not a merged ship-a-change
/// car with a recorded probe and an open `proven` step, so nothing ran
/// and nothing was judged. Distinct from 1 (ran, not proven) so a
/// reader of the ops-request does not go looking for a probe run that
/// never happened.
pub(crate) const REFUSED_EXIT: i32 = 2;

/// THE THREE-WAY READING (backlog 726562de). Two doors run a car's
/// probe, and until 2026-09-14 they read exit 75 in opposite
/// directions. The forge runner learned the not-yet protocol on
/// 2026-09-13 (feat/a-probe-can-say-not-yet): a clean exit 75 is NOT
/// YET — the probe ran, found nothing to judge yet, and said so — so it
/// stamps `proof_attempt{not_yet:true}`, leaves `proven` ready and lets
/// the daily recheck run it again. This door was not taught: `judge`
/// has two rules, and exit 75 broke the first, so `boss prove <car>
/// --from-car` on the SAME run printed "A probe that fails is evidence
/// AGAINST the claim". One record, two verdicts, depending on which
/// door happened to run it — found by the pin that could not make the
/// wording equal because one side had no wording at all.
///
/// So the verdict is an enum with three arms rather than a `Result`
/// with two, and every path that runs a probe reads it here. Not-yet
/// is what a probe SAYS, not merely a number it exits with: it takes
/// exit 75 AND no [`failure_diagnosis`] — the same order the forge's
/// verdict block uses, where a crashed numeric test (68081368) and a
/// silent exit (4fccc595) are checked before the code is read, because
/// in both the 75 is whichever `||` branch caught a probe that never
/// judged anything.
///
/// UNRUNNABLE IS THE FOURTH ARM, and the first one read after the two
/// rules (46f67333). The forge's verdict block checks fd 9's channel
/// before anything else — a probe that never resolved its tool has no
/// exit code worth reading and no streams worth diagnosing — and, like
/// the forge, this door checks it only on a run the two rules refused:
/// a probe that lost a tool on a `||` branch and still printed its token
/// proved the claim at both doors.
#[derive(Debug)]
pub(crate) enum Verdict {
    /// Exit 0 and the expectation printed — [`judge`]'s two rules.
    Proven,
    /// Exit 75 with something to say and nothing to diagnose. `said` is
    /// the probe's own not-yet line ([`what_it_said`]).
    NotYet { said: String },
    /// The not-found channel named a tool: the probe DID NOT RUN, so
    /// nothing was judged. `missing` is [`Outcome::missing_tools`].
    Unrunnable { missing: Vec<String> },
    /// Everything else: [`judge_probe`]'s refusal, diagnosis attached.
    NotProven(anyhow::Error),
}

/// Read one run four ways. See [`Verdict`].
pub(crate) fn verdict(probe: &str, o: &Outcome, expect: Option<&str>) -> Verdict {
    let refused = match judge_probe(probe, o, expect) {
        Ok(()) => return Verdict::Proven,
        Err(e) => e,
    };
    if !o.missing_tools.is_empty() {
        return Verdict::Unrunnable {
            missing: o.missing_tools.clone(),
        };
    }
    if o.exit == NOT_YET_EXIT
        && failure_diagnosis(probe, o).is_none()
        && let Some(said) = what_it_said(o)
    {
        return Verdict::NotYet { said };
    }
    Verdict::NotProven(refused)
}

/// WHAT THE PROBE SAID, in one line: the first non-empty line of
/// stderr, else of stdout, trimmed and cut as the forge cuts it (300
/// characters). stderr first because that is where a tool puts its
/// diagnosis — jq's `error(…)`, curl's message, bash's command-not-
/// found. One definition, so the reason clause both doors quote is the
/// same line.
pub(crate) fn what_it_said(o: &Outcome) -> Option<String> {
    o.stderr
        .lines()
        .chain(o.stdout.lines())
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(300).collect())
}

/// THE NOT-YET SENTENCE — `proof_attempt.why` when the probe said not
/// yet, and what the operator reads. The lead word, the reason clause,
/// and the disclaimer that this is not a verdict against the change
/// are load-bearing text a reader of the car acts on; both doors
/// compose it here, so it cannot depend on which door ran the probe.
pub(crate) fn not_yet_why(host: &str, said: &str) -> String {
    format!(
        "NOT YET: the probe ran on {host} and said the claim cannot be judged until \
         something happens — {said}. Not a verdict against the change; \
         recheck-failing-probes-hourly runs it again."
    )
}

/// THE DID-NOT-RUN SENTENCE — `proof_attempt.why` when the channel
/// named a tool, and what the operator reads. The lead word, the `on
/// <host>: <tools> not found` clause a reader acts on, and the
/// disclaimer that this says nothing about the change are the same at
/// both doors. The middle sentence is the door's own — the unattended
/// door names the forge host's vantage (the probe user and the
/// converged checkout), the hand door names the machine the operator
/// is standing at — because what to do instead differs by door
/// (`boss_jobs::probe`).
pub(crate) fn unrunnable_why(host: &str, missing: &[String]) -> String {
    format!(
        "THE PROBE DID NOT RUN on {host}: {list} not found. It ran HERE, as you, with this \
         machine's PATH, and bash could not resolve the tool, so nothing about the claim was \
         judged — the same finding the unattended door records from its fd-9 channel. This \
         says nothing about whether the change works; re-probe from a vantage this host has, \
         or record the car as event-bound.",
        list = missing.join(", ")
    )
}

/// [`unrunnable_why`] as the unattended door says it: the forge host,
/// the user the probe ran as, the checkout it ran in — and where the
/// tool the builder reached for actually lives (f9304366: two kubectl
/// probes, correct from the pod, impossible on the forge).
pub(crate) fn unrunnable_why_unattended(
    host: &str,
    missing: &[String],
    user: &str,
    dir: &Path,
) -> String {
    format!(
        "THE PROBE DID NOT RUN on {host}: {list} not found. A recorded probe runs on the \
         forge host as {user} in {dir}, with this host's tools — not on the dev pod where it \
         was written, which is where cluster tools like kubectl live. This says nothing about \
         whether the change works; re-probe from a vantage this host has, or record the car \
         as event-bound.",
        list = missing.join(", "),
        dir = dir.display(),
    )
}

/// THE ATTEMPT RECORD — a run that settled nothing, written on the CAR
/// (`metadata.proof_attempt`, via the merge-PATCH), never on the step,
/// which stays open. One shape from both doors, because `--recheck`,
/// `boss orient` and the yard's shed (`apps/web/src/it/yard/yard.ts`,
/// `proofAttempt`) read it and must not learn which door wrote it;
/// `the_attempt_record_carries_the_keys_the_yard_reads` names the keys.
/// `unrunnable` and `missing_tools` are the not-found channel's
/// finding, from whichever door ran the prelude (46f67333). `expect` is
/// `null` under `--exit-only`, as `proof_json` records it.
///
/// `prior` is the car's CURRENT `proof_attempt`, which this record
/// replaces: a not-yet carries its streak forward from it
/// (`boss_jobs::car::carried_not_yet_streak`, backlog adef5ddf), because
/// the replace is the only moment the previous answer is still in hand.
/// Anything else carries no streak — `null` and `0`, never an absent
/// key, so the shape the yard reads is one shape.
pub(crate) fn attempt_json(
    probe: &str,
    expect: Option<&str>,
    o: &Outcome,
    host: &str,
    at: &str,
    why: &str,
    prior: Option<&Value>,
) -> Value {
    let not_yet = why.starts_with("NOT YET");
    let (since, runs) = if not_yet {
        let (s, n) = boss_jobs::car::carried_not_yet_streak(prior, probe, at);
        (json!(s), n)
    } else {
        (Value::Null, 0)
    };
    json!({
        (boss_jobs::car::NOT_YET_SINCE): since,
        (boss_jobs::car::NOT_YET_RUNS): runs,
        "at": at,
        "exit": o.exit,
        "stdout": clip(&o.stdout),
        "stderr": clip(&o.stderr),
        "host": host,
        "probe": probe,
        "expect": expect,
        "why": why,
        "unrunnable": !o.missing_tools.is_empty(),
        // The flag is the sentence's: a record whose `not_yet` and `why`
        // disagree cannot be written from here.
        "not_yet": not_yet,
        "missing_tools": o.missing_tools,
    })
}

/// WHICH PROBES THIS DOOR WILL RUN (backlog 23b2dffa).
///
/// TWO DOORS RUN A CAR'S RECORDED PROBE AND ONLY ONE CHECKED IT. `boss
/// gate --park-probe` refuses a probe that names a tool the forge host
/// lacks, and one that reads the system of record unidentified. This
/// verb — the HAND door onto the same text, and the one an operator
/// reaches for while moving fast — applied neither, and the only trace
/// of the rule here was a comment describing the gate's refusal. One
/// rule, two enforcement sites, one of which is prose about the other,
/// is CLAUDE.md §9a in its behavioural form, so the predicates are now
/// `boss_jobs::probe` and both doors call them.
///
/// WHY IT REFUSES RATHER THAN WARNS. The operator IS present, and may
/// have a reason the check cannot see — which is an argument for a
/// warning until you notice `--recheck`: it re-runs this recorded probe
/// later, when nobody is watching, so a probe admitted by hand BECOMES
/// an unattended one. A refusal with a stated override keeps both — the
/// operator gets through, and the next reader learns it was overridden
/// and why.
///
/// AND WHY ONLY ONE OF THE TWO RULES REFUSES HERE. They are not the same
/// kind of rule. "Can this text run where it is going" is host-relative:
/// `host-absent-tools.txt` is a measurement of the FORGE, because a
/// `--park-probe` runs there, while this verb runs the probe HERE, in
/// front of the operator, one second later — and a pod-local `kubectl`
/// proof is the established shape for a claim only the cluster can show
/// (memory: proof probes from the pod). Refusing that would block
/// exactly the case where a human must prove what the forge cannot, so a
/// hand `--probe` is not judged against another machine's tools: the
/// question the manifest predicts, this verb measures. "Can its answer
/// be trusted" is host-independent, and refuses everywhere.
///
/// `--from-car` is the one path where both apply, because that text has
/// a second destination: the arrival rule runs it on the forge. There
/// the mismatch is NAMED and not refused — proving it by hand here is
/// the right move; what needs fixing is the probe the car recorded.
///
/// AND WHY THE TWO SHAPE CHECKS ONLY WARN (backlog 4fccc595). They are
/// host-independent like the unidentified read, so they are checked on
/// every probe at every door a person is standing at — but they are said,
/// not refused, and the reason is the DIRECTION OF THE LIE. An
/// unidentified read fails OPEN: the probe passes, the absence assertion
/// is green against a world it was never allowed to see, and a car closes
/// on a proof of nothing. A `jq -e` whose success branch is `empty`, and
/// a bare `|| exit n`, fail CLOSED: `judge` records nothing on a nonzero
/// exit, so the worst they can do is strand a car and misdescribe why —
/// which is exactly what they did for 18 hours, and which a line of text
/// in front of the operator fixes. Both detectors are also deliberately
/// coarse text scans (`boss_jobs::probe` says where they are wrong), and
/// a refusal cannot be spent that cheaply: a false refusal blocks a
/// correct probe, while a false warning costs a line someone ignores.
///
/// The SAME two warnings are said by `boss gate --park-probe`
/// (`ParkIntent::probe_warnings`), because that is the door where the
/// measured probe was admitted and the one moment the shape is cheap to
/// fix. Neither is said at the two UNATTENDED doors — the arrival rule
/// and `--unattended` — because nothing there reads a warning, and a
/// check nobody reads is a check that is not running (CLAUDE.md
/// §Diagnosis). What those two got instead is a VERDICT that names one
/// thing: [`failure_diagnosis`], and the `why` on the attempt record.
pub(crate) const OVERRIDE_FLAG: &str = "--probe-anyway";

pub(crate) use boss_jobs::probe::{GIT_TIME_RULE, UNIDENTIFIED_RULE, override_record};

/// Why a probe must not run: the rule that refused it — recorded when
/// an operator overrides it, so `--recheck` and every later reader find
/// the argument against the rule it was actually made against — and
/// the wording this door says. Two rules refuse here since c0ac92b8;
/// until then the override always recorded [`UNIDENTIFIED_RULE`]
/// because it was the only one.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub rule: &'static str,
    pub text: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// What this door makes of a probe: a reason not to run it, things the
/// operator should know, or neither.
#[derive(Debug, Default)]
pub(crate) struct Admission {
    /// Why the probe must not run, unless the operator overrides it.
    pub refusal: Option<Refusal>,
    /// Things worth saying that do not stop the probe. A list because
    /// they are independent findings and a probe can trip more than one
    /// — the measured 18-hour probe (4fccc595) tripped two, and showing
    /// the operator only the first would have hidden the other.
    pub warnings: Vec<String>,
}

/// Judge a probe at this door. `from_car` says the text came from the
/// car's `proof_probe`, which means the forge will run it too.
pub(crate) fn admit(probe: &str, from_car: bool) -> Admission {
    let unidentified = boss_jobs::probe::reads_the_sor_unidentified(probe).map(|client| {
        format!(
            "THE PROBE READS THE SYSTEM OF RECORD WITH `{client}` AND NO IDENTITY, so it \
             reads as operator:unidentified — and an unidentified reader is answered with a \
             NARROWER WORLD, silently. What this verb would record is a proof of nothing.\
             \n\n{evidence}\n\n\
             Read it as a named reader instead — `boss-api GET /api/...` where that door \
             exists, `{reader} /api/...` on the forge, a curl that sends an identity \
             header, or a gateway session (POST /api/auth/guest, then curl -b). The DOOR \
             has to do the read; `jq` or `python3` fed its stdout is a parser and is not \
             refused (5dc5159d).\n\n\
             If this read IS identified in a way a text check cannot see, say so and it \
             runs: {OVERRIDE_FLAG} '<reason>'. The reason is recorded in the proof, because \
             `--recheck` re-runs this probe later when nobody is watching.",
            evidence = boss_jobs::probe::UNIDENTIFIED_READ_EVIDENCE,
            reader = boss_jobs::probe::SOR_READER,
        )
    });
    // The git-date rule (c0ac92b8) fails open the same way — a string
    // compare of mixed-offset timestamps can pass for an event that
    // never happened — so it refuses at this door as at the gate, with
    // the same evidence and the same rewrite.
    let git_time = boss_jobs::probe::reads_git_time_with_an_offset(probe).map(|token| {
        format!(
            "THE PROBE READS A GIT DATE WITH `{token}`, which carries the committer's UTC \
             offset — and a probe that compares that string against the system of record's \
             UTC timestamps lies in BOTH directions. What this verb could record is a proof \
             of nothing.\n\n{evidence}\n\n\
             This refuses the TOKEN, not the compare: whether the string reaches a \
             `[ ... \\> ... ]` would take a shell parser to know honestly, and the fix is the \
             same either way — `git log -1 --format=%ct` on one side, `date -u -d \"$ts\" +%s` \
             on the other after the empty guard, `-gt` between them. `boss gate --park-probe` \
             refuses this text too, so a car carrying it needs re-parking.\n\n\
             If the date is only printed and never compared, say so and it runs: \
             {OVERRIDE_FLAG} '<reason>'. The reason is recorded in the proof.",
            evidence = boss_jobs::probe::GIT_TIME_STRING_EVIDENCE,
        )
    });
    let refusal = unidentified
        .map(|text| Refusal {
            rule: UNIDENTIFIED_RULE,
            text,
        })
        .or_else(|| {
            git_time.map(|text| Refusal {
                rule: GIT_TIME_RULE,
                text,
            })
        });
    let mut warnings: Vec<String> = Vec::new();
    if from_car && let Some(tool) = boss_jobs::probe::needs_absent_tool(probe) {
        warnings.push(format!(
            "boss prove: NOTE — this car's recorded probe invokes `{tool}`, which the \
             forge host does not have (infra/forge/host-absent-tools.txt). It runs HERE, \
             so proving by hand is fine and is the point; but the arrival rule could not \
             have run it, and any later re-run on the forge will report `unrunnable` \
             rather than a verdict (f9304366). Re-park the car with a probe the forge \
             can run, or record it as --park-proof-event."
        ));
    }
    if from_car && let Some(verb) = boss_jobs::probe::changes_directory(probe) {
        warnings.push(format!(
            "boss prove: NOTE — this car's recorded probe runs `{verb}`, and on the forge \
             the door has already placed it in the converged checkout; a pod path there \
             is `No such file or directory` and an exit 1 on the car (4bb6797c). It runs \
             HERE, so proving by hand is fine; `boss gate --park-probe` refuses this text, \
             so re-park the car with the `{verb}` dropped — `git show HEAD:<path>` reads \
             the converged tree from where the door puts it."
        ));
    }
    if from_car && let Some(var) = boss_jobs::probe::names_an_actor(probe) {
        warnings.push(format!(
            "boss prove: NOTE — this car's recorded probe assigns `{var}`, which would name \
             an actor for the `boss` verbs it runs on the forge, and a probe proves, it \
             does not act (infra/forge/host-absent-tools.txt, the `boss` block). Here it \
             runs as you, so proving by hand is fine; `boss gate --park-probe` refuses this \
             text, so re-park the car with the assignment dropped."
        ));
    }
    warnings.extend(shape_warnings(probe).map(|w| format!("boss prove: {w}")));
    Admission { refusal, warnings }
}

/// THE SIX SHAPE WARNINGS, in the wording every door can use (the
/// prefix is the door's). All read the probe TEXT, like the two rules
/// above them, and all are host-independent — so they are said at every
/// door a human is standing at, `--from-car` or not. The third
/// (0df3af1c) joined the first two for the same reason they exist: `[`
/// exits 2 on a non-integer, so the shape fails CLOSED, and the one
/// place its stderr has a reader is the terminal it is typed at. The
/// fourth (a92571a6) is the same argument again: a cutoff dated from a
/// moving HEAD answers 75 forever, never a false green.
///
/// THE FIFTH AND SIXTH (e7cf78c6) ARE WARNINGS ON A DIFFERENT ARGUMENT,
/// and the difference is worth keeping in sight: a counted page and a
/// grep for a bare name can both record a FALSE GREEN, which is the
/// direction that made `reads_git_time_with_an_offset` a refusal. What
/// keeps them here is decidability, not harmlessness. `limit=` is
/// correct in most of the probes that carry it (16 of 49 measured pair
/// it with `.total` already, and a page that is never counted is fine),
/// and a grep for a mention is a legitimate claim the text cannot be
/// told apart from the defect — so a refusal would fire on correct
/// probes and earn a routine override, which is read by nobody
/// (CLAUDE.md §Diagnosis). Both texts therefore SAY that they can fail
/// open, because a warning is only worth what its reader does with it.
pub(crate) fn shape_warnings(probe: &str) -> impl Iterator<Item = String> {
    let inverted = boss_jobs::probe::asserts_its_own_negation(probe).then(|| {
        format!(
            "THIS PROBE MAY ASSERT ITS OWN NEGATION — it runs `jq -e` over a filter whose \
             success branch is `empty`. `jq -e` exits 4 when the filter produces no output, \
             and `empty` is no output, so the claim HOLDING is what makes jq exit nonzero.\
             \n  {evidence}\n  \
             This is a warning, not a refusal: the shape fails CLOSED, so it can strand a \
             car but never record a proof of nothing. Check it if the filter is doing \
             something else.",
            evidence = boss_jobs::probe::SELF_CONTRADICTORY_EVIDENCE,
        )
    });
    let rewritten = boss_jobs::probe::rewrites_its_exit_status(probe).map(|n| {
        format!(
            "THIS PROBE DISCARDS THE STATUS THAT WOULD EXPLAIN ITS FAILURE — a bare \
             `|| exit {n}` replaces the failing command's exit code with {n} and prints \
             nothing, so jq's 4 (the filter produced nothing) and 5 (the filter called \
             `error`), or curl's 7 (could not connect) and 22 (the server said no), all \
             arrive as {n}. That is the reduction CLAUDE.md §Diagnosis names: it suppresses \
             OUTPUT, not work, and it is paid for by whoever is next in front of the \
             failure.\n  \
             Keep the evidence first: `|| {{ echo \"<what failed> (exit $?)\"; exit 1; }}`."
        )
    });
    let unguarded = boss_jobs::probe::compares_an_unguarded_number(probe).map(|var| {
        format!(
            "THIS PROBE COMPARES `${var}` AS A NUMBER WITHOUT CHECKING THAT IT IS ONE. When \
             the query matches nothing, `jq -r` prints the literal `null` — four characters, \
             not an empty string, so a `${{{var}:-9999}}` default does not fill it — and `[` \
             answers `integer expression expected` on stderr, exits 2, and takes the else \
             branch. Measured 2026-09-14 (0df3af1c): three cars sat UNPROVEN on exactly that, \
             each behind a stderr nobody reads, until an operator rewrote the probe by hand.\
             \n  \
             Guard on either side of the pipe, so a missing number says NOT YET instead:\n    \
             in jq:        `… | first | .field // empty`   (or `first | select(. != null)`)\n    \
             in the shell: `case \"${var}\" in ''|*[!0-9]*) echo 'not yet: <what has not happened>'; exit 75;; esac`\n  \
             This is a warning, not a refusal: `[` fails closed on the string, so the shape \
             strands a car but never records a proof of nothing."
        )
    });
    let moving = boss_jobs::probe::compares_against_a_moving_head(probe).map(|cmd| {
        format!(
            "THIS PROBE DATES ITS CUTOFF FROM A TARGET THAT MOVES — `{cmd}` reads the \
             converged checkout's CURRENT head, which advances with every train (about 28 a \
             day), so the claim quietly becomes 'this change worked more recently than any \
             other change landed'. The car's change converged ONCE; an unrelated train \
             landing afterwards is not evidence against it.\n  {evidence}\n  \
             Date it from the car's own merge, which is fixed and handed to every recorded \
             probe as `${var}` — absent only when the car has not converged here, which is \
             what the guard reports:\n{recipe}\n  \
             This is a warning, not a refusal: the shape fails CLOSED — a starved probe \
             answers 75 (not yet), never a false green — but a car whose qualifying event \
             is rarer than a train starves by construction, and a daily one is effectively \
             unprovable.",
            evidence = boss_jobs::probe::MOVING_HEAD_EVIDENCE,
            var = CAR_CONVERGED_AT_VAR,
            recipe = boss_jobs::probe::CAR_INSTANT_RECIPE,
        )
    });
    let truncated = boss_jobs::probe::counts_a_page_it_may_not_have_read(probe).map(|token| {
        format!(
            "THIS PROBE COUNTS A PAGE IT MAY NOT HAVE READ — `{token}` asks the server for a \
             PAGE, and a page answers in the same shape the whole list does: \
             `{{\"data\":[…],\"total\":345}}` is a correct reply to a request for 300, and \
             nothing in it says 45 rows were left behind. A count taken over that page is a \
             count of a set the probe did not see.\n  {evidence}\n  \
             UNLIKE THE THREE WARNINGS ABOVE, THIS SHAPE CAN FAIL OPEN: a probe that counts \
             a page to assert an ABSENCE — no row since the cutoff matches the bad shape — \
             records a false green when the row it wanted was in the tail. It is a warning \
             and not a refusal only because `limit=` is right far more often than it is \
             wrong and this is a coarse text scan, so read this one rather than skim it.",
            evidence = boss_jobs::probe::TRUNCATED_PAGE_EVIDENCE,
        )
    });
    let mention = boss_jobs::probe::greps_a_name_where_a_definition_is_meant(probe).map(|name| {
        format!(
            "THIS PROBE COUNTS A NAME WHERE A DEFINITION LOOKS LIKE WHAT IT MEANS — `{name}` \
             matches every line that MENTIONS it, and the doc comment above a call site is \
             such a line. The count is nonzero whether or not the thing was ever defined.\n  \
             {evidence}\n  \
             This is a warning, not a refusal, because the text cannot say which you meant: \
             counting a mention is a legitimate claim (a call site exists, a literal is still \
             in the config, a name was not removed). But when a definition WAS meant it fails \
             OPEN — absent reads as present — so it is worth the ten seconds to check.",
            evidence = boss_jobs::probe::MENTION_NOT_DEFINITION_EVIDENCE,
        )
    });
    inverted
        .into_iter()
        .chain(rewritten)
        .chain(unguarded)
        .chain(moving)
        .chain(truncated)
        .chain(mention)
}

/// THE NINTH SHAPE, AND THE ONE THE TEXT CANNOT SHOW (backlog
/// 8ac42ee5): a grep over a file whose every match is a COMMENT. It
/// lives outside [`shape_warnings`] because it needs the tree — `read`
/// answers a path at the revision `at` names — so each door that has a
/// tree hands it one: `boss gate --park-probe` reads the car's own tip,
/// where the comment that defeats a removal probe is written by the same
/// car, and the hand door of `boss prove` reads HEAD where the probe
/// runs, where a NOT YET that will never clear is otherwise
/// indistinguishable from one that will.
pub(crate) fn prose_only_warning(
    probe: &str,
    at: &str,
    read: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let found = boss_jobs::probe::a_grep_only_prose_answers(probe, read)?;
    let shown: Vec<String> = found
        .lines
        .iter()
        .take(3)
        .map(|(n, text)| format!("    {}:{n}: {text}", found.path))
        .collect();
    let more = found.lines.len().saturating_sub(shown.len());
    let more = if more > 0 {
        format!("\n    … and {more} more, all comments")
    } else {
        String::new()
    };
    Some(format!(
        "THIS PROBE'S GREP IS ANSWERED ONLY BY PROSE — `{pattern}` matches {path} at {at} on \
         {n} line(s), and every one is a comment:\n{lines}{more}\n  \
         Whatever the probe concludes from that count is a conclusion about a sentence. An \
         ABSENCE assertion over it cannot pass while the comment stands — and the car that \
         removes a thing is usually the one that writes the comment saying so — while a \
         PRESENCE assertion passes on the mention with nothing behind it.\n  {evidence}\n  \
         This is a warning, not a refusal: counting a mention can be the claim, and the \
         comment test is a line-prefix scan.",
        pattern = found.pattern,
        path = found.path,
        n = found.lines.len(),
        lines = shown.join("\n"),
        evidence = boss_jobs::probe::PROSE_ONLY_EVIDENCE,
    ))
}

/// A reader of `<rev>:<path>` in the repository at `dir`, for
/// [`prose_only_warning`]: `None` for anything git will not show, which
/// the detector reads as nothing to judge.
pub(crate) fn git_show_reader(dir: &Path, rev: &str) -> impl Fn(&str) -> Option<String> + use<> {
    let (dir, rev) = (dir.to_path_buf(), rev.to_string());
    move |path: &str| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["show", &format!("{rev}:{path}")])
            .output()
            .ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    }
}

/// The override, resolved once: `None` when the flag was not given,
/// refusing a flag given with no reason — an override with no stated
/// reason is the silent yes the whole verb exists to end.
pub(crate) fn override_reason(given: Option<&str>) -> Result<Option<&str>> {
    match given.map(str::trim) {
        Some("") => bail!(
            "{OVERRIDE_FLAG} needs a reason: it is recorded in the proof, where the next \
             reader — or `--recheck`, unattended — finds out why a refused probe was run."
        ),
        other => Ok(other),
    }
}

/// WHERE A PROBE'S TREE IS OBSERVED: the directory the probe will
/// actually run in, which is the shell's own `cwd` — the converged
/// checkout at the unattended door, the recorded one under `--recheck`,
/// and this process's directory at the hand door. Reading it anywhere
/// else would answer about a tree the probe never opens, which is the
/// class of mistake this whole area exists to refuse.
fn probe_tree(shell: &Shell) -> crate::freshness::TreeObservation {
    crate::freshness::observe_tree(shell.cwd.as_deref().unwrap_or_else(|| Path::new(".")))
}

/// The proof record. Serialised once, stored verbatim, re-read by
/// `--recheck` — so its field names are a contract, not a detail.
/// `overridden` is the one optional key: present only when the
/// operator ran a probe this door refused, absent otherwise, so its
/// presence means something to whoever reads the proof back. The
/// unattended door never writes it: it has no override, and a probe it
/// refuses is refused on the ops-request, exit 2.
///
/// `tree` is taken by value rather than left to each door to remember,
/// because a door that forgets it records a proof that cannot say what
/// it read — the defect this closed (backlog 6f581de6). See
/// [`crate::freshness::tree_metadata`] for what the two keys mean.
pub(crate) fn proof_json(
    probe: &str,
    expect: Option<&str>,
    o: &Outcome,
    host: &str,
    at: &str,
    tree: &crate::freshness::TreeObservation,
    overridden: Option<&Value>,
) -> Value {
    let mut p = json!({
        "probe": probe,
        "expect": expect,
        "exit": o.exit,
        "stdout": clip(&o.stdout),
        "stderr": clip(&o.stderr),
        "host": host,
        // WHERE IT RAN, not just what ran. A command means the same
        // thing twice only if the host and the directory are the same
        // both times, and 3 of 10 rechecks false-failed for exactly
        // this: probes opening `git rev-parse HEAD` re-run outside a
        // repository, and probes authored on the workstation as
        // `ssh boss-gcp ...` re-run ON boss-gcp, where that name does
        // not resolve. Both are free to record — the verb already
        // knows them (66fd64c6).
        "cwd": std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        "at": at,
    });
    // WHAT IT READ, beside where it ran. `cwd` names a directory whose
    // contents change under it; these name the revision that answered.
    for (k, v) in crate::freshness::tree_metadata(tree)
        .as_object()
        .into_iter()
        .flatten()
    {
        p[k] = v.clone();
    }
    if let Some(o) = overridden {
        p["overridden"] = o.clone();
    }
    p
}

/// Can the recorded probe be re-run HERE, meaning the same thing?
///
/// The distinction this draws is the whole point of the packet: a probe
/// that cannot be re-run is not the same fact as a claim that stopped
/// being true, and `--recheck` used to render them identically — as
/// NO LONGER HOLDS. An instrument that cries wolf 30% of the time gets
/// ignored, and then decay stops being detected at all.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Rerunnable {
    /// Same host, and the directory is available.
    Here,
    /// Recorded somewhere else. Re-running here tests a different thing.
    WrongHost { recorded: String, here: String },
    /// The directory the probe assumed is gone.
    MissingDir { cwd: String },
}

/// ABSENT CONTEXT IS NOT A MISMATCH. Proofs recorded before this change
/// carry no `cwd`, and the oldest carry no `host`. Refusing on missing
/// data would break every proof already on the board, so an unrecorded
/// field means "cannot check", and the recheck proceeds exactly as it
/// did before.
pub(crate) fn rerunnable(
    recorded_host: Option<&str>,
    here: &str,
    cwd: Option<&str>,
    dir_exists: bool,
) -> Rerunnable {
    if let Some(rec) = recorded_host.filter(|h| !h.is_empty() && *h != "unknown")
        && rec != here
    {
        return Rerunnable::WrongHost {
            recorded: rec.to_string(),
            here: here.to_string(),
        };
    }
    if let Some(dir) = cwd.filter(|c| !c.is_empty())
        && !dir_exists
    {
        return Rerunnable::MissingDir {
            cwd: dir.to_string(),
        };
    }
    Rerunnable::Here
}

/// The matching core `find_car` and `boss job` share: a row matches by
/// exact `metadata.branch`, or by an id prefix of at least 8
/// characters. Pure and message-free so each verb can refuse in its
/// own vocabulary — `find_car`'s "no ship-a-change car" would be a lie
/// coming from `boss job get`, which sees every kind.
pub(crate) fn matching_jobs<'a>(rows: &'a [Value], given: &str) -> Vec<&'a Value> {
    rows.iter()
        .filter(|c| {
            let by_branch = c
                .get("metadata")
                .and_then(|m| m.get("branch"))
                .and_then(Value::as_str)
                == Some(given);
            let by_id = c
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|i| i.starts_with(given) && given.len() >= 8);
            by_branch || by_id
        })
        .collect()
}

/// Which cars a lookup may legitimately resolve to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Eligible {
    /// Recording a proof WRITES to the car, so a car the pipeline has
    /// finished with is not a candidate: it cannot take a new proof.
    Live,
    /// `--recheck` re-runs a probe already recorded on the car and
    /// writes nothing, so a finished car is a legitimate target — which
    /// is the whole reason [`crate::gate::all_cars_at`] pages over closed
    /// cars instead of filtering `status=open` at the API.
    AnyStatus,
}

/// The car's `status` as the jobs API spelled it, or `"?"` when the row
/// carries none. For DISPLAY only — whether a car is live is
/// `boss_jobs::car::is_open`'s single answer, never a second reading of
/// this string.
fn status_of(car: &Value) -> &str {
    car.get("status").and_then(Value::as_str).unwrap_or("?")
}

/// One `id8  status  title` line per candidate, so a refusal that lists
/// cars is answerable from the refusal.
///
/// The id is shortened by [`crate::train::id8`] rather than a second
/// `[..8]` of its own: the bare slice panicked on any row whose id the
/// reader could not find, which is the listing for a malformed row
/// crashing instead of printing it.
fn listed(rows: &[&Value]) -> String {
    rows.iter().fold(String::new(), |mut out, c| {
        out.push_str(&format!(
            "\n  {}  {}  {}",
            crate::train::id8(c.get("id").and_then(Value::as_str).unwrap_or("?")),
            status_of(c),
            c.get("title").and_then(Value::as_str).unwrap_or("?")
        ));
        out
    })
}

/// Find the one car for `given` — a branch name, or an id prefix.
///
/// Refuses on ambiguity rather than picking. `boss park` learned this
/// the expensive way: it took the LAST match from a list the API
/// returns newest-first, and quietly parked two cars against a stale
/// receipt. Choosing among candidates is how that happens, so this
/// does not choose.
///
/// It does DISCARD candidates that cannot take a proof at all — which
/// is not choosing between live cars, and is what backlog 0a58827d asked
/// for. Measured live 2026-09-10 05:02 UTC: `boss prove
/// feat/arrival-runs-the-probe-rerail` refused with "2 open cars match"
/// over one live car and one closed, abandoned twin ("gated, then
/// changed"). There was one candidate, so the refusal was wrong to fire,
/// and the word "open" sent the reader to investigate a dead car — two
/// queries spent learning what the message had already asserted falsely.
///
/// Whether a car is live is `boss_jobs::car::is_open`, the one definition
/// `boss park`, `boss rerail` and the auto-park handler already decide
/// on (CLAUDE.md §9a) — this verb does not carry a second reading of
/// `status`. What the CALLER's mode decides is what a finished car means
/// here:
///
/// - [`Eligible::Live`] — recording a proof is a WRITE, so a finished
///   car is discarded outright. With nothing left the refusal says the
///   matches are finished, rather than sending the reader after a typo.
/// - [`Eligible::AnyStatus`] — `--recheck` writes nothing, so a finished
///   car is a legitimate target; a live match is still PREFERRED when
///   the name has one, so a recheck resolves where it used to.
///
/// Either way every listed candidate is labelled with its status, so a
/// refusal that must name a finished car stays answerable without a
/// second query — the per-row label replaces the one collective "open"
/// / "closed" word, which could only describe a homogeneous set.
pub(crate) fn find_car<'a>(
    cars: &'a [Value],
    given: &str,
    eligible: Eligible,
) -> Result<&'a Value> {
    let matched = matching_jobs(cars, given);
    let live: Vec<&Value> = matched
        .iter()
        .copied()
        .filter(|c| boss_jobs::car::is_open(c))
        .collect();
    let candidates: Vec<&Value> = match eligible {
        Eligible::Live => live,
        Eligible::AnyStatus if !live.is_empty() => live,
        Eligible::AnyStatus => matched.clone(),
    };

    match candidates.len() {
        1 => Ok(candidates[0]),
        // Nothing live, but the name DID match — say so, rather than
        // sending the operator to hunt a typo that is not there.
        0 if !matched.is_empty() => bail!(
            "no car for {given:?} that can take a proof — every match is finished:{}\n\n\
             A closed or cancelled car is done, and recording a new proof on one would be \
             writing to a terminal. Use --recheck to re-run the probe already recorded on \
             it, or name the car that carries this work now.",
            listed(&matched)
        ),
        0 => bail!(
            "no ship-a-change car for {given:?}. Give the car's branch exactly \
             as it was parked, or at least 8 characters of its id."
        ),
        n => bail!(
            "{n} cars match {given:?} — say which:{}",
            listed(&candidates)
        ),
    }
}

/// The car's `proven` step, refusing unless it is actually reachable.
///
/// `proven` is gated on `job.metadata.merged = "true"`, so a step still
/// pending means the change has not merged. Recording proof there would
/// be recording that unshipped code works in production.
pub(crate) fn proven_step(car: &Value, replace: bool) -> Result<&Value> {
    let step = car
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("title").and_then(Value::as_str) == Some(PROVEN))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "the car has no step titled {PROVEN:?}. The ship-a-change workflow was \
                 renamed or re-versioned; this verb fills a step by title and will not \
                 guess which one was meant."
            )
        })?;

    match step.get("status").and_then(Value::as_str) {
        // --replace WITH NOTHING TO REPLACE (backlog 251dba77, measured
        // 2026-09-11). The flag's write path appends to `reproof` in JOB
        // metadata and never touches the step, because a COMPLETED step
        // is frozen — so given an open step it recorded a second-class
        // copy of a proof nobody had yet, printed "re-proven — recorded
        // as reproof #1. The original proof is untouched on the step"
        // (two assertions, both false), and left the step `ready`. The
        // car did not advance and nothing said so.
        Some(open @ ("ready" | "active")) if replace && recorded_proof_on(step).is_none() => bail!(
            "--replace has nothing to replace: the `{PROVEN}` step is {open} and carries no \
             recorded proof, so there is no first proof to keep beside a second one.\n\n\
             Run the same command WITHOUT --replace. That is the path that records a FIRST \
             proof: it writes the proof on the step and completes it, which is what makes \
             the car advance.\n\n\
             --replace is for a step already COMPLETED, whose proof is frozen and must not \
             be erased — a proof that used to hold and no longer does is evidence, not a \
             mistake (2b30eff4)."
        ),
        Some("ready") | Some("active") => Ok(step),
        // A PROOF THAT STOPPED HOLDING IS EVIDENCE, NOT A MISTAKE.
        // Car 932aa956's probe observed a real production refusal and
        // stopped holding within the hour, because the condition it
        // keyed on was transient. `--recheck` reported NO LONGER HOLDS
        // correctly and was wrong about the cause, and there was no way
        // to put a better probe under the same claim short of an
        // operator editing job metadata by hand (2b30eff4).
        Some("completed") if replace => Ok(step),
        Some("completed") => bail!(
            "this car is already proven. To check whether the proof STILL holds, \
             re-run with --recheck; it re-executes the recorded probe and changes \
             nothing. To put a BETTER probe under the same claim — because the first \
             one was transient rather than wrong — re-run with --replace, which keeps \
             the original proof and records the new one beside it."
        ),
        Some("pending") => bail!(
            "the `{PROVEN}` step is still pending, which means the car has not merged \
             — its predicate is `steps.review.done AND job.metadata.merged = \"true\"`. \
             A change that has not shipped cannot be proven in production."
        ),
        other => bail!("the `{PROVEN}` step is {other:?}, which this verb does not fill"),
    }
}

/// The proof already recorded on this step, if there is one.
///
/// The step's own `proof` field — required by the workflow when the step
/// completes, so a completed step always has one — read here to answer
/// the only question `--replace` needs: is there anything to replace?
pub(crate) fn recorded_proof_on(step: &Value) -> Option<&str> {
    step.get("metadata")
        .and_then(|m| m.get("proof"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
}

/// Which record a re-runnable probe was read from. `--recheck` reports
/// differently on each: a PROOF that fails now has decayed ("NO LONGER
/// HOLDS"), while an ATTEMPT was never a proof, so the same failure is
/// "still not proven" and the same success is "now provable" — never
/// "still HOLDS" about a claim nobody ever proved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The step's `proof`, or the newest `reproof` entry.
    Proof,
    /// The car's `proof_attempt`: a run that settled nothing, with
    /// `not_yet` as the writer stamped it.
    Attempt { not_yet: bool },
}

/// A recorded proof, as much of it as the writer stored.
#[derive(Debug)]
pub(crate) struct Recorded {
    pub probe: String,
    pub expect: Option<String>,
    /// `None` on proofs written before the context was recorded.
    pub host: Option<String>,
    pub cwd: Option<String>,
    /// The override this proof was admitted under, if the operator ran
    /// a probe the door refused. Read back so `--recheck` — the
    /// unattended re-run — says so instead of reporting a clean HOLDS
    /// on a probe somebody had to argue past.
    pub overridden: Option<Value>,
    pub source: Source,
}

/// Read back a recorded proof so `--recheck` can re-run it.
pub(crate) fn recorded_probe_for(car: &Value, step: &Value) -> Result<Recorded> {
    // A REPLACEMENT SUPERSEDES THE ORIGINAL, and is read in preference
    // to it — the same precedence `regate_receipt` has over a stale
    // gate receipt. The original stays on the step; this is which proof
    // `--recheck` should be re-running, not which one happened.
    if let Some(last) = car
        .get("metadata")
        .and_then(|m| m.get("reproof"))
        .and_then(Value::as_array)
        .and_then(|a| a.last())
        && let Some(p) = last.get("proof")
    {
        return read_proof(p);
    }
    if recorded_proof_on(step).is_some() {
        return recorded_probe(step);
    }
    // A NOT-YET CAR HAS NO PROOF TO RE-RUN, AND ONE PROBE THAT RAN
    // (726562de). Its `proven` step is still ready, so the step reader
    // below would refuse with "nothing to re-run" — but "not yet" is a
    // request to run it again, and the car holds exactly what ran in
    // `proof_attempt` (the forge's record or this door's, one shape).
    // Read in preference to nothing, never in preference to a proof.
    if let Some(attempt) = car.get("metadata").and_then(|m| m.get("proof_attempt"))
        && attempt.get("probe").and_then(Value::as_str).is_some()
    {
        let mut rec = read_proof(attempt)?;
        rec.source = Source::Attempt {
            not_yet: attempt.get("not_yet").and_then(Value::as_bool) == Some(true),
        };
        return Ok(rec);
    }
    recorded_probe(step)
}

/// Is `value` one of the variants `field_type` declares, for the field
/// named `name` on this step?
///
/// Reads the step's OWN field spec — the one that arrived with the car
/// we already fetched, from the workflow version this packet is pinned
/// to. Deliberately not a constant in this file: the vocabulary already
/// lives in `infra/platform/workflows/` and in the registry
/// default, and a third copy here would be the drift CLAUDE.md 9a is
/// about. Validating against the packet's own spec also means a
/// workflow version that adds a variant needs no CLI release.
///
/// `Ok(())` when the field is absent or free-text: this verb is not the
/// place to invent a contract the protocol did not state.
pub(crate) fn check_enum_field(step: &Value, name: &str, value: &str) -> Result<()> {
    let Some(ft) = step
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|f| f.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|f| f.get("field_type"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    if !ft.contains('|') {
        return Ok(());
    }
    let allowed: Vec<&str> = ft.split('|').map(str::trim).collect();
    if allowed.contains(&value) {
        return Ok(());
    }
    bail!(
        "--{name} {value:?} is not one of {}. This is checked BEFORE the probe runs: \
         the probe would have executed against production, passed, and then been \
         thrown away when the API refused the record — which is how two proofs were \
         silently lost on 2026-09-02.",
        allowed.join(" | ")
    )
}

pub(crate) fn recorded_probe(step: &Value) -> Result<Recorded> {
    let md = step.get("metadata");
    let raw = md.and_then(|m| m.get("proof")).ok_or_else(|| {
        anyhow::anyhow!(
            "this step carries no `proof`, so there is nothing to re-run. It was \
                 completed before proof was mechanised, or filled by hand — its \
                 `verified` prose is a claim with no probe under it."
        )
    })?;
    read_proof(raw)
}

/// Parse one recorded proof, however it was stored.
fn read_proof(raw: &Value) -> Result<Recorded> {
    // Stored as a JSON string (verbatim, like the gate receipt) or, if a
    // future writer stores it structurally, as an object. Read both.
    let v: Value = match raw.as_str() {
        Some(s) => serde_json::from_str(s)
            .map_err(|e| anyhow::anyhow!("the recorded proof is not readable JSON: {e}"))?,
        None => raw.clone(),
    };
    let probe = v
        .get("probe")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("the recorded proof has no `probe` to re-run"))?
        .to_string();
    let expect = v.get("expect").and_then(Value::as_str).map(str::to_string);
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(Recorded {
        probe,
        expect,
        host: text("host"),
        cwd: text("cwd"),
        overridden: v.get("overridden").cloned(),
        source: Source::Proof,
    })
}

/// The probe a car recorded for itself at park time, read back as the
/// pair `boss prove` would have been given by hand.
///
/// WHERE IT RUNS. Two callers run this text on two different machines:
/// `--from-car` runs it wherever the operator is standing, and the
/// arrival rule runs it on the FORGE HOST as david, in the converged
/// checkout, with that host's tools. The forge has no kubeconfig, so a
/// probe can pass here and be unrunnable there — measured 2026-09-09
/// (f9304366), which is why `boss gate --park-probe` refuses a probe
/// naming a tool in infra/forge/host-absent-tools.txt.
///
/// THE CAR CARRIES ITS PROBE (28ac45ab): `boss gate --park-probe/
/// --park-expect` stamps the intent, the auto-park handler copies it
/// onto the car under `boss_jobs::car::PROOF_*`, and this is the read
/// side — `--from-car` here, and the arrival rule's runner. The keys
/// are one definition in core so the three cannot drift.
///
/// Refuses, naming the door, when the car recorded nothing — and says
/// so differently for an EVENT-BOUND car, whose builder decided a
/// machine cannot prove it: running "no probe" as a probe would be the
/// silent yes this verb exists to end.
pub(crate) fn car_probe(car: &Value) -> Result<(String, String)> {
    let md = |k: &str| {
        car.get("metadata")
            .and_then(|m| m.get(k))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if let Some(event) = md(boss_jobs::car::PROOF_EVENT) {
        bail!(
            "this car is EVENT-BOUND, not probed: {event}\n\n\
             Its builder recorded the event that proves it instead of a command, so there \
             is nothing for --from-car to run. Prove it by hand when the event fires: \
             boss prove <car> --probe '<what you observed>' --expect '<string>' --verified '<prose>'."
        );
    }
    let Some(probe) = md(boss_jobs::car::PROOF_PROBE) else {
        bail!(
            "this car recorded no `{}`, so --from-car has nothing to run. The probe is \
             written at park time — `boss gate --park-probe '<cmd>' --park-expect '<string>'` \
             — by the builder who knows what the change does. Give one now with --probe \
             and --expect instead.",
            boss_jobs::car::PROOF_PROBE
        );
    };
    let Some(expect) = md(boss_jobs::car::PROOF_EXPECT) else {
        bail!(
            "this car recorded a `{}` but no `{}` — a probe asserting nothing is `echo hi`. \
             The gate verb refuses that pair, so the car was edited by hand; give --expect \
             here or fix the car's metadata.",
            boss_jobs::car::PROOF_PROBE,
            boss_jobs::car::PROOF_EXPECT
        );
    };
    Ok((probe, expect))
}

/// What a car-carried proof MEANS, when the operator did not say: the
/// car's own summary — the claim the change makes, which is exactly
/// what the probe checks in production. `verified` stays required in
/// the record; it just has a source the builder already wrote.
pub(crate) fn car_verified(car: &Value, given: Option<String>) -> Result<String> {
    if let Some(v) = given {
        return Ok(v);
    }
    car.get("metadata")
        .and_then(|m| m.get("summary"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "--verified is required: the car has no summary to fall back on, so say \
                 what the probe means, in prose, for a reader"
            )
        })
}

/// The host a verb is running on. Shared with `boss car open`, which
/// records it on the car at build start — the same question, one
/// definition (CLAUDE.md §9a).
pub(crate) fn host() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// The evidence the `proven` step carries, and when it was recorded.
///
/// `now` is the instant the probe ran under, not a fresh one taken at
/// write time: the proof and its stamp describe a single event, and a
/// slow API call should not drag the timestamp away from the probe.
fn proven_metadata(
    verified: &str,
    proof: &str,
    method: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Value {
    let mut md = json!({
        "verified": verified,
        // VERBATIM, as a string, exactly like the gate receipt: what the
        // machine saw, not a summary of it.
        "proof": proof,
        "completed_at": crate::gate::stamp(now),
    });
    if let Some(m) = method {
        md["method"] = json!(m);
    }
    md
}

// ---------------------------------------------------------------------
// THE UNATTENDED DOOR — `boss prove <car> --from-car --unattended`
// ---------------------------------------------------------------------
//
// What the forge's ops-runner runs for a `run-car-probe` ops-request
// (infra/ops/verbs/run-car-probe.json), filed by the dispatcher rule
// run-car-probes-on-train-arrived for every car aboard an arrived
// train that recorded a probe at park time, and again by
// recheck-failing-probes-hourly for a car whose last run settled
// nothing. Until backlog 9f00a805 (consolidation H8, car 2) the verb
// ran infra/forge/run-car-probe.sh — 482 lines of shell re-implementing
// this file's judge, verdict and records, because the forge had no
// `boss` binary; car 1 installs the tree's CLI there from the converged
// image (infra/estate/install-cli-from-image.sh), so the twin is
// retired and the ops-request runs this door instead. Measured before
// choosing it: of the four candidate twins this was the only one with
// a CLI verb to retire INTO (tenant-census.sh, retire-example-
// reference-rows.sh and prune-registry-versions.sh have none), and the
// gaps it had were these, each closed here:
//
//   - the probe runs as ANOTHER USER (root drops to $BOSS_PROBE_USER,
//     default david) in the converged checkout ($BOSS_PROBE_DIR,
//     default /home/david/boss) under a timeout ($BOSS_PROBE_TIMEOUT,
//     default 60 s) — `Shell::unattended`;
//   - the probe's environment is exactly what a recorded probe is
//     promised: BOSS_JOBS_URL, a READ-ONLY reader identity as
//     BOSS_SOR_USER (backlog 61085a9e — never this verb's own write
//     actor, which is stripped), the reader's port table as
//     BOSS_SOR_PORTS (design 28d2bed9, read as data from the checkout's
//     infra/forge/sor-ports.env), infra/forge/probe-bin first on
//     PATH so `boss-sor-read` is the cheap thing to type, and the
//     car's OWN converged instant (backlog a92571a6 — the fixed cutoff
//     a dated claim compares against, since the checkout's HEAD moves
//     with every train);
//   - every outcome lands on the car: PROVEN completes `proven` with
//     the proof record (and `proven_by`, which the yard reads); every
//     other verdict — NOT YET, NOT RUN, NOT PROVEN — stamps
//     `proof_attempt` and leaves `proven` ready, where the hand door
//     prints NOT PROVEN to the operator and records nothing;
//   - the exit code IS the verdict the ops-request carries: 0 proven,
//     1 not proven, 3 did not run, 75 not yet, 2 refused;
//   - a refusal changes nothing and names why (exit 2); a car already
//     proven is "nothing to run", exit 0, because the daily recheck
//     re-files for a car whose attempt has since been settled by hand.

/// THE READER A RECORDED PROBE READS THE SYSTEM OF RECORD AS (backlog
/// 61085a9e). The probe is program text a builder wrote, run here as
/// the probe user; the privilege matches the job — read the system of
/// record, change nothing. `audit-readonly` is what core policy
/// (boss-policy-client::defaults) grants Read at Scope::All on every
/// shipped resource and NO other action anywhere: verified by effect on
/// the live deployment, a PATCH and a step PUT under it both answered
/// 403 while every list read matched the operator's. The role is
/// NAMED in `boss_core::roles` and DEFINED by the policy defaults, and
/// `the_probes_reader_role_can_read_everything_and_write_nothing` holds
/// them equal (CLAUDE.md §9a); this crate reads the name from core
/// (`identity::READER_ROLE`) rather than spelling it again — until
/// backlog d843abf2 (2026-09-19) it was a literal here, the second
/// spelling the unidentified reader in identity.rs would have needed a
/// third of. The id is the one the credentials door already names for
/// this reader (`boss_jobs::credentials`).
pub(crate) const READER_ACTOR: &str = "automation:run-car-probe-reader";
pub(crate) use crate::identity::READER_ROLE;

/// The `x-boss-user` header a recorded probe's reader sends —
/// `boss-sor-read` puts it on the wire verbatim. One shape with the
/// CLI's own unidentified read (identity.rs), because they are the
/// same identity: a reader nobody-in-particular is, with the
/// platform's own read role.
pub(crate) use crate::identity::reader_header;

/// The reader's port table, from `infra/forge/sor-ports.env` in the
/// checkout the probe runs in: `name=port` lines, `#` comments and
/// blanks dropped, one space between entries — the shape
/// `boss-sor-read` parses from `BOSS_SOR_PORTS`. Read as DATA, never
/// sourced (this verb runs as root at that door). An absent file is an
/// empty table, which the reader treats as absent: jobs only.
pub(crate) fn sor_ports_table(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where the twin's tools live, relative to the checkout the probe runs
/// in: the sanctioned reader on PATH, and the port table it reads.
const PROBE_BIN: &str = "infra/forge/probe-bin";
const SOR_PORTS_ENV: &str = "infra/forge/sor-ports.env";

impl Shell {
    /// The unattended door's shell, from the same environment the
    /// twin read (`BOSS_PROBE_USER`, `BOSS_PROBE_DIR`,
    /// `BOSS_PROBE_TIMEOUT`, `BOSS_PROBE_READER_ACTOR`) so a forge that
    /// set none of them changes nothing. `base` is the system of record
    /// this verb resolved, handed to the probe as `BOSS_JOBS_URL`.
    pub(crate) fn unattended(base: &str) -> Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let user = var("BOSS_PROBE_USER").unwrap_or_else(|| "david".into());
        let dir = std::path::PathBuf::from(
            var("BOSS_PROBE_DIR").unwrap_or_else(|| "/home/david/boss".into()),
        );
        let timeout = match var("BOSS_PROBE_TIMEOUT") {
            Some(t) => t.parse::<u64>().map_err(|_| {
                anyhow::anyhow!("BOSS_PROBE_TIMEOUT={t:?} is not a number of seconds")
            })?,
            None => 60,
        };
        Ok(Self {
            path_prefix: None,
            cwd: Some(dir.clone()),
            user: Some(user),
            timeout_secs: Some(timeout),
            env: Vec::new(),
            // This verb's own write credential never reaches the
            // probe's text; `boss_jobs::probe::names_an_actor` refuses
            // a probe that sets one, and this is the other half.
            strip: vec![
                crate::identity::ACTOR_ENV.into(),
                crate::identity::ACTOR_FILE_ENV.into(),
            ],
        }
        .with_probe_reader(Some(&dir), base))
    }

    /// THE ENVIRONMENT A RECORDED PROBE IS PROMISED, BUILT ONCE FOR
    /// BOTH DOORS (backlog 18fee481, measured 2026-09-22). `tree` is the
    /// checkout the reader and its port table are read out of — the
    /// converged checkout at the unattended door, the operator's own
    /// worktree at the hand door — and `base` is the system of record
    /// the verb resolved.
    ///
    /// WHY THE HAND DOOR GETS IT TOO. `--from-car` exists so an operator
    /// can rehearse locally what the forge will run, and its own help
    /// says so; it ran that text with the caller's PATH and nothing
    /// else, so `boss prove <car> --from-car` on the dev pod answered
    /// `boss-sor-read not found` for every car whose probe reads the
    /// system of record — which is most of them. The refusal was
    /// correct (did-not-run, not failed) and the defect was upstream of
    /// it: two doors onto one text, in two environments, and the flag
    /// meant to make them agree was the one that did not. Nothing has to
    /// be installed for this — the reader is IN THE TREE beside its
    /// route table. Measured that hour: the shed sat at 8 of 12 cars
    /// open past 24 h, oldest 108 h, with the forge's hourly recheck as
    /// its only worker.
    ///
    /// The privilege is unchanged and server-enforced: the identity is
    /// the READ-SCOPED reader, at both doors, and never
    /// `operator:unidentified` — an unidentified read is answered with a
    /// NARROWER WORLD silently, which is a green absence assertion
    /// against a page the probe was never allowed to see (61085a9e). It
    /// does not come from the tree, so a caller standing outside a
    /// worktree is still handed it; what such a caller loses is the
    /// reader on PATH, and a probe that needs it then says `not found`,
    /// which is the honest did-not-run.
    ///
    /// What this does NOT make the same is the rest of the hand door:
    /// the probe still runs HERE, as the operator, with no timeout and
    /// with their own `BOSS_ACTOR` — `admit` already says so, and a
    /// pod-local proof is the established shape for a claim only the
    /// cluster can show.
    pub(crate) fn with_probe_reader(mut self, tree: Option<&Path>, base: &str) -> Self {
        let reader = std::env::var("BOSS_PROBE_READER_ACTOR")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| READER_ACTOR.into());
        let ports = tree
            .and_then(|t| std::fs::read_to_string(t.join(SOR_PORTS_ENV)).ok())
            .map(|t| sor_ports_table(&t))
            .unwrap_or_default();
        self.path_prefix = tree.map(|t| t.join(PROBE_BIN));
        self.env.push(("BOSS_JOBS_URL".into(), base.to_string()));
        self.env
            .push(("BOSS_SOR_USER".into(), reader_header(&reader)));
        self.env.push(("BOSS_SOR_PORTS".into(), ports));
        self
    }
}

/// The proven step, by slug first and title second — the twin's read,
/// and the one the arrival handler uses to decide a car is probe-able.
fn proven_step_unattended(car: &Value) -> Option<&Value> {
    let steps = car.get("steps").and_then(Value::as_array)?;
    steps
        .iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("proven"))
        .or_else(|| {
            steps
                .iter()
                .find(|s| s.get("title").and_then(Value::as_str) == Some(PROVEN))
        })
}

/// What the unattended door found before running anything. Pure over
/// the car, so the refusals are pinned without a socket.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admitted {
    /// Run this probe, expect this token, complete this step id with
    /// this `verified` prose.
    Run {
        probe: String,
        expect: String,
        step_id: String,
        verified: String,
    },
    /// `proven` is already completed — nothing to run, and not a red.
    AlreadyProven,
    /// Not a car this door will run: the reason, for the packet.
    Refused(String),
}

/// The twin's admission, in order: kind, merged, probe and expect
/// (with the event-bound car named as such), a `proven` step, its
/// status. Every refusal changes nothing.
pub(crate) fn admit_unattended(car: &Value) -> Admitted {
    let short = crate::train::id8(car.get("id").and_then(Value::as_str).unwrap_or("?"));
    let kind = car.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind != "ship-a-change" {
        return Admitted::Refused(format!("{short} is a {kind}, not a ship-a-change car"));
    }
    let merged = car
        .get("metadata")
        .and_then(|m| m.get("merged"))
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default();
    if merged != "true" {
        return Admitted::Refused(format!(
            "{short} has not merged (metadata.merged={merged:?}) — a probe against production \
             proves nothing about unshipped code"
        ));
    }
    let (probe, expect) = match car_probe(car) {
        Ok(pair) => pair,
        Err(e) => return Admitted::Refused(format!("{short}: {e}")),
    };
    let Some(step) = proven_step_unattended(car) else {
        return Admitted::Refused(format!("{short} has no proven step"));
    };
    match step.get("status").and_then(Value::as_str).unwrap_or("") {
        "ready" | "active" => {}
        "completed" => return Admitted::AlreadyProven,
        other => {
            return Admitted::Refused(format!(
                "{short}'s proven step is {other:?} (pending = not merged; skipped = abandoned)"
            ));
        }
    }
    let Some(step_id) = step.get("id").and_then(Value::as_str) else {
        return Admitted::Refused(format!("{short}'s proven step has no id"));
    };
    // `verified` is the car's own claim (the summary), the same
    // default the hand door's --from-car uses; a car without one is
    // still proven, by the probe it recorded.
    let verified = car_verified(car, None)
        .unwrap_or_else(|_| "proven by the probe the car recorded at park time".into());
    Admitted::Run {
        probe,
        expect,
        step_id: step_id.to_string(),
        verified,
    }
}

/// The one line the ops-request's output leads its verdict with: the
/// ALL-CAPS word a reader of the packet scans for, then the car, then
/// why. The twin printed the same four words for two weeks of packets,
/// and `the_unattended_verdict_line_leads_with_its_word` pins them.
pub(crate) fn unattended_verdict_line(
    short: &str,
    verdict: &Verdict,
    expect: &str,
    why: &str,
) -> String {
    match verdict {
        Verdict::Proven => format!("PROVEN {short} — exit 0 and printed {expect:?}"),
        Verdict::NotYet { .. } => format!("NOT YET {short} — {why}"),
        Verdict::Unrunnable { .. } => format!("NOT RUN {short} — {why}"),
        Verdict::NotProven(_) => format!("NOT PROVEN {short} — {why}"),
    }
}

/// The exit code the ops-request carries for a verdict — see
/// [`NOT_YET_EXIT`], [`UNRUNNABLE_EXIT`].
pub(crate) fn unattended_exit(verdict: &Verdict) -> i32 {
    match verdict {
        Verdict::Proven => 0,
        Verdict::NotYet { .. } => NOT_YET_EXIT,
        Verdict::Unrunnable { .. } => UNRUNNABLE_EXIT,
        Verdict::NotProven(_) => 1,
    }
}

/// Who completed `proven` at this door — what the yard's inspection
/// shed shows as the stamp's `by`, and the verb's name so a reader of
/// the step finds the door that wrote it.
pub(crate) const PROVEN_BY: &str = "run-car-probe";

/// Run a car's recorded probe the way the forge's ops-runner asks for
/// it, and write the verdict on the car — see the header of this
/// section. `car_id` is the full uuid the verb's argument pattern
/// admitted; the car is re-read from the system of record, never
/// trusted from the packet that asked.
pub(crate) async fn run_unattended(car_id: &str, now: chrono::DateTime<chrono::Utc>) -> Result<()> {
    let http = reqwest::Client::new();
    let base = crate::gate::resolve_jobs_base(None)?;
    let car = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{car_id}"),
        None,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("GET /api/jobs/{car_id} answered no body"))?;
    let short = crate::train::id8(car_id);
    let (probe, expect, step_id, verified) = match admit_unattended(&car) {
        Admitted::Run {
            probe,
            expect,
            step_id,
            verified,
        } => (probe, expect, step_id, verified),
        Admitted::AlreadyProven => {
            println!("boss prove: {short} is already proven — nothing to run");
            return Ok(());
        }
        Admitted::Refused(why) => {
            eprintln!("boss prove: REFUSED — {why}");
            std::process::exit(REFUSED_EXIT);
        }
    };
    // The gate's rule on a recorded probe, at this door too: a probe
    // that reads the system of record unidentified is refused — it
    // would pass an absence assertion against a narrowed world — and
    // there is no operator here to override.
    if let Some(r) = admit(&probe, true).refusal {
        eprintln!("boss prove: REFUSED — {r}");
        std::process::exit(REFUSED_EXIT);
    }

    let shell = Shell::unattended(&base)?.with_car_instant(car_merge_ref(&car));
    println!("boss prove: {short}  $ {probe}");
    // THE TREE THIS PROBE READS, observed where it will run — the
    // converged checkout. Here the standing is nearly always
    // `unreadable`, because the forge's clone need not carry a
    // remote-tracking ref, and `tree_head` is the fact that matters:
    // production's own revision at the moment the claim was judged.
    let tree = probe_tree(&shell);
    let o = execute_with(&probe, &shell)?;
    let at = now.to_rfc3339();
    let here = host();
    let verdict = verdict(&probe, &o, Some(&expect));
    if let Verdict::Proven = verdict {
        let proof = proof_json(&probe, Some(&expect), &o, &here, &at, &tree, None);
        let mut md = proven_metadata(&verified, &serde_json::to_string(&proof)?, None, now);
        md["proven_by"] = json!(PROVEN_BY);
        crate::gate::api(
            &http,
            reqwest::Method::PUT,
            &format!("/api/jobs/{car_id}/steps/{step_id}"),
            Some(json!({"status": "completed", "metadata": md})),
        )
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "the probe passed but recording it on {short} failed — {e}; re-file the \
                 ops-request or run boss prove {short} --from-car"
            )
        })?;
        println!(
            "boss prove: {}",
            unattended_verdict_line(&short, &verdict, &expect, "")
        );
        let shown = o.stdout.trim();
        if !shown.is_empty() {
            println!("{shown}");
        }
        return Ok(());
    }
    let why = match &verdict {
        Verdict::NotYet { said } => not_yet_why(&here, said),
        Verdict::Unrunnable { missing } => unrunnable_why_unattended(
            &here,
            missing,
            shell.user.as_deref().unwrap_or("?"),
            shell.cwd.as_deref().unwrap_or(Path::new("?")),
        ),
        Verdict::NotProven(e) => e.to_string(),
        Verdict::Proven => unreachable!("handled above"),
    };
    let prior = car.pointer("/metadata/proof_attempt");
    let attempt = attempt_json(&probe, Some(&expect), &o, &here, &at, &why, prior);
    crate::gate::api(
        &http,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{car_id}/metadata"),
        Some(json!({"proof_attempt": attempt})),
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "probe exited {} and recording the attempt on {short} failed too — {e}",
            o.exit
        )
    })?;
    println!(
        "boss prove: {}",
        unattended_verdict_line(&short, &verdict, &expect, &why)
    );
    println!("boss prove: proof_attempt recorded, proven stays ready");
    println!(
        "  stdout: {}\n  stderr: {}",
        if o.stdout.trim().is_empty() {
            "(empty)"
        } else {
            o.stdout.trim()
        },
        if o.stderr.trim().is_empty() {
            "(empty)"
        } else {
            o.stderr.trim()
        }
    );
    std::process::exit(unattended_exit(&verdict))
}

/// Run a probe against production and record it on the car — or refuse.
pub(crate) async fn run(
    car_ref: &str,
    probe: Option<String>,
    expect: Option<String>,
    exit_only: bool,
    verified: Option<String>,
    method: Option<String>,
    recheck: bool,
    replace: bool,
    dry: bool,
    from_car: bool,
    // The stated override for a probe this door refuses — see [`admit`].
    probe_anyway: Option<String>,
    // The operator's now, taken once at the CLI entry point and passed
    // in — the same shape `train::run` uses, so nothing down here reads
    // the wall clock on its own.
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let overriding = override_reason(probe_anyway.as_deref())?;
    let http = reqwest::Client::new();
    let base = crate::gate::resolve_jobs_base(None)?;
    let cars = crate::gate::all_cars_at(&http, &base).await?;
    // THE TREE THE PROBE'S READER COMES OUT OF (backlog 18fee481): the
    // worktree the operator is standing in, which carries
    // `infra/forge/probe-bin` by construction. Outside one there is no
    // reader to put on PATH — say so here rather than let the probe
    // report `boss-sor-read not found` as if the tool were missing from
    // the machine, which is the reading that sent a session looking for
    // an install that was never needed.
    let tree = crate::brief::repo_root().ok();
    if tree.is_none() {
        eprintln!(
            "boss prove: NOTE — this is not a checkout, so {PROBE_BIN} is not on the \
             probe's PATH and a probe that reads the system of record will report \
             `{reader}: command not found`. Run this from one.",
            reader = boss_jobs::probe::SOR_READER,
        );
    }
    // `--recheck` RE-RUNS a recorded probe and writes nothing, so a
    // finished car is a legitimate target for it and for nothing else.
    // Every other path records a proof, which a closed or cancelled car
    // cannot take — so those are not candidates, and naming them as
    // "open" is what sent an operator after a dead twin (0a58827d).
    let eligible = if recheck {
        Eligible::AnyStatus
    } else {
        Eligible::Live
    };
    let car = find_car(&cars, car_ref, eligible)?;
    let car_id = car
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("car has no id"))?
        .to_string();
    let short = &car_id[..8.min(car_id.len())];

    // --recheck reads the proof already on the step and re-runs it. It
    // is read-only on purpose: a decayed proof is a finding to act on,
    // not something to quietly overwrite.
    if recheck {
        let step = car
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|s| s.get("title").and_then(Value::as_str) == Some(PROVEN))
            .ok_or_else(|| anyhow::anyhow!("the car has no step titled {PROVEN:?}"))?;
        let rec = recorded_probe_for(car, step)?;
        let (probe, expect) = (rec.probe, rec.expect);
        let here = host();
        let dir_exists = rec.cwd.as_deref().is_none_or(|d| Path::new(d).is_dir());
        match rerunnable(rec.host.as_deref(), &here, rec.cwd.as_deref(), dir_exists) {
            Rerunnable::WrongHost { recorded, here } => bail!(
                "CANNOT RE-RUN HERE — this proof was recorded on {recorded}, and you are on \
                 {here}.\n  $ {probe}\n\n\
                 The claim has NOT been tested either way, which is a different fact from it \
                 having decayed. Reporting the two identically is how this instrument \
                 false-failed 3 times in 10 (66fd64c6) — probes reference paths, hostnames and \
                 services that exist on the host they were written for, so re-running one \
                 elsewhere tests something else and usually fails.\n  \
                 Re-run it on {recorded}, or re-prove the car here to record a probe that \
                 belongs to this host — and if its probe is host-INdependent, which a probe \
                 reading the system of record through the sanctioned reader is, \
                 `boss prove {short} --from-car` runs that same text here and records the \
                 proof: both doors now put the reader on PATH and export the same \
                 read-scoped identity (18fee481)."
            ),
            Rerunnable::MissingDir { cwd } => bail!(
                "CANNOT RE-RUN HERE — this proof was recorded in {cwd}, which does not exist \
                 on this machine.\n  $ {probe}\n\n\
                 A probe that opens `git rev-parse HEAD` means nothing outside a repository. \
                 The claim has not been tested either way."
            ),
            Rerunnable::Here => {}
        }
        // THE RULES APPLY TO A RE-RUN TOO, and this is the path that
        // most needs them: `--recheck` is the unattended re-run, so a
        // recorded probe that reads the system of record unidentified
        // would report a confident HOLDS against a narrowed world. The
        // refusal says so instead, and names `--replace` — which exists
        // to put a better probe under a claim already proven.
        if let Some(r) = admit(&probe, true).refusal {
            match overriding {
                None => bail!(
                    "CANNOT RE-CHECK THIS PROBE — {r}\n\n\
                     A recorded proof is not evidence of itself: re-prove the car with \
                     `--replace` and a probe that reads as a named reader, which leaves the \
                     original on the step where a reader can see what used to be claimed."
                ),
                Some(reason) => println!(
                    "boss prove: re-checking a probe this door refuses, on your stated \
                     reason — {reason}"
                ),
            }
        }
        if let Some(o) = &rec.overridden {
            println!(
                "boss prove: this proof was recorded OVER A REFUSAL — {}",
                serde_json::to_string(o).unwrap_or_else(|_| "(unreadable)".into())
            );
        }
        if let Source::Attempt { not_yet } = rec.source {
            println!(
                "boss prove: {short} is not proven — re-running the probe of its last \
                 attempt, which said {}",
                if not_yet { "NOT YET" } else { "NOT PROVEN" }
            );
        }
        println!("boss prove: re-running the recorded probe for {short}\n  $ {probe}");
        // The recorded cwd, and the car's own converged instant read
        // there — a re-run compares against the same fixed cutoff the
        // unattended door hands over, or the claim means a different
        // thing at each door (a92571a6).
        let cwd = rec.cwd.as_deref().filter(|d| !d.is_empty());
        let shell = Shell::here(cwd.map(Path::new))
            .with_probe_reader(tree.as_deref(), &base)
            .with_car_instant(car_merge_ref(car));
        // WHICH TREE THIS RE-RUN READS (backlog 6f581de6). A recheck
        // records nothing, so there is no immutable fact to refuse —
        // but its HOLDS / NO LONGER HOLDS is acted on by a human, and
        // acting on a verdict taken off an unnamed tree is the same
        // defect one step removed. So it is stated, never refused.
        println!(
            "{}",
            crate::freshness::unrecorded_tree_note(
                &probe_tree(&shell),
                crate::freshness::freshness_silenced(),
            )
        );
        let o = execute_with(&probe, &shell)?;
        // THREE READINGS, and which record they are read against
        // decides the sentence: a PROOF that fails now has decayed; an
        // ATTEMPT was never a proof. --recheck writes nothing on any of
        // them — `proven` stays exactly as it was (ready, for a not-yet
        // car), and a not-yet exits 75 so a script can tell.
        return match (verdict(&probe, &o, expect.as_deref()), rec.source) {
            (Verdict::Proven, Source::Proof) => {
                println!("boss prove: HOLDS — {short} is still true in production");
                Ok(())
            }
            (Verdict::Proven, Source::Attempt { .. }) => {
                println!(
                    "boss prove: NOW PROVABLE — {short}'s last attempt did not settle the \
                     claim and the same probe passes now. Nothing is recorded by --recheck; \
                     run boss prove {short} --from-car (or --probe/--expect) to record it \
                     and complete `{PROVEN}`."
                );
                Ok(())
            }
            (Verdict::NotYet { said }, _) => {
                println!("boss prove: {}", not_yet_why(&here, &said));
                println!("boss prove: --recheck records nothing; `{PROVEN}` stays as it is");
                std::process::exit(NOT_YET_EXIT)
            }
            // DID NOT RUN (46f67333): not decay, not "still not proven" —
            // the claim was not tested either way, which is the fact
            // 66fd64c6 taught this path to keep separate. Exit 3, as the
            // forge does.
            (Verdict::Unrunnable { missing }, _) => {
                println!("boss prove: {}", unrunnable_why(&here, &missing));
                println!("boss prove: --recheck records nothing; `{PROVEN}` stays as it is");
                std::process::exit(UNRUNNABLE_EXIT)
            }
            (Verdict::NotProven(e), Source::Proof) => bail!(
                "NO LONGER HOLDS — {short} was proven once and is not true now.\n\n{e}\n\n\
                 A proof can decay honestly: a step-plugin ConfigMap preview survives \
                 exactly until the next converge, and a car that never landed leaves \
                 prod looking fixed until it isn't."
            ),
            (Verdict::NotProven(e), Source::Attempt { .. }) => bail!(
                "STILL NOT PROVEN — {short}'s recorded probe does not pass now either.\n\n{e}"
            ),
        };
    }

    // --from-car: the probe the builder recorded at park time, and the
    // car's summary as the default prose. The clap-level conflicts
    // keep --probe/--expect/--exit-only off this path.
    let (probe, expect, verified) = if from_car {
        let (p, e) = car_probe(car)?;
        println!("boss prove: {short} runs the probe it carried from park time");
        (Some(p), Some(e), Some(car_verified(car, verified)?))
    } else {
        (probe, expect, verified)
    };
    let probe = probe.ok_or_else(|| {
        anyhow::anyhow!(
            "--probe is required: proof is a command that ran, not a sentence \
             (or --from-car, to run the probe the car recorded at park time)"
        )
    })?;
    let verified = verified.ok_or_else(|| {
        anyhow::anyhow!("--verified is required: say what the probe means, in prose, for a reader")
    })?;
    if expect.is_none() && !exit_only {
        bail!(
            "give --expect '<string the probe must print>', or pass --exit-only to assert \
             on the exit code alone.\n\n\
             Exit codes are a fine assertion when the command IS the test (`grep -q`, \
             `test -f`), and a weak one otherwise — `echo hi` exits 0. Downgrading is \
             allowed, but it is recorded in the proof so a reader can see what was \
             actually asserted."
        );
    }

    // Refuse before running anything: a car that has not merged should
    // cost a line of output, not a probe against production.
    let target = proven_step(car, replace)?;
    // ...and a --method the protocol does not accept is the same class
    // of refusal, for the same reason. It used to be caught only by the
    // API when the finished proof was recorded, so the probe ran, the
    // evidence was good, and it was discarded on a 400.
    if let Some(m) = method.as_deref() {
        check_enum_field(target, "method", m)?;
    }

    // A bare number is legal but weak, so say so BEFORE the probe runs
    // — the reader is looking at the output right then, which is the
    // only moment the better shape is cheap to adopt.
    if let Some(w) = expect.as_deref().and_then(bare_number_warning) {
        eprintln!("{w}");
    }

    // THE SAME RULES THE GATE APPLIES, AT THIS DOOR (23b2dffa), before
    // the probe runs and before anything is recorded.
    let admission = admit(&probe, from_car);
    for w in &admission.warnings {
        eprintln!("{w}");
    }
    let overridden = match (&admission.refusal, overriding) {
        (Some(r), None) => bail!("{r}"),
        (Some(r), Some(reason)) => {
            println!(
                "boss prove: running a probe this door refuses, on your stated reason — \
                 {reason}\n  It is recorded in the proof as `overridden`, so a later reader \
                 (and `--recheck`) sees the claim was argued past rather than clean."
            );
            Some(override_record(r.rule, reason))
        }
        (None, Some(_)) => {
            eprintln!(
                "boss prove: {OVERRIDE_FLAG} was given but nothing refused this probe — \
                 nothing is recorded as overridden."
            );
            None
        }
        (None, None) => None,
    };

    // WHICH TREE THIS PROBE IS ABOUT TO READ (backlog a09bd894), and a
    // refusal when it is not the one the forge reads. The first half of
    // this fix (18fee481) gave `--from-car` the forge's probe
    // ENVIRONMENT; this is the other half, because the two doors then
    // agreed about the environment and disagreed about the TREE — a
    // recorded probe reads it with `git show HEAD:<path>`, and on the
    // pod HEAD is whatever the checkout last fast-forwarded to, which
    // under load is trains behind (nine, measured 2026-09-22).
    //
    // Only `--from-car`: that flag exists so an operator can rehearse
    // what the forge will run, so it is the one door that PROMISES the
    // forge's reading. A hand-written `--probe` is the operator's own
    // text about their own tree, and refusing it would be this verb
    // deciding what their probe meant.
    let shell = Shell::here(None)
        .with_probe_reader(tree.as_deref(), &base)
        .with_car_instant(car_merge_ref(car));
    // OBSERVED ONCE, USED TWICE: the guard below judges it, and the
    // proof records it (backlog 6f581de6). Two readings could disagree
    // — a train lands between them — and a proof stamped with a tree
    // the guard did not judge is the same gap in a new place.
    let obs = probe_tree(&shell);
    // The shape only the tree can show, read where the probe will read
    // it (8ac42ee5) — said BEFORE the run, so a NOT YET that follows is
    // read beside the reason it will never clear.
    let dir = shell.cwd.as_deref().unwrap_or_else(|| Path::new("."));
    if let Some(w) = prose_only_warning(&probe, "HEAD", git_show_reader(dir, "HEAD")) {
        eprintln!("boss prove: {w}");
    }
    if from_car {
        match crate::freshness::stale_tree_guard(
            &obs,
            // A `--dry` run records nothing, so it is the rehearsal the
            // refusal leaves open rather than a write to refuse.
            !dry,
            crate::freshness::freshness_silenced(),
        ) {
            crate::freshness::BaseGuard::Note(n) => println!("{n}"),
            crate::freshness::BaseGuard::Refuse(why) => bail!("{why}"),
        }
    }

    println!("boss prove: {short}  $ {probe}");
    let o = execute_with(&probe, &shell)?;
    let at = now.to_rfc3339();
    match verdict(&probe, &o, expect.as_deref()) {
        Verdict::Proven => {}
        Verdict::NotProven(e) => return Err(e),
        // DID NOT RUN (46f67333): the channel named a tool this machine
        // lacks, so nothing about the claim was judged — not a verdict
        // against it, and not a proof. What lands is the forge's record
        // from this door: `proof_attempt{unrunnable:true, missing_tools}`
        // on the car, `proven` untouched, exit 3 as the forge exits.
        Verdict::Unrunnable { missing } => {
            let here = host();
            let why = unrunnable_why(&here, &missing);
            println!("boss prove: {why}");
            let step_status = target.get("status").and_then(Value::as_str).unwrap_or("?");
            if dry {
                println!(
                    "boss prove: DRY — would record this as proof_attempt on {short}; \
                     `{PROVEN}` stays {step_status}"
                );
                std::process::exit(UNRUNNABLE_EXIT);
            }
            let prior = car.pointer("/metadata/proof_attempt");
            let attempt = attempt_json(&probe, expect.as_deref(), &o, &here, &at, &why, prior);
            crate::gate::api(
                &http,
                reqwest::Method::PATCH,
                &format!("/api/jobs/{car_id}/metadata"),
                Some(json!({"proof_attempt": attempt})),
            )
            .await?;
            println!(
                "boss prove: proof_attempt recorded on {short}; `{PROVEN}` stays {step_status}"
            );
            std::process::exit(UNRUNNABLE_EXIT);
        }
        // NOT YET (726562de): the probe ran and said the claim cannot be
        // judged until something happens. Not a verdict against the
        // change, so nothing is refused — and not a proof, so nothing
        // completes. What lands is the forge's record, from this door:
        // `proof_attempt{not_yet:true}` on the car, `proven` untouched,
        // and the hourly recheck (recheck-failing-probes-hourly picks up
        // any car carrying an attempt) runs it again. Exit 75, as the
        // probe did and as the forge does.
        Verdict::NotYet { said } => {
            let here = host();
            let why = not_yet_why(&here, &said);
            println!("boss prove: {why}");
            let step_status = target.get("status").and_then(Value::as_str).unwrap_or("?");
            if dry {
                println!(
                    "boss prove: DRY — would record this as proof_attempt on {short}; \
                     `{PROVEN}` stays {step_status}"
                );
                std::process::exit(NOT_YET_EXIT);
            }
            let prior = car.pointer("/metadata/proof_attempt");
            let attempt = attempt_json(&probe, expect.as_deref(), &o, &here, &at, &why, prior);
            crate::gate::api(
                &http,
                reqwest::Method::PATCH,
                &format!("/api/jobs/{car_id}/metadata"),
                Some(json!({"proof_attempt": attempt})),
            )
            .await?;
            println!(
                "boss prove: proof_attempt recorded on {short}; `{PROVEN}` stays {step_status}"
            );
            std::process::exit(NOT_YET_EXIT);
        }
    }

    let proof = proof_json(
        &probe,
        expect.as_deref(),
        &o,
        &host(),
        &at,
        &obs,
        overridden.as_ref(),
    );
    let shown = o.stdout.trim();
    println!(
        "boss prove: probe exited 0{}\n  {}",
        match &expect {
            Some(w) => format!(" and printed {w:?}"),
            None => " (--exit-only, no output asserted)".into(),
        },
        if shown.is_empty() {
            "(no output)"
        } else {
            shown
        }
    );

    if dry {
        println!("boss prove: DRY — would record this proof on {short} and complete `{PROVEN}`");
        return Ok(());
    }

    // Re-read the step id from the car we already fetched.
    let sid = proven_step(car, replace)?
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("the `{PROVEN}` step has no id"))?
        .to_string();

    let md = proven_metadata(
        &verified,
        &serde_json::to_string(&proof)?,
        method.as_deref(),
        now,
    );

    if replace {
        // THE STEP IS FROZEN, SO THE NEW PROOF LANDS BESIDE IT. A
        // completed step cannot be rewritten — which is right, because
        // the original proof is evidence about the system and erasing
        // it would destroy the record of what used to hold. So the
        // replacement appends to `reproof` in JOB metadata, the same
        // door `regate_receipt` uses for a receipt whose branch moved,
        // and `--recheck` reads the newest entry in preference.
        let mut history: Vec<Value> = car
            .get("metadata")
            .and_then(|m| m.get("reproof"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        history.push(json!({
            "proof": serde_json::to_string(&proof)?,
            "verified": verified,
            "recorded_at": crate::gate::stamp(now),
        }));
        crate::gate::api(
            &http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{car_id}/metadata"),
            Some(json!({"reproof": history})),
        )
        .await?;
        println!(
            "boss prove: {short} re-proven — recorded as reproof #{}. The original \
             proof is untouched on the step; a proof that used to hold and no longer \
             does is evidence, not a mistake to erase.",
            history.len()
        );
        return Ok(());
    }

    crate::gate::api(
        &http,
        reqwest::Method::PUT,
        &format!("/api/jobs/{car_id}/steps/{sid}"),
        Some(json!({"status": "completed", "metadata": md})),
    )
    .await?;

    println!("boss prove: {short} proven — the probe is recorded and re-runnable");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(stdout: &str) -> Outcome {
        Outcome {
            exit: 0,
            stdout: stdout.into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        }
    }

    /// The tree observation for a test that is not about the tree. It
    /// is deliberately the UNREADABLE one rather than a current tree:
    /// a fixture must not hand a proof a standing nothing measured.
    fn no_tree() -> crate::freshness::TreeObservation {
        crate::freshness::TreeObservation::default()
    }

    /// THE RULE THE VERB EXISTS TO ENFORCE: a failing probe is not proof.
    #[test]
    fn a_nonzero_probe_is_refused_and_its_output_is_shown() {
        let o = Outcome {
            exit: 1,
            stdout: "nope".into(),
            stderr: "boom".into(),
            missing_tools: Vec::new(),
        };
        let e = judge(&o, None).unwrap_err().to_string();
        assert!(e.contains("exited 1"), "{e}");
        assert!(
            e.contains("boom"),
            "the refusal must quote the streams: {e}"
        );
    }

    /// A signalled probe must not be read as success.
    #[test]
    fn a_killed_probe_is_not_success() {
        let o = Outcome {
            exit: -1,
            stdout: String::new(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        };
        assert!(
            judge(&o, None).is_err(),
            "a signalled probe reports no code, not a pass"
        );
    }

    /// The case that motivated the verb: exit 0 proving nothing.
    #[test]
    fn exit_zero_without_the_expected_string_is_refused() {
        let e = judge(&ok("hi"), Some("MARKER")).unwrap_err().to_string();
        assert!(e.contains("never printed"), "{e}");
        assert!(
            e.contains("echo hi"),
            "the refusal should name the weakness: {e}"
        );
    }

    #[test]
    fn the_expected_string_may_arrive_on_stderr() {
        let o = Outcome {
            exit: 0,
            stdout: String::new(),
            stderr: "MARKER present".into(),
            missing_tools: Vec::new(),
        };
        assert!(
            judge(&o, Some("MARKER")).is_ok(),
            "real probes report on stderr too"
        );
    }

    #[test]
    fn a_matching_probe_passes() {
        assert!(judge(&ok("MARKER present"), Some("MARKER")).is_ok());
    }

    /// THE BUG (421b3032): `--expect "1"` accepted a probe that printed
    /// `10`, because the comparison was a substring test. The claim was
    /// true; the proof did not check what was meant. A numeric
    /// expectation must appear as a WHOLE token or it is not evidence.
    #[test]
    fn a_number_does_not_match_a_longer_number() {
        for printed in ["10", "21", "100", "0.15", "v1.2.3", "step_1a"] {
            let e = judge(&ok(printed), Some("1")).unwrap_err().to_string();
            assert!(
                e.contains("never printed"),
                "expect \"1\" must not match {printed:?}: {e}"
            );
            assert!(
                e.contains("whole"),
                "the refusal must say the number is compared as a whole token: {e}"
            );
        }
    }

    /// ...and it still matches when it really is the number printed.
    #[test]
    fn a_number_matches_when_it_stands_alone() {
        for printed in ["1", "1 rule", "count=1", "rules: 1", "1\n", "(1)"] {
            assert!(
                judge(&ok(printed), Some("1")).is_ok(),
                "expect \"1\" must match {printed:?}"
            );
        }
    }

    /// Only NUMERIC expectations tighten. A token expectation is still
    /// a substring test, which is what every good probe relies on.
    #[test]
    fn a_non_numeric_expectation_is_still_a_substring() {
        assert!(judge(&ok("prefix-claim:ok-suffix"), Some("claim:ok")).is_ok());
        assert!(judge(&ok("unprovenX"), Some("unproven")).is_ok());
    }

    /// The stronger rider: a bare number is a weak expectation even
    /// when it matches, so recording one warns and names the shape
    /// that cannot lie.
    #[test]
    fn a_bare_number_expectation_warns_at_record_time() {
        let w = bare_number_warning("1").expect("a bare number must warn");
        assert!(w.contains("WEAK"), "the warning must be loud: {w}");
        assert!(
            w.contains("-eq 1 && echo claim:ok"),
            "the warning must name the shape that cannot lie: {w}"
        );
        assert!(bare_number_warning("0.15").is_some());
        assert!(
            bare_number_warning("claim:ok").is_none(),
            "a token expectation is the good shape and must not warn"
        );
        assert!(bare_number_warning("1 rule").is_none());
    }

    /// The probe is actually EXECUTED, not parsed — this is what makes
    /// the recorded output evidence rather than transcription.
    #[test]
    fn execute_captures_what_really_happened() {
        let o = execute("printf hello; printf oops >&2; exit 3").unwrap();
        assert_eq!(o.exit, 3);
        assert_eq!(o.stdout, "hello");
        assert_eq!(o.stderr, "oops");
    }

    /// THE CAR CARRIES ITS PROBE (28ac45ab). `--from-car` reads the
    /// pair the auto-park handler copied from the gate's park intent,
    /// under the one set of keys core defines.
    #[test]
    fn from_car_reads_the_probe_the_car_recorded() {
        let car = serde_json::json!({
            "id": "c1", "metadata": {
                "summary": "The yard shows the probe count. And why.",
                "proof_probe": "curl -s http://sor/api/yard | grep -c unproven",
                "proof_expect": "unproven"
            }
        });
        let (p, e) = car_probe(&car).unwrap();
        assert_eq!(p, "curl -s http://sor/api/yard | grep -c unproven");
        assert_eq!(e, "unproven");
        // The prose defaults to the car's own claim, and an explicit
        // --verified still wins.
        assert_eq!(
            car_verified(&car, None).unwrap(),
            "The yard shows the probe count. And why."
        );
        assert_eq!(car_verified(&car, Some("seen".into())).unwrap(), "seen");
    }

    /// No probe recorded → a refusal that names the park-time door; an
    /// event-bound car → a refusal that quotes the event, so nobody
    /// runs "nothing" and records a yes.
    #[test]
    fn from_car_refuses_an_unprobed_or_event_bound_car() {
        let bare = serde_json::json!({"id": "c1", "metadata": {"summary": "x"}});
        let e = car_probe(&bare).unwrap_err().to_string();
        assert!(e.contains("--park-probe"), "{e}");

        let event = serde_json::json!({"id": "c1", "metadata": {
            "proof_event": "event-bound — the next yard-button cancel"
        }});
        let e = car_probe(&event).unwrap_err().to_string();
        assert!(e.contains("EVENT-BOUND"), "{e}");
        assert!(e.contains("yard-button cancel"), "quotes the event: {e}");

        // A probe with no expectation is refused here too — the gate
        // refuses the pair, so this is a hand-edited car.
        let half = serde_json::json!({"id": "c1", "metadata": {"proof_probe": "true"}});
        let e = car_probe(&half).unwrap_err().to_string();
        assert!(e.contains("echo hi"), "{e}");

        // And with no summary there is no default prose to fall back on.
        let e = car_verified(&half, None).unwrap_err().to_string();
        assert!(e.contains("--verified is required"), "{e}");
    }

    /// Guards the bug class that made a hand-rolled check lie on
    /// 2026-08-28: `grep -c || echo 0` prints "0\n0" on no match. Under
    /// this verb the same probe is refused, because grep's exit is 1.
    #[test]
    fn the_grep_c_fallback_that_lied_is_now_refused() {
        let o = execute("printf '' | grep -c MARKER || echo 0").unwrap();
        assert_eq!(
            o.stdout.trim(),
            "0\n0".trim_matches('"'),
            "reproduces the double zero"
        );
        // The honest form is what the verb pushes callers toward:
        let honest = execute("printf '' | grep -q MARKER").unwrap();
        assert!(
            judge(&honest, None).is_err(),
            "no match must refuse, not pass"
        );
    }

    #[test]
    fn a_proof_round_trips_through_the_recorded_form() {
        let o = ok("MARKER present");
        let p = proof_json(
            "grep -q MARKER f",
            Some("MARKER"),
            &o,
            "h",
            "2026-08-28T00:00:00Z",
            &no_tree(),
            None,
        );
        let step = json!({"metadata": {"proof": serde_json::to_string(&p).unwrap()}});
        let rec = recorded_probe(&step).unwrap();
        assert_eq!(rec.probe, "grep -q MARKER f");
        assert_eq!(rec.expect.as_deref(), Some("MARKER"));
    }

    /// A RECORDED PROOF SAYS WHICH TREE ANSWERED IT (backlog 6f581de6).
    /// `host` and `cwd` say WHERE it ran; a directory's contents change
    /// under it, and a recorded probe reads the tree with `git show
    /// HEAD:<path>` — so without the sha the artifact is incomplete in
    /// the one dimension this verb exists to close. Provenance is the
    /// first of the five properties, and the standing rides beside the
    /// sha because a later reader cannot re-derive it: `origin/main`
    /// has moved a hundred times by the time anyone reads the proof.
    #[test]
    fn a_recorded_proof_names_the_tree_it_read() {
        use crate::freshness::{Base, TreeObservation};
        let o = ok("MARKER present");
        let tree = TreeObservation {
            standing: Base::Behind,
            head: "1c63ca24ffff".into(),
            main_head: "de960a2affff".into(),
            behind_by: 9,
            unreadable: None,
        };
        let p = proof_json(
            "grep -q MARKER f",
            Some("MARKER"),
            &o,
            "h",
            "2026-09-22T00:00:00Z",
            &tree,
            None,
        );
        assert_eq!(p["tree_head"], json!("1c63ca24ffff"));
        assert_eq!(p["tree_standing"], json!("behind"));
        // BESIDE the existing keys, not instead of them: these eight
        // are what a live proof carried when this was measured, and
        // `--recheck` reads three of them back.
        for k in [
            "at", "cwd", "exit", "expect", "host", "probe", "stderr", "stdout",
        ] {
            assert!(p.get(k).is_some(), "{k} is a contract, not a detail: {p}");
        }
    }

    /// THE 932aa956 / 3f846cc5 CASE. A probe authored on the workstation
    /// as `ssh boss-gcp ...` re-runs ON boss-gcp, where that name does
    /// not resolve. The claim still held; the instrument was wrong.
    #[test]
    fn a_proof_recorded_elsewhere_cannot_be_rechecked_here() {
        assert_eq!(
            rerunnable(Some("mac-studio"), "boss-gcp", None, true),
            Rerunnable::WrongHost {
                recorded: "mac-studio".into(),
                here: "boss-gcp".into()
            }
        );
    }

    /// THE 64d5e3c7 CASE. The probe opened `git rev-parse --short HEAD`
    /// and was re-run outside any repository.
    #[test]
    fn a_proof_whose_directory_is_gone_cannot_be_rechecked() {
        assert_eq!(
            rerunnable(Some("h"), "h", Some("/var/lib/boss-train/repo"), false),
            Rerunnable::MissingDir {
                cwd: "/var/lib/boss-train/repo".into()
            }
        );
    }

    /// AND THE ONE THAT MUST NOT BREAK. Every proof already on the board
    /// was recorded without a `cwd`, and the oldest without a `host`.
    /// Refusing on absent context would turn a 30% false-alarm rate into
    /// a 100% one.
    #[test]
    fn absent_context_is_not_a_mismatch() {
        assert_eq!(rerunnable(None, "anywhere", None, true), Rerunnable::Here);
        assert_eq!(
            rerunnable(Some(""), "anywhere", None, true),
            Rerunnable::Here
        );
        assert_eq!(
            rerunnable(Some("unknown"), "anywhere", None, true),
            Rerunnable::Here
        );
        assert_eq!(
            rerunnable(Some("h"), "h", Some(""), false),
            Rerunnable::Here
        );
    }

    #[test]
    fn the_same_host_and_a_live_directory_re_runs() {
        assert_eq!(
            rerunnable(Some("h"), "h", Some("/tmp"), true),
            Rerunnable::Here
        );
    }

    /// The context is recorded so it can be read back — a field that is
    /// written but not recoverable is not a contract.
    #[test]
    fn the_recorded_context_round_trips() {
        let o = Outcome {
            exit: 0,
            stdout: "ok".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        };
        let proof = proof_json(
            "echo ok",
            None,
            &o,
            "somehost",
            "2026-08-29T00:00:00Z",
            &no_tree(),
            None,
        );
        let step = json!({"metadata": {"proof": proof.to_string()}});
        let rec = recorded_probe(&step).expect("readable");
        assert_eq!(rec.host.as_deref(), Some("somehost"));
        assert!(
            rec.cwd.is_some_and(|c| !c.is_empty()),
            "cwd must be recorded"
        );
    }

    #[test]
    fn a_step_proven_by_hand_has_nothing_to_recheck() {
        let step = json!({"metadata": {"verified": "trust me"}});
        let e = recorded_probe(&step).unwrap_err().to_string();
        assert!(e.contains("a claim with no probe under it"), "{e}");
    }

    fn car(id: &str, branch: &str, proven_status: &str) -> Value {
        json!({
            "id": id,
            "title": "a car",
            "metadata": {"branch": branch},
            "steps": [{"id": "s1", "title": PROVEN, "status": proven_status, "metadata": {}}],
        })
    }

    /// An unmerged car cannot be proven in production.
    #[test]
    fn a_pending_proven_step_is_refused_because_it_has_not_merged() {
        let c = car("11111111-a", "feat/x", "pending");
        let e = proven_step(&c, false).unwrap_err().to_string();
        assert!(e.contains("has not merged"), "{e}");
    }

    #[test]
    fn a_ready_proven_step_is_fillable() {
        assert!(proven_step(&car("11111111-a", "feat/x", "ready"), false).is_ok());
    }

    /// A car at a named status — the shape the jobs API returns, which
    /// `car()` predates.
    fn car_at(id: &str, branch: &str, status: &str) -> Value {
        let mut c = car(id, branch, "ready");
        c["status"] = json!(status);
        c
    }

    /// Ambiguity is refused rather than resolved — the failure mode that
    /// cost two cars when `boss park` picked from a list instead.
    #[test]
    fn two_matching_cars_are_refused_not_chosen() {
        let cars = vec![
            car("11111111-aaa", "feat/x", "ready"),
            car("11111111-bbb", "feat/x", "ready"),
        ];
        let e = find_car(&cars, "feat/x", Eligible::Live)
            .unwrap_err()
            .to_string();
        assert!(e.contains("2 cars match"), "{e}");
    }

    /// THE DEFECT (backlog 0a58827d). `boss prove` refused with "2 open
    /// cars match" when only one was open: the other had been abandoned
    /// ("gated, then changed"). A closed car cannot take a new proof, so
    /// it was never a candidate — and with the dead twin gone the case
    /// resolves to one car and needs no refusal at all. The refusal sent
    /// an operator to investigate a dead car.
    #[test]
    fn a_closed_twin_is_not_a_candidate_for_a_new_proof() {
        let cars = vec![
            car_at("ef4eff3c-aaa", "feat/arrival-runs-the-probe-rerail", "open"),
            car_at(
                "538775dd-bbb",
                "feat/arrival-runs-the-probe-rerail",
                "closed",
            ),
        ];
        let got = find_car(&cars, "feat/arrival-runs-the-probe-rerail", Eligible::Live)
            .expect("one live car is not ambiguous");
        assert_eq!(got.get("id").and_then(Value::as_str), Some("ef4eff3c-aaa"));
    }

    /// A cancelled car is as finished as a closed one — answered by
    /// `boss_jobs::car::is_open`, so this verb carries no second reading
    /// of `status` to drift from it.
    #[test]
    fn a_cancelled_twin_is_not_a_candidate_either() {
        let cars = vec![
            car_at("ef4eff3c-aaa", "feat/x", "open"),
            car_at("538775dd-bbb", "feat/x", "cancelled"),
        ];
        assert!(find_car(&cars, "feat/x", Eligible::Live).is_ok());
    }

    /// And when the ONLY match is finished, the refusal says so. The
    /// bare "no open car — check your spelling" would send the operator
    /// hunting a typo that is not there.
    #[test]
    fn the_only_match_being_closed_is_said_out_loud() {
        let cars = vec![car_at("538775dd-bbb", "feat/x", "closed")];
        let e = find_car(&cars, "feat/x", Eligible::Live)
            .unwrap_err()
            .to_string();
        assert!(e.contains("538775dd"), "names the car: {e}");
        assert!(e.contains("closed"), "names its status: {e}");
        assert!(
            e.contains("--recheck"),
            "names what a closed car can do: {e}"
        );
    }

    /// `--recheck` re-runs a probe already recorded on the car, which is
    /// a read — so a closed car IS its candidate, and that is why the
    /// reader pages over closed cars at all.
    #[test]
    fn a_recheck_can_still_name_a_closed_car() {
        let cars = vec![car_at("538775dd-bbb", "feat/x", "closed")];
        assert!(find_car(&cars, "feat/x", Eligible::AnyStatus).is_ok());
    }

    /// A listing is not homogeneous, so one collective word cannot
    /// describe it: two finished matches can be finished DIFFERENTLY.
    /// Each row carries its own status, which is what makes "say which"
    /// answerable without a second query — and is why the refusal no
    /// longer prefixes the count with a single "open" / "closed".
    #[test]
    fn a_refusal_labels_each_candidates_status() {
        let cars = vec![
            car_at("ef4eff3c-aaa", "feat/x", "cancelled"),
            car_at("538775dd-bbb", "feat/x", "closed"),
        ];
        let e = find_car(&cars, "feat/x", Eligible::Live)
            .unwrap_err()
            .to_string();
        assert!(
            !e.contains("cars match"),
            "nothing is a candidate, so nothing is ambiguous: {e}"
        );
        assert!(e.contains("ef4eff3c  cancelled"), "{e}");
        assert!(e.contains("538775dd  closed"), "{e}");
    }

    /// A row whose status is absent is NOT assumed finished. Dropping a
    /// candidate the code cannot see the state of would be guessing in
    /// the direction that loses cars.
    #[test]
    fn a_car_with_no_status_stays_a_candidate() {
        let cars = vec![car("abcdef12-3456", "feat/x", "ready")];
        assert!(find_car(&cars, "feat/x", Eligible::Live).is_ok());
    }

    /// The live match is PREFERRED for a recheck too — the behaviour
    /// that landed before this change and must not regress. A recheck
    /// admits a finished car, but when the name also matches a live one
    /// that is the one meant, so the abandoned twin does not turn a
    /// resolvable recheck into an ambiguity.
    #[test]
    fn a_recheck_prefers_the_live_car_over_an_abandoned_twin() {
        let mut abandoned = car("22222222-bbb", "feat/x", "ready");
        abandoned["status"] = json!("closed");
        abandoned["metadata"]["skip_reason"] =
            json!("gate receipt is for fb973bd0 but the branch boards 0ec4521f");
        let cars = vec![car("11111111-aaa", "feat/x", "ready"), abandoned];
        let found = find_car(&cars, "feat/x", Eligible::AnyStatus)
            .expect("the one live car is the candidate");
        assert_eq!(
            found.get("id").and_then(Value::as_str),
            Some("11111111-aaa")
        );
    }

    /// And the refusal is not regressed: two cars that are genuinely
    /// open still refuse, and still name both so "say which" is
    /// answerable.
    #[test]
    fn two_genuinely_open_cars_still_refuse_and_list_both() {
        let cars = vec![
            car_at("11111111-aaa", "feat/x", "open"),
            car_at("22222222-bbb", "feat/x", "open"),
        ];
        let e = find_car(&cars, "feat/x", Eligible::Live)
            .unwrap_err()
            .to_string();
        assert!(e.contains("2 cars match"), "{e}");
        assert!(e.contains("11111111  open"), "{e}");
        assert!(e.contains("22222222  open"), "{e}");
    }

    /// And when the ambiguity is among finished cars only — which only a
    /// recheck can reach — no row is called open.
    #[test]
    fn an_ambiguity_among_closed_cars_is_not_called_open() {
        let cars = vec![
            car_at("11111111-aaa", "feat/x", "closed"),
            car_at("22222222-bbb", "feat/x", "closed"),
        ];
        let e = find_car(&cars, "feat/x", Eligible::AnyStatus)
            .unwrap_err()
            .to_string();
        assert!(e.contains("2 cars match"), "{e}");
        assert!(e.contains("11111111  closed"), "{e}");
        assert!(!e.contains("open"), "{e}");
    }

    #[test]
    fn a_car_is_found_by_branch_or_by_id_prefix() {
        let cars = vec![car("abcdef12-3456", "feat/x", "ready")];
        assert!(find_car(&cars, "feat/x", Eligible::Live).is_ok());
        assert!(find_car(&cars, "abcdef12", Eligible::Live).is_ok());
        // Too short to be an id, and not a branch: refused, not guessed.
        assert!(find_car(&cars, "abc", Eligible::Live).is_err());
    }

    #[test]
    fn a_long_stream_is_clipped_rather_than_pushed_into_metadata() {
        let clipped = clip(&"x".repeat(MAX_STREAM + 500));
        assert!(clipped.len() < MAX_STREAM + 100);
        assert!(clipped.contains("more bytes"));
    }

    #[test]
    fn a_recorded_proof_says_when_it_was_taken() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-29T04:15:09.847213Z")
            .unwrap()
            .into();
        let md = proven_metadata("v", "{\"exit\":0}", Some("api"), now);

        assert_eq!(
            md["completed_at"], "2026-08-29T04:15:09Z",
            "proof with no timestamp cannot be aged, so a --recheck cannot \
             tell a proof taken minutes ago from one taken in June"
        );
        // The stamp must match the conductor's format byte for byte —
        // proof lag is this minus `review.completed_at`.
        assert_eq!(md["completed_at"], json!(crate::gate::stamp(now)));
        // And it does not disturb what the step already carried.
        assert_eq!(md["verified"], json!("v"));
        assert_eq!(md["proof"], json!("{\"exit\":0}"));
        assert_eq!(md["method"], json!("api"));
    }

    /// The refusal happens BEFORE the probe runs, and it reads the
    /// step's own declared vocabulary rather than a copy in this file
    /// (9a: it already lives in workflows.toml and the registry
    /// default). 2026-09-02: two proof probes ran green against
    /// production and were thrown away when the API refused a
    /// free-prose --method, because nothing checked it locally.
    #[test]
    fn a_bad_method_is_refused_against_the_steps_own_vocabulary() {
        let step = json!({"fields": [
            {"name": "method", "field_type": "browser|api|log", "required": false}
        ]});
        assert!(check_enum_field(&step, "method", "api").is_ok());
        let e = check_enum_field(&step, "method", "ran it live")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("browser | api | log"),
            "names the vocabulary: {e}"
        );
        assert!(
            e.contains("BEFORE the probe runs"),
            "says why it refuses early: {e}"
        );
    }

    /// A field the step does not declare, or one that is free text, is
    /// not this verb's business to constrain — it must not invent a
    /// contract the protocol never stated.
    #[test]
    fn an_undeclared_or_free_text_field_is_not_constrained() {
        let free = json!({"fields": [{"name": "note", "field_type": "string"}]});
        assert!(check_enum_field(&free, "note", "anything at all").is_ok());
        assert!(check_enum_field(&json!({}), "method", "whatever").is_ok());
    }

    #[test]
    fn an_omitted_method_stays_omitted() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-29T04:15:09Z")
            .unwrap()
            .into();
        let md = proven_metadata("", "{}", None, now);
        assert!(md.get("method").is_none(), "an absent method is not `null`");
        assert!(md["completed_at"].is_string());
    }

    /// A COMPLETED STEP IS CLOSED TO A NEW PROOF, AND OPEN TO A BETTER
    /// ONE. Car 932aa956's probe observed a real production refusal and
    /// stopped holding within the hour because the condition was
    /// transient — `--recheck` said NO LONGER HOLDS, correctly, and was
    /// wrong about the cause. Without --replace the only route to a
    /// durable probe was an operator editing job metadata by hand.
    #[test]
    fn a_proven_step_reopens_only_for_a_replacement() {
        let done = car("11111111-a", "feat/x", "completed");
        assert!(
            proven_step(&done, false).is_err(),
            "an ordinary prove must not overwrite a recorded proof"
        );
        assert!(
            proven_step(&done, true).is_ok(),
            "--replace is how a transient proof gets a better probe"
        );
    }

    /// ...and the refusal now says how, rather than only saying no.
    #[test]
    fn the_refusal_names_replace_as_the_way_forward() {
        let e = proven_step(&car("11111111-a", "feat/x", "completed"), false)
            .unwrap_err()
            .to_string();
        assert!(e.contains("--recheck"), "{e}");
        assert!(e.contains("--replace"), "{e}");
    }

    /// A REPLACEMENT SUPERSEDES THE ORIGINAL FOR RE-RUNNING, and the
    /// original is still on the step — the same precedence
    /// `regate_receipt` has over a stale gate receipt. Which proof
    /// `--recheck` should re-run is a different question from which one
    /// happened, and both answers are kept.
    #[test]
    fn recheck_re_runs_the_newest_replacement() {
        let o = Outcome {
            exit: 0,
            stdout: String::new(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        };
        let first = proof_json(
            "old-probe",
            None,
            &o,
            "h",
            "2026-08-29T00:00:00Z",
            &no_tree(),
            None,
        );
        let better = proof_json(
            "better-probe",
            None,
            &o,
            "h",
            "2026-08-30T00:00:00Z",
            &no_tree(),
            None,
        );
        let step = json!({"metadata": {"proof": first.to_string()}});
        let with_reproof = json!({"metadata": {"reproof": [
            {"proof": better.to_string(), "recorded_at": "2026-08-30T00:00:00Z"}
        ]}});
        let plain = json!({"metadata": {}});

        assert_eq!(
            recorded_probe_for(&plain, &step).expect("readable").probe,
            "old-probe",
            "with no replacement, the step's own proof is what re-runs"
        );
        assert_eq!(
            recorded_probe_for(&with_reproof, &step)
                .expect("readable")
                .probe,
            "better-probe",
            "a replacement is what --recheck should be re-running"
        );
    }

    /// THE FALSE NEGATIVE THAT GREW WITH THE PIPELINE'S AGE.
    ///
    /// The car read was one `?kind=ship-a-change&limit=200`. Once more
    /// than 200 closed cars had accumulated, a legitimately open,
    /// unproven car sorting after them fell off that single page, and
    /// `find_car` reported "no open ship-a-change car" for a car that
    /// plainly existed. Paging on `total` must reach it — and must keep
    /// reading closed cars too, because `--recheck` proves a CLOSED one,
    /// which is why the fix is not a `status=open` filter.
    ///
    /// The stub honours `limit`/`offset` and reports the true `total`,
    /// so the reader is exercised across page boundaries with no
    /// `BOSS_JOBS_URL` anywhere in the environment.
    /// A page with no `total` is refused, never read as the whole
    /// population (backlog 10776b6c). The read counted a missing total
    /// as 0, so one page of rows already "covered" it and the loop
    /// stopped there — a car past page one was reported as no car, the
    /// false negative the paging above exists to prevent.
    #[tokio::test]
    async fn a_car_page_without_a_total_is_refused_not_read_as_all() {
        let (base, stub) = crate::gate::stub::one_request(
            r#"{"data":[{"id":"00000000-0000-0000-0000-0000000000cc","status":"closed"}]}"#,
        )
        .await;
        let http = reqwest::Client::new();
        let why = crate::gate::all_cars_at(&http, &base)
            .await
            .expect_err("a page without its total cannot say the read is complete")
            .to_string();
        stub.abort();
        assert!(why.contains("total"), "{why}");
    }

    #[tokio::test]
    async fn a_car_past_the_first_page_is_still_reachable() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Enough closed cars to overflow more than one page, then one
        // OPEN car dead last — only a full paging read finds it.
        let needle = "fix/needle-in-the-haystack";
        let mut all: Vec<Value> = (0..1100)
            .map(|i| {
                json!({
                    "id": format!("{i:08}-0000-0000-0000-0000000000cc"),
                    "status": "closed",
                    "title": format!("closed car {i}"),
                    "metadata": {"branch": format!("fix/closed-{i}")},
                })
            })
            .collect();
        all.push(json!({
            "id": "ffffffff-0000-0000-0000-0000000000ff",
            "status": "open",
            "title": "the open one",
            "metadata": {"branch": needle},
        }));
        let total = all.len();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let served = all.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                let target = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let param = |k: &str| -> Option<usize> {
                    target
                        .split(['?', '&'])
                        .find_map(|p| p.strip_prefix(&format!("{k}=")))
                        .and_then(|v| v.parse().ok())
                };
                let limit = param("limit").unwrap_or(100).max(1);
                let offset = param("offset").unwrap_or(0);
                let page: Vec<Value> = served.iter().skip(offset).take(limit).cloned().collect();
                let body = json!({"data": page, "total": served.len()}).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let base = format!("http://{addr}");
        let http = reqwest::Client::new();
        let cars = crate::gate::all_cars_at(&http, &base)
            .await
            .expect("paging read succeeds");
        assert_eq!(
            cars.len(),
            total,
            "every page must be read, not just the first {}",
            cars.len()
        );
        let car = find_car(&cars, needle, Eligible::Live)
            .expect("the open car past page 1 must be found");
        assert_eq!(
            car.get("metadata")
                .and_then(|m| m.get("branch"))
                .and_then(Value::as_str),
            Some(needle)
        );
    }

    // ------------------------------------------------------------------
    // ONE RULE SET, BOTH DOORS (backlog 23b2dffa). `boss gate
    // --park-probe` refuses a probe naming a tool the forge lacks, and
    // one reading the system of record unidentified. This verb — the
    // HAND door onto the same recorded probe — applied neither, and
    // said so only in a comment describing the gate's refusal.
    // ------------------------------------------------------------------

    /// THE RULE THAT HOLDS ON EVERY HOST. An unidentified read of the
    /// system of record is answered with a NARROWER WORLD, silently, so
    /// an absence assertion passes falsely — measured twice in one hour
    /// on 2026-09-10 (61085a9e), once on a probe that would have
    /// recorded a green proof of nothing. That is true wherever the
    /// probe runs, so the hand verb refuses it too, and names the way
    /// out rather than leaving the operator to argue with a wall.
    #[test]
    fn a_probe_that_reads_the_sor_unidentified_is_refused_by_the_hand_verb() {
        for probe in [
            "curl -fsS $BOSS_JOBS_URL/api/jobs?kind=pr-train&status=open | grep -q b9302f90",
            "curl -sf http://boss-jobs-internal.boss.svc.cluster.local:7900/api/yard/status | grep -q trains",
            "wget -qO- http://10.20.0.34:7900/api/stations/loading-dock/queue | grep -q x",
        ] {
            let a = admit(probe, false);
            let r = a
                .refusal
                .unwrap_or_else(|| panic!("admitted: {probe}"))
                .text;
            assert!(r.contains("unidentified"), "{r}");
            assert!(
                r.contains(OVERRIDE_FLAG),
                "the refusal must name its override: {r}"
            );
        }
    }

    /// The same refusal on the path that runs the probe the CAR
    /// recorded: a car's `proof_probe` can be written by hand with a
    /// metadata PATCH, so it never had to meet a gate.
    #[test]
    fn a_car_carried_probe_that_reads_the_sor_unidentified_is_refused_too() {
        let a = admit(
            "curl -fsS $BOSS_JOBS_URL/api/yard/status | grep -q dock_depth",
            true,
        );
        assert!(
            a.refusal.is_some(),
            "a car-carried probe gets the same rule"
        );
    }

    /// A GIT DATE READ WITH ITS OFFSET IS REFUSED HERE TOO (c0ac92b8):
    /// the string compare it feeds lies in both directions, so this
    /// door — hand-run or `--from-car` — says what the gate says, from
    /// the one evidence text, and names its OWN rule id so an override
    /// is recorded against the rule that refused, not the other one.
    #[test]
    fn a_probe_that_reads_a_git_date_with_an_offset_is_refused_by_the_hand_verb() {
        let probe = "since=$(git log -1 --format=%cI HEAD); \
                     last=$(boss-api GET /api/audit | jq -r '.data[0].at'); \
                     [ \"$last\" \\> \"$since\" ] && echo retire:after-landing";
        for from_car in [false, true] {
            let a = admit(probe, from_car);
            let r = a
                .refusal
                .unwrap_or_else(|| panic!("admitted (from_car={from_car}): {probe}"));
            assert_eq!(r.rule, boss_jobs::probe::GIT_TIME_RULE);
            assert!(r.text.contains("`%cI`"), "{}", r.text);
            assert!(r.text.contains("--format=%ct"), "{}", r.text);
            assert!(
                r.text.contains(boss_jobs::probe::GIT_TIME_STRING_EVIDENCE),
                "{}",
                r.text
            );
            assert!(r.text.contains(OVERRIDE_FLAG), "{}", r.text);
        }
        // And the unidentified read still records ITS rule.
        let a = admit(
            "curl -fsS $BOSS_JOBS_URL/api/yard/status | grep -q x",
            false,
        );
        assert_eq!(a.refusal.map(|r| r.rule), Some(UNIDENTIFIED_RULE));
    }

    /// AND THE RULE THAT DOES NOT. `host-absent-tools.txt` says what the
    /// FORGE lacks, because a `--park-probe` runs there. This verb runs
    /// the probe HERE, in front of the operator, one second later — and
    /// a pod-local `kubectl` proof is the established shape for a claim
    /// only the cluster can show, so refusing it would block exactly the
    /// case where a human has to prove what the forge cannot.
    #[test]
    fn the_hand_verb_does_not_judge_a_hand_probe_against_the_forges_tools() {
        let a = admit(
            "kubectl -n boss get deploy boss-jobs -o json | grep -q boss-ci",
            false,
        );
        assert!(a.refusal.is_none(), "{:?}", a.refusal);
        assert!(a.warnings.is_empty(), "{:?}", a.warnings);
    }

    /// But a probe that came from the CAR has a second destination — the
    /// arrival rule runs that same text on the forge — so the mismatch
    /// is named. Named, not refused: proving it by hand here is the
    /// right move, and the car's recorded probe is what needs fixing.
    #[test]
    fn a_car_carried_probe_the_forge_cannot_run_is_named_but_not_refused() {
        let a = admit(
            "kubectl -n boss get deploy boss-jobs -o json | grep -q boss-ci",
            true,
        );
        assert!(a.refusal.is_none(), "{:?}", a.refusal);
        let w = a
            .warnings
            .first()
            .expect("the forge cannot run this car's probe");
        assert!(w.contains("kubectl"), "{w}");
        assert!(w.contains("forge"), "{w}");
    }

    /// A car-carried probe that ASSIGNS AN ACTOR is named at this door
    /// too (8a1fcd22): the forge will run that text, and an actor is the
    /// one thing that would let its `boss` verbs write. Named, not
    /// refused, for the same reason as the absent tool above — the hand
    /// run here is the operator's, already signed as them; what needs
    /// fixing is the probe the car recorded, which `boss gate` refuses.
    #[test]
    fn a_car_carried_probe_that_names_an_actor_is_named_but_not_refused() {
        let a = admit(
            "BOSS_ACTOR=emp-david boss job close 1234 && echo closed",
            true,
        );
        assert!(a.refusal.is_none(), "{:?}", a.refusal);
        let w = a.warnings.first().expect("the actor assignment is named");
        assert!(w.contains("BOSS_ACTOR"), "{w}");
        assert!(w.contains("does not act"), "{w}");
        let hand = admit(
            "BOSS_ACTOR=emp-david boss job close 1234 && echo closed",
            false,
        );
        assert!(hand.warnings.is_empty(), "{:?}", hand.warnings);
    }

    /// A car-carried probe that `cd`s is named here too (4bb6797c): on
    /// this pod `cd /work/boss` works, so the hand run is fine and is
    /// not refused — but the forge has no such path, so the recorded
    /// text is what needs re-parking, and `boss gate` refuses it. A
    /// probe typed by hand (`--probe`) is the operator's own shell and
    /// is not this rule's business.
    #[test]
    fn a_car_carried_probe_that_changes_directory_is_named_but_not_refused() {
        let probe = "cd /work/boss && git show HEAD:x | grep -q y && echo ok";
        let a = admit(probe, true);
        assert!(a.refusal.is_none(), "{:?}", a.refusal);
        let w = a.warnings.first().expect("the cd is named");
        assert!(w.contains("`cd`"), "{w}");
        assert!(w.contains("converged checkout"), "{w}");
        assert!(admit(probe, false).warnings.is_empty());
    }

    /// A named read is not this rule's business, and a MENTION is not a
    /// read — the shared command-position scan is what keeps both legal.
    #[test]
    fn named_reads_and_mere_mentions_are_admitted() {
        for probe in [
            "boss-sor-read /api/yard/status | grep -q dock_depth",
            "boss-api GET /api/jobs?kind=pr-train | grep -q arrived",
            "grep -c BOSS_JOBS_URL infra/ops/ops-runner.sh",
            "test -n \"$BOSS_JOBS_URL\" && echo claim-ok",
        ] {
            let a = admit(probe, true);
            assert!(a.refusal.is_none(), "{probe}: {:?}", a.refusal);
        }
    }

    // -----------------------------------------------------------------
    // THE PROBE THAT COULD NOT PASS (backlog 4fccc595)
    // -----------------------------------------------------------------

    /// jq is a declared required tool of the CI image
    /// (infra/forge/boss-ci/required-tools.txt), and this section is
    /// about jq's own exit codes — so its absence is a failure, never a
    /// skip. A suite that passes by skipping is the check-nobody-reads
    /// defect with extra steps.
    fn require_jq() {
        let ok = std::process::Command::new("sh")
            .arg("-c")
            .arg("command -v jq >/dev/null")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(
            ok,
            "jq is missing and this test would otherwise pass by skipping — \
             jq is declared in infra/forge/boss-ci/required-tools.txt"
        );
    }

    /// The shape off car a0ab90a5, reduced to one claim so the test needs
    /// no network: `if <claim> then empty else error(…) end` under
    /// `jq -e`. The claim HOLDS in this fixture.
    const INVERTED_FILTER: &str = "printf '%s' '{\"category\":\"platform\"}' | \
         jq -e 'if (.category==\"platform\") then empty else error(\"category=\\(.category)\") end'";

    /// THE ANCHOR (4fccc595). Eighteen hours were spent on a probe that
    /// reported failure precisely when its claim held, and the record
    /// said "not holding, or the probe is wrong" over an empty output and
    /// a rewritten exit code. Three things must now be true of that
    /// exact shape: the door NAMES it, the recorded attempt carries jq's
    /// OWN exit status, and the verdict names ONE thing.
    #[test]
    fn the_inverted_jq_e_probe_is_named_and_its_real_exit_is_recorded() {
        require_jq();

        // 1. The door says so, before anything runs — and warns rather
        //    than refuses, because this shape cannot record a false
        //    proof (see `admit`).
        let a = admit(INVERTED_FILTER, false);
        assert!(a.refusal.is_none(), "{:?}", a.refusal);
        let w = a
            .warnings
            .iter()
            .find(|w| w.contains("jq -e"))
            .unwrap_or_else(|| panic!("the inverted shape is not named: {:?}", a.warnings));
        assert!(w.contains("empty"), "{w}");
        assert!(w.contains("4"), "the warning must name jq's exit 4: {w}");

        // 2. THE CLAIM HOLDS AND THE PROBE FAILS, with jq's own 4 —
        //    the number that identifies the defect. Recorded verbatim.
        let o = execute(INVERTED_FILTER).unwrap();
        assert_eq!(
            o.exit, 4,
            "jq -e exits 4 when its filter produces no output; `empty` produces none"
        );
        let p = proof_json(
            INVERTED_FILTER,
            Some("claim:ok"),
            &o,
            "h",
            "now",
            &no_tree(),
            None,
        );
        assert_eq!(p["exit"], 4, "the proof record carries the REAL status");
        assert!(
            p.get("stderr").is_some(),
            "stderr is recorded as its own field"
        );

        // 3. And the verdict names the self-contradiction instead of
        //    offering the reader two possibilities.
        let e = judge_probe(INVERTED_FILTER, &o, Some("claim:ok"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("exited 4"), "{e}");
        assert!(
            e.contains("SELF-CONTRADICTORY"),
            "the verdict must name what failed: {e}"
        );
        assert!(
            e.contains("4fccc595"),
            "and carry the measured evidence: {e}"
        );
    }

    /// THE OTHER HALF OF THE SAME PROBE: jq's stderr, which exists the
    /// moment the claim actually fails. `error(…)` prints to stderr and
    /// exits 5 — a DIFFERENT number from the 4 above, which is the whole
    /// reason a rewritten exit code costs a diagnosis.
    #[test]
    fn jq_names_the_failing_row_on_stderr_and_exits_five() {
        require_jq();
        let failing = "printf '%s' '{\"category\":\"a sentence\"}' | \
             jq -e 'if (.category==\"platform\") then empty else \
             error(\"maintenance-backup: category=\\(.category)\") end'";
        let o = execute(failing).unwrap();
        assert_eq!(o.exit, 5, "jq exits 5 when the filter calls error()");
        assert!(
            o.stderr.contains("maintenance-backup: category=a sentence"),
            "jq's own diagnosis, captured: {:?}",
            o.stderr
        );
        let p = proof_json(failing, Some("claim:ok"), &o, "h", "now", &no_tree(), None);
        assert!(
            p["stderr"].as_str().unwrap().contains("maintenance-backup"),
            "and recorded: {p}"
        );
    }

    /// `|| exit 1` IS WHY THE NUMBER WAS GONE. The same filter with the
    /// trailing bare exit records a 1, and 4 and 5 — two different
    /// causes — become the same digit. The diagnosis says so, naming the
    /// reduction rather than leaving a reader to find it.
    #[test]
    fn a_bare_exit_rewrite_is_named_as_the_reason_the_status_is_gone() {
        require_jq();
        let rewritten = format!("{INVERTED_FILTER} || exit 1");
        let o = execute(&rewritten).unwrap();
        assert_eq!(o.exit, 1, "jq's 4 is gone — this is the measured record");
        let d = failure_diagnosis(&rewritten, &o).expect("a failure this shape is diagnosable");
        assert!(d.contains("SELF-CONTRADICTORY"), "{d}");
        assert!(
            d.contains("|| exit 1"),
            "the reduction that erased jq's status must be named: {d}"
        );
        let a = admit(&rewritten, false);
        assert!(
            a.warnings.iter().any(|w| w.contains("|| exit")),
            "the door names it too: {:?}",
            a.warnings
        );
    }

    /// A FAILURE THAT CANNOT BE READ is its own finding. Exit code, both
    /// streams empty, nothing to go on — the verdict must say THAT,
    /// rather than asserting which of several causes it was.
    #[test]
    fn a_failure_with_no_output_at_all_is_named_as_unreadable() {
        let o = Outcome {
            exit: 1,
            stdout: String::new(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        };
        let d = failure_diagnosis("some-command --quiet", &o).expect("silence is diagnosable");
        assert!(
            d.contains("printed NOTHING"),
            "the reader must be told the record is empty, not left to infer it: {d}"
        );
        assert!(
            d.contains("not a verdict on the claim"),
            "an unreadable failure is not evidence against the claim: {d}"
        );
    }

    /// And an honest failure — a probe that ran, disagreed, and SAID so —
    /// gets no invented diagnosis. A verdict that speculates on every
    /// failure is noise, and noise is what gets ignored.
    #[test]
    fn a_probe_that_explains_its_own_failure_is_left_alone() {
        let o = Outcome {
            exit: 1,
            stdout: "CLAIM FAILS for maintenance-backup (jq exit 5)".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        };
        assert!(
            failure_diagnosis(
                "boss-sor-read /api/x | grep -q y || { echo x; exit 1; }",
                &o
            )
            .is_none(),
            "the probe already named what failed"
        );
        // Nor on a pass: `judge_probe` adds nothing when there is
        // nothing to explain.
        assert!(judge_probe("true", &ok("claim:ok"), Some("claim:ok")).is_ok());
    }

    // -----------------------------------------------------------------
    // A PROBE THAT CRASHED IS NOT A PROBE THAT SAID NOT-YET (68081368)
    // -----------------------------------------------------------------

    /// THE MEASURED RECORD, twice (cars dd1d872d and 2e4d3bce,
    /// 2026-09-14): a probe read a count with jq, jq printed `null`,
    /// bash's `[` refused to compare it and exited 2, and the `||`
    /// branch behind it said "not yet" and exited 75. The verdict
    /// recorded "said the claim cannot be judged until something
    /// happens" — the not-yet sentence, over stderr that held the crash.
    /// The reader had to open stderr to learn the probe had never
    /// judged anything.
    const CRASHED_STDERR: &str = "bash: line 10: [: null: integer expression expected\n";
    const NULL_COUNT_PROBE: &str = "n=$(boss-sor-read /api/jobs?kind=x | jq -r .total); \
         if [ \"$n\" -ge 1 ]; then echo x:ok; else echo 'not yet: none'; exit 75; fi";

    fn crashed(exit: i32, stderr: &str) -> Outcome {
        Outcome {
            exit,
            stdout: "not yet: none\n".into(),
            stderr: stderr.into(),
            missing_tools: Vec::new(),
        }
    }

    #[test]
    fn a_probe_that_crashed_comparing_a_non_number_has_the_crash_named() {
        let o = crashed(75, CRASHED_STDERR);
        let d = failure_diagnosis(NULL_COUNT_PROBE, &o).expect("a crash is diagnosable");
        assert!(
            d.contains("THE PROBE CRASHED"),
            "the crash comes first: {d}"
        );
        assert!(
            d.contains("[: null: integer expression expected"),
            "the stderr line is quoted, not pointed at: {d}"
        );
        assert!(
            d.contains("// empty") && d.contains("*[!0-9]*"),
            "and the guard to add is named: {d}"
        );
        assert!(
            !d.contains("cannot be judged"),
            "a crash is not the not-yet sentence: {d}"
        );
        // The door's refusal carries it too — `judge_probe` is the one
        // path every probe run goes through.
        let e = judge_probe(NULL_COUNT_PROBE, &o, Some("x:ok"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("THE PROBE CRASHED"), "{e}");
    }

    /// THE MEASURED c4c1ac17 STDERR (backlog 302bc2f2). Its stored probe
    /// carried backslash-escaped quotes, so under sh `\"boss` was the
    /// PATTERN and `step` and `complete"` were FILENAMES. grep warned
    /// about both and exited 2, the probe's own `case` arm caught the
    /// non-number and exited 75, and the runner read 75 as an honest
    /// wait. `recheck-failing-probes-hourly` then re-ran a probe that
    /// could never pass, hourly, each run producing the same reassuring
    /// sentence. Nothing was ever judged about the claim — which was, as
    /// it happens, already true on main.
    const MANGLED_ARGS_STDERR: &str = "\
ugrep: warning: step: No such file or directory\n\
ugrep: warning: complete\": No such file or directory\n";

    #[test]
    fn a_probe_whose_quoting_was_mangled_is_not_read_as_an_honest_wait() {
        // Exit 75 is the probe's own not-yet code, and that is the whole
        // trap: this must NOT come back as NotYet.
        let o = crashed(75, MANGLED_ARGS_STDERR);
        let d = failure_diagnosis(NULL_COUNT_PROBE, &o)
            .expect("a mangled argument list is diagnosable");
        assert!(d.contains("QUOTING"), "the cause is named: {d}");
        assert!(
            d.contains("complete\": No such file or directory"),
            "the stderr line is quoted, not pointed at: {d}"
        );
        assert!(!d.contains("not yet"), "a broken probe is not a wait: {d}");
        // The verdict is the thing that mattered: 75 with a diagnosis is
        // NotProven, so the shed renders it troubled instead of waiting.
        assert!(
            matches!(
                verdict(NULL_COUNT_PROBE, &o, Some("x:ok")),
                Verdict::NotProven(_)
            ),
            "exit 75 with mangled arguments must not read as NotYet"
        );
    }

    /// Every phrasing a tool uses for a missing operand is the same
    /// finding, not only the one measured — a fourth is a line in
    /// `MISSING_OPERAND_MARKERS`, and this walks the list so the list is
    /// what gets tested, at every exit code a `||` branch could turn it
    /// into. The quote in the name is what makes each one damage rather
    /// than a wait, so each marker is checked both ways.
    #[test]
    fn every_missing_operand_marker_is_damage_only_when_the_name_carries_a_quote() {
        for marker in MISSING_OPERAND_MARKERS {
            for exit in [1, 2, 75] {
                let mangled = format!("grep: complete\": {marker}\n");
                let d = failure_diagnosis(NULL_COUNT_PROBE, &crashed(exit, &mangled))
                    .unwrap_or_else(|| panic!("{marker:?} at exit {exit} is mangled quoting"));
                assert!(d.contains("QUOTING"), "{marker:?}: {d}");
                let honest = format!("grep: newfile.txt: {marker}\n");
                assert!(
                    failure_diagnosis(NULL_COUNT_PROBE, &crashed(exit, &honest)).is_none(),
                    "{marker:?} without a quote in the name is an honest wait"
                );
            }
        }
    }

    /// The rule is narrow ON PURPOSE. A probe that greps a file which is
    /// not on main yet is an HONEST not-yet, and its stderr says "No such
    /// file or directory" too. What is never honest is a missing file
    /// whose NAME carries a double quote: no probe greps such a file, so
    /// the quote is the residue of an extra escaping layer.
    #[test]
    fn a_plainly_missing_file_is_still_an_honest_wait() {
        let o = crashed(75, "grep: newfile.txt: No such file or directory\n");
        assert!(
            failure_diagnosis(NULL_COUNT_PROBE, &o).is_none(),
            "a file that has simply not landed yet is not a mangled probe"
        );
        assert!(
            matches!(
                verdict(NULL_COUNT_PROBE, &o, Some("x:ok")),
                Verdict::NotYet { .. }
            ),
            "and it still reads as the honest wait it is"
        );
    }

    /// Every message bash's `[` and `((` print for a non-number is a
    /// crash, not only the one measured. A fourth one is a line in
    /// `NUMERIC_CRASH_MARKERS`, and this test walks the list so the
    /// list is what gets tested — at every exit code a `||` branch
    /// could turn the crash into.
    #[test]
    fn every_numeric_crash_marker_is_named_whatever_the_exit_code() {
        for marker in NUMERIC_CRASH_MARKERS {
            let stderr = format!("bash: line 3: [: : {marker}\n");
            for exit in [1, 2, 75] {
                let d = failure_diagnosis(NULL_COUNT_PROBE, &crashed(exit, &stderr))
                    .unwrap_or_else(|| panic!("{marker:?} at exit {exit} is a crash"));
                assert!(d.contains("THE PROBE CRASHED"), "{marker}: {d}");
                assert!(d.contains(marker), "the line is quoted: {d}");
            }
        }
    }

    /// A CLEAN NOT-YET IS LEFT ALONE BY THE DIAGNOSER. Exit 75, a "not
    /// yet:" line, an empty stderr — the probe said what it meant, and
    /// `failure_diagnosis` invents nothing for it; `verdict` reads it as
    /// NOT YET precisely BECAUSE there is no diagnosis (726562de), the
    /// same order the forge's verdict block uses. Nor does an ordinary
    /// failure — a curl refusal on stderr — read as a crash.
    #[test]
    fn a_clean_exit_75_gets_no_crash_diagnosis() {
        let o = Outcome {
            exit: 75,
            stdout: "not yet: no disk-report request carrying for_sweep yet\n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        };
        assert!(failure_diagnosis(NULL_COUNT_PROBE, &o).is_none());
        let o = Outcome {
            exit: 1,
            stdout: String::new(),
            stderr: "curl: (7) Failed to connect\n".into(),
            missing_tools: Vec::new(),
        };
        assert!(failure_diagnosis(NULL_COUNT_PROBE, &o).is_none());
    }

    // -----------------------------------------------------------------
    // --replace WITH NOTHING TO REPLACE (backlog 251dba77)
    // -----------------------------------------------------------------

    /// THE MEASURED DEFECT. `boss prove <car> --replace` on a car whose
    /// `proven` step had never been proven printed "re-proven — recorded
    /// as reproof #1. The original proof is untouched on the step" —
    /// two assertions, both false — and left the step `ready`, so the
    /// car never advanced and nothing said so.
    #[test]
    fn replace_refuses_when_there_is_no_proof_to_replace() {
        let car = json!({"steps": [{
            "id": "s1", "title": PROVEN, "status": "ready",
            "metadata": {"authority_role": "platform-admin", "procedure": "boss prove"}
        }]});
        let e = proven_step(&car, true).unwrap_err().to_string();
        assert!(
            e.contains("no recorded proof"),
            "it must say what is missing: {e}"
        );
        assert!(e.contains("ready"), "and that the step has not moved: {e}");
        assert!(
            e.contains("WITHOUT --replace"),
            "and name the plain form, which is what records a FIRST proof: {e}"
        );
        // The plain form is unaffected: this is the path that records it.
        assert!(proven_step(&car, false).is_ok());
    }

    /// AND THE FLAG STILL DOES WHAT IT WAS BUILT FOR (2b30eff4): a
    /// completed step's proof is frozen, and `--replace` is how a better
    /// probe lands beside it without erasing what used to hold.
    #[test]
    fn replace_still_works_on_a_step_whose_proof_is_frozen() {
        let proven = json!({"steps": [{
            "id": "s1", "title": PROVEN, "status": "completed",
            "metadata": {"verified": "it held", "proof": "{\"exit\":0}"}
        }]});
        assert!(
            proven_step(&proven, true).is_ok(),
            "a completed step is exactly what --replace is for"
        );
        assert!(
            proven_step(&proven, false)
                .unwrap_err()
                .to_string()
                .contains("--replace"),
            "and the plain form still points at it"
        );
    }

    /// A step that is OPEN but already carries a proof is the one
    /// non-obvious case: the plain form would overwrite the recorded
    /// proof, which is the erasure 2b30eff4 exists to prevent — so
    /// `--replace` is accepted there and appends instead.
    #[test]
    fn replace_is_accepted_on_an_open_step_that_does_carry_a_proof() {
        let car = json!({"steps": [{
            "id": "s1", "title": PROVEN, "status": "ready",
            "metadata": {"proof": "{\"exit\":0,\"probe\":\"true\"}"}
        }]});
        assert!(proven_step(&car, true).is_ok());
    }

    /// AN OVERRIDE NOBODY CAN SEE IS THE SAME DEFECT AGAIN. The escape
    /// hatch is recorded IN the proof — the one record `--recheck` and
    /// every later reader already open — and absent entirely when it was
    /// not used, so its presence means something.
    #[test]
    fn an_overridden_refusal_is_recorded_in_the_proof() {
        let ov = override_record(
            UNIDENTIFIED_RULE,
            "the identity header comes from my shell profile",
        );
        let p = proof_json(
            "curl $BOSS_JOBS_URL/api/x",
            Some("x"),
            &ok("x"),
            "h",
            "now",
            &no_tree(),
            Some(&ov),
        );
        assert_eq!(p["overridden"]["rule"], UNIDENTIFIED_RULE);
        assert_eq!(
            p["overridden"]["reason"],
            "the identity header comes from my shell profile"
        );
        let plain = proof_json("true", Some("x"), &ok("x"), "h", "now", &no_tree(), None);
        assert!(
            plain.get("overridden").is_none(),
            "no override, no key — a reader must not read one to learn nothing"
        );
    }

    // -----------------------------------------------------------------
    // THE HAND DOOR CAN SAY NOT YET (backlog 726562de)
    // -----------------------------------------------------------------

    /// THE MEASURED ASYMMETRY. The forge runner records a clean exit 75
    /// as NOT YET — the probe said the claim cannot be judged until
    /// something happens — leaves `proven` ready and re-runs it daily.
    /// `boss prove <car> --from-car` on the SAME run read "A probe that
    /// fails is evidence AGAINST the claim": the opposite verdict for
    /// one record, depending on which door ran it. Exit 75 is a third
    /// answer, and this door now gives it.
    #[test]
    fn an_exit_75_with_a_not_yet_line_is_not_yet_not_evidence_against() {
        let o = Outcome {
            exit: NOT_YET_EXIT,
            stdout: "not yet: no disk-report request carrying for_sweep yet\n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        };
        let Verdict::NotYet { said } = verdict(NULL_COUNT_PROBE, &o, Some("sweep-measured:ok"))
        else {
            panic!("a clean exit 75 is NOT YET, not a refusal");
        };
        assert_eq!(
            said,
            "not yet: no disk-report request carrying for_sweep yet"
        );
        let why = not_yet_why("pod", &said);
        assert!(why.starts_with("NOT YET"), "{why}");
        assert!(
            why.contains(&said),
            "the probe's own line is the reason: {why}"
        );
        assert!(
            !why.contains("evidence AGAINST") && !why.contains("not proof of anything"),
            "not-yet is not a verdict against the change: {why}"
        );
        assert_eq!(
            NOT_YET_EXIT, 75,
            "the verb exits as the probe did, so scripts can tell"
        );
    }

    /// A CRASH THAT EXITED 75 IS THE CRASH (68081368), at this door as
    /// at the forge: the code is whichever `||` branch caught it, and
    /// re-running a crash daily judges nothing forever.
    #[test]
    fn an_exit_75_whose_stderr_shows_a_crash_is_the_crash_not_not_yet() {
        let o = crashed(75, CRASHED_STDERR);
        let Verdict::NotProven(e) = verdict(NULL_COUNT_PROBE, &o, Some("x:ok")) else {
            panic!("a crashed numeric test is not not-yet");
        };
        let e = e.to_string();
        assert!(e.contains("THE PROBE CRASHED"), "{e}");
        assert!(!e.contains("cannot be judged"), "{e}");
    }

    /// AND AN EXIT 75 THAT SAID NOTHING CANNOT BE READ — the forge's
    /// `why` checks the silent nonzero exit before it reads the code,
    /// and this door keeps the same order: not-yet is what a probe SAYS,
    /// not a number it exits with.
    #[test]
    fn an_exit_75_that_said_nothing_cannot_be_read() {
        let o = Outcome {
            exit: 75,
            stdout: String::new(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        };
        let Verdict::NotProven(e) = verdict("some-command --quiet", &o, Some("x:ok")) else {
            panic!("silence is a missing record, not a not-yet");
        };
        assert!(e.to_string().contains("THE FAILURE CANNOT BE READ"), "{e}");
    }

    /// The other two readings are unchanged by the third.
    #[test]
    fn the_other_two_readings_are_unchanged() {
        assert!(matches!(
            verdict("true", &ok("x:ok"), Some("x:ok")),
            Verdict::Proven
        ));
        let red = Outcome {
            exit: 1,
            stdout: "CLAIM FAILS (jq exit 5)\n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        };
        let Verdict::NotProven(e) = verdict("true", &red, Some("x:ok")) else {
            panic!("exit 1 is still a refusal");
        };
        assert!(e.to_string().contains("evidence AGAINST"), "{e}");
    }

    /// WHAT THE PROBE SAID, read as the forge reads it: the first
    /// non-empty line of stderr, else of stdout — stderr first because
    /// that is where a tool puts its diagnosis.
    #[test]
    fn what_it_said_reads_stderr_first_then_stdout() {
        let o = Outcome {
            exit: 75,
            stdout: "\n  not yet: from stdout\nmore\n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        };
        assert_eq!(what_it_said(&o).as_deref(), Some("not yet: from stdout"));
        let o = Outcome {
            exit: 75,
            stdout: "not yet: from stdout\n".into(),
            stderr: "\ncurl: (7) refused\n".into(),
            missing_tools: Vec::new(),
        };
        assert_eq!(what_it_said(&o).as_deref(), Some("curl: (7) refused"));
        let o = Outcome {
            exit: 75,
            stdout: "  \n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        };
        assert_eq!(what_it_said(&o), None);
    }

    // -----------------------------------------------------------------
    // THE UNRUNNABLE PRELUDE, AT THIS DOOR (backlog 46f67333)
    // -----------------------------------------------------------------

    /// THE MEASURED ASYMMETRY. The forge runner runs every probe behind
    /// a `command_not_found_handle` writing to fd 9 and records DID NOT
    /// RUN with the tool named; this door ran `sh -c` and read the same
    /// run as CANNOT BE READ or NOT PROVEN — a verdict about the
    /// operator's machine recorded as a verdict about the claim. Now the
    /// probe runs behind the forge's own prelude here too, so a tool the
    /// PATH lacks is a finding on the record, not a red.
    /// THE MANGLED-QUOTING CASE (backlog 302bc2f2, measured 2026-09-20).
    /// Two shed cars carried a probe stored with LITERAL backslash-quotes,
    /// so `grep -c \"boss step complete\"` made `step` and `complete\"`
    /// FILENAMES. grep warned, the count came back non-numeric, the
    /// `case` arm caught it, and the probe exited 75 with a sentence that
    /// read exactly like an honest wait — hourly, for days, while both
    /// claims were already TRUE on main.
    #[test]
    fn a_probe_whose_quoting_was_mangled_is_not_a_wait() {
        let probe = concat!(
            r#"n=$(echo hi | grep -c \"pub enum StepAction\" || true); "#,
            r#"case ${n:-empty} in empty|*[!0-9]*) echo \"not yet: cannot read the file\"; "#,
            "exit 75;; esac; echo ok"
        );
        let o = execute(probe).unwrap();
        assert_eq!(o.exit, NOT_YET_EXIT, "the case arm catches it: {o:?}");

        let d = failure_diagnosis(probe, &o)
            .expect("a probe the shell could not read as written has no verdict to give");
        assert!(d.starts_with("THE PROBE'S QUOTING WAS MANGLED"), "{d}");
        assert!(
            d.contains("--park-probe-file") || d.contains("single-quote"),
            "the diagnosis names the repair: {d}"
        );

        // …and because a diagnosis exists, the not-yet arm cannot claim it.
        assert!(
            !matches!(verdict(probe, &o, Some("ok")), Verdict::NotYet { .. }),
            "a mangled probe read as an honest wait is the whole defect: {o:?}"
        );
    }

    /// THE SHED, AS IT ACTUALLY READ (backlog 302bc2f2, 2026-09-20).
    /// These are the not-yet lines the thirteen cars awaiting proof
    /// really recorded. Eleven are honest waits and ELEVEN OF THEM
    /// carry a `\"` somewhere in their probe — inside a jq filter,
    /// where it belongs — which is exactly why "the probe text contains
    /// an escape" is the wrong detector and was my first, wrong count.
    /// Not one of them may be diagnosed.
    #[test]
    fn the_honest_waits_the_shed_really_recorded_are_not_diagnosed() {
        for said in [
            "not yet: the newest run ed96b6e3 was dispatched before this car converged",
            "not yet: no publish-github-pr request answered with an exit",
            "not yet: no tenant.published in the audit tail; none lands until a publish",
            "not yet: no build step nominated to the executor since convergence",
            "not yet: no backlog-item filed by the rule carrying design 0e07ce64",
            "not yet: no tag-release ops-request has been answered",
            "not yet: no green after a red at the same head opened since convergence",
            "not yet: no answered sweep-archive-branches request opened after convergence",
            "not yet: no real prune since convergence",
            "not yet: no sponsorship polled since convergence",
            "not yet: no sponsorship packet opened since the change converged",
        ] {
            // A jq filter's own escaped quotes, which are correct and
            // must not by themselves condemn the probe.
            let probe = format!(
                r#"row=$(printf '%s' "$body" | jq -r ".data[]|select(.k==\"x\")"); echo '{said}'; exit 75"#
            );
            let o = execute(&probe).unwrap();
            assert_eq!(o.exit, NOT_YET_EXIT, "{said}");
            assert!(
                quoting_was_mangled(&probe, &o).is_none(),
                "an honest wait was condemned: {said}\n{o:?}"
            );
            assert!(
                matches!(verdict(&probe, &o, Some("token")), Verdict::NotYet { .. }),
                "an honest wait must stay a wait: {said}"
            );
        }
    }

    /// The guard on the guard: a CORRECTLY quoted probe that says not-yet
    /// keeps saying not-yet. Every honest wait in the shed looks like
    /// this, and 9 of the 13 measured cars were exactly this.
    #[test]
    fn a_correctly_quoted_not_yet_is_still_a_wait() {
        let probe = "echo 'not yet: no tag-release ops-request has been answered'; exit 75";
        let o = execute(probe).unwrap();
        assert_eq!(o.exit, NOT_YET_EXIT);
        assert!(
            failure_diagnosis(probe, &o).is_none(),
            "a clean probe must not be diagnosed: {o:?}"
        );
        let Verdict::NotYet { said } = verdict(probe, &o, Some("token")) else {
            panic!("an honest wait stays a wait: {o:?}")
        };
        assert!(said.contains("no tag-release"), "{said}");
    }

    #[test]
    fn a_probe_naming_a_tool_the_path_lacks_did_not_run() {
        let stub = boss_testing::scratch::scratch_dir("prove-stub-path");
        let probe = format!(
            "export PATH={}; kubectl get pods -A && echo pods:ok",
            stub.display()
        );
        let o = execute(&probe).unwrap();
        assert_eq!(
            o.missing_tools,
            vec!["kubectl".to_string()],
            "the channel names the tool bash could not resolve: {o:?}"
        );
        assert!(
            o.stderr.contains("kubectl: command not found"),
            "bash's own message still reaches stderr: {}",
            o.stderr
        );
        let Verdict::Unrunnable { missing } = verdict(&probe, &o, Some("pods:ok")) else {
            panic!("a missing tool is DID NOT RUN, not a verdict on the claim: {o:?}")
        };
        assert_eq!(missing, vec!["kubectl".to_string()]);
        let why = unrunnable_why("pod-7", &missing);
        assert!(
            why.starts_with("THE PROBE DID NOT RUN on pod-7: kubectl not found."),
            "{why}"
        );
        assert!(
            !why.contains("CANNOT BE READ") && !why.contains("NOT PROVEN"),
            "the two verdicts about the claim must not appear: {why}"
        );
        let a = attempt_json(&probe, Some("pods:ok"), &o, "pod-7", "now", &why, None);
        assert_eq!(a["unrunnable"], true);
        assert_eq!(a["missing_tools"], json!(["kubectl"]));
        assert_eq!(a["not_yet"], false);
    }

    /// A probe whose tools are present is judged exactly as before: the
    /// channel is empty, and the two rules decide.
    #[test]
    fn a_probe_whose_tools_are_present_is_judged_as_today() {
        let green = execute("echo pods:ok").unwrap();
        assert!(green.missing_tools.is_empty(), "{green:?}");
        assert!(matches!(
            verdict("echo pods:ok", &green, Some("pods:ok")),
            Verdict::Proven
        ));
        let red = execute("echo CLAIM FAILS; exit 1").unwrap();
        assert!(red.missing_tools.is_empty(), "{red:?}");
        let Verdict::NotProven(e) = verdict("echo CLAIM FAILS; exit 1", &red, Some("pods:ok"))
        else {
            panic!("a false claim is still NOT PROVEN")
        };
        assert!(
            !e.to_string().contains("DID NOT RUN"),
            "a false claim must not look unrunnable: {e}"
        );
        // A NOT-YET probe is still read as not-yet, prelude and all.
        let ny = execute("echo 'not yet: nothing filed'; exit 75").unwrap();
        assert!(matches!(
            verdict("true", &ny, Some("x:ok")),
            Verdict::NotYet { .. }
        ));
    }

    /// The channel is read the way the runner reads it — `sort -u` —
    /// so a tool a probe fails to find three times is named once, and
    /// the empty lines a `>>` append can leave are not tools.
    #[test]
    fn missing_tools_is_the_channel_sorted_and_deduplicated() {
        assert_eq!(
            missing_tools("kubectl\nboss\nkubectl\n\nboss\n"),
            vec!["boss".to_string(), "kubectl".to_string()]
        );
        assert!(missing_tools("").is_empty());
        assert!(missing_tools("\n\n").is_empty());
    }

    /// `--recheck` ON A NOT-YET CAR RE-RUNS THE ATTEMPT'S PROBE. The
    /// step is still `ready` and carries no `proof`, so the old reader
    /// refused with "nothing to re-run" — but the car holds exactly what
    /// ran, in `proof_attempt`, and re-running it is what "not yet"
    /// asks for. The record's provenance rides along so the report says
    /// "now provable", never "still HOLDS", about a claim never proven.
    #[test]
    fn a_recheck_of_a_not_yet_car_re_runs_the_attempts_probe() {
        let car = json!({"metadata": {"proof_attempt": {
            "at": "2026-09-14T00:00:25Z", "exit": 75, "not_yet": true, "unrunnable": false,
            "probe": "n=$(boss-sor-read /api/x | jq -r '.total // empty'); echo x:ok",
            "expect": "x:ok", "host": "david-asus-minipc",
            "why": "NOT YET: the probe ran on david-asus-minipc and said …",
            "stdout": "not yet: none", "stderr": "", "missing_tools": []
        }}});
        let step = json!({"title": PROVEN, "status": "ready", "metadata": {}});
        let rec = recorded_probe_for(&car, &step).expect("the attempt is what re-runs");
        assert!(rec.probe.starts_with("n=$(boss-sor-read"), "{}", rec.probe);
        assert_eq!(rec.expect.as_deref(), Some("x:ok"));
        assert_eq!(rec.host.as_deref(), Some("david-asus-minipc"));
        assert!(
            rec.cwd.is_none(),
            "the forge's attempt records no cwd; absent is not a mismatch"
        );
        assert_eq!(rec.source, Source::Attempt { not_yet: true });

        // A step that DOES carry a proof still wins — an attempt is the
        // record of a run that settled nothing, not a replacement.
        let proven = json!({"metadata": {"proof": proof_json(
            "real-probe", Some("x"), &ok("x"), "h", "now", &no_tree(), None).to_string()}});
        let rec = recorded_probe_for(&car, &proven).unwrap();
        assert_eq!(rec.probe, "real-probe");
        assert_eq!(rec.source, Source::Proof);

        // And a car with neither still says so.
        let bare = json!({"metadata": {}});
        let e = recorded_probe_for(&bare, &step).unwrap_err().to_string();
        assert!(e.contains("nothing to re-run"), "{e}");
    }

    // -----------------------------------------------------------------
    // THE UNATTENDED DOOR (backlog 9f00a805, consolidation H8 car 2):
    // what infra/forge/run-car-probe.sh did that this file did not,
    // each a gap measured before the twin was retired, each pinned.
    // -----------------------------------------------------------------

    /// THE PRELUDE IS ONE DEFINITION AND IT WORKS: a probe that pipes
    /// its own stderr into a `grep -q` still cannot hide which tool was
    /// missing (f9304366 — the shape the first two arrival probes had).
    /// This ran out of the shell twin's markers in boss-testing's
    /// run_car_probe_sh.rs until the twin was retired; it runs the
    /// door itself now.
    #[test]
    fn a_missing_tool_is_named_even_when_the_probe_swallows_its_own_stderr() {
        let o = execute("kubectl-no-such-tool -n boss get pods 2>&1 | grep -q Running")
            .expect("bash runs");
        assert_eq!(
            o.missing_tools,
            vec!["kubectl-no-such-tool".to_string()],
            "fd 9 must name the tool the probe's own redirection hid: {o:?}"
        );
        assert_ne!(o.exit, 0);
        // And the usual message is still printed, on the stream the
        // probe redirected — nothing is taken away.
        let o = execute("kubectl-no-such-tool get pods").expect("bash runs");
        assert!(
            o.stderr.contains("kubectl-no-such-tool: command not found"),
            "{o:?}"
        );
        assert_eq!(o.exit, 127);
        // A runnable probe leaves the channel empty.
        let o = execute("printf 'claim:ok\\n'").expect("bash runs");
        assert!(o.missing_tools.is_empty(), "{o:?}");
        assert_eq!(o.exit, 0);
    }

    /// THE SHELL'S ARGV, in the twin's order: `timeout -k 5 <secs>`,
    /// then `runuser -u <user> --` only when this process is root, then
    /// `bash -c` with the prelude ahead of the probe's text. The hand
    /// door is the bare `bash -c`.
    #[test]
    fn the_unattended_shell_runs_under_timeout_and_drops_to_the_probe_user() {
        let shell = Shell {
            cwd: Some("/home/david/boss".into()),
            user: Some("david".into()),
            timeout_secs: Some(60),
            env: Vec::new(),
            strip: Vec::new(),
            path_prefix: None,
        };
        let as_root = shell.command_line("echo x", true);
        assert_eq!(
            &as_root[..8],
            &["timeout", "-k", "5", "60", "runuser", "-u", "david", "--"]
        );
        assert_eq!(&as_root[8..10], &["bash", "-c"]);
        assert!(as_root[10].contains("command_not_found_handle()"));
        assert!(as_root[10].ends_with("\necho x"), "{}", as_root[10]);
        // Not root: the twin's by-hand path, as this user, still timed.
        let as_user = shell.command_line("echo x", false);
        assert_eq!(&as_user[..6], &["timeout", "-k", "5", "60", "bash", "-c"]);
        // The hand door: no timeout, no user.
        let hand = Shell::here(None).command_line("echo x", true);
        assert_eq!(&hand[..2], &["bash", "-c"]);
        assert_eq!(hand.len(), 3);
    }

    /// A PROBE THAT OUTLIVES ITS TIMEOUT IS KILLED AND SAYS SO: exit
    /// 124 from `timeout`, and the record's stderr carries the note the
    /// twin appended, where the probe's own last words are.
    #[test]
    fn a_probe_that_outlives_the_timeout_is_killed_and_the_record_says_so() {
        let shell = Shell {
            timeout_secs: Some(1),
            ..Shell::here(None)
        };
        let o = execute_with("sleep 30; echo never", &shell).expect("bash runs");
        assert_eq!(o.exit, TIMEOUT_EXIT, "{o:?}");
        assert!(o.stderr.contains("killed at 1s timeout"), "{o:?}");
        assert!(!o.stdout.contains("never"));
        // Under the timeout the exit is the probe's own.
        let o = execute_with("echo fast:ok", &shell).expect("bash runs");
        assert_eq!(o.exit, 0);
        assert!(o.stdout.contains("fast:ok"));
    }

    /// THE PROBE'S ENVIRONMENT IS THE ONE IT WAS PROMISED and nothing
    /// of this verb's own: `env` set, `strip` removed, `path_prefix`
    /// first on PATH, and the run in `cwd`. The reader identity a
    /// recorded probe's `boss-sor-read` sends is exactly the one
    /// `Shell::unattended` builds.
    #[test]
    fn the_unattended_shell_hands_the_probe_its_env_and_strips_the_verbs_actor() {
        let dir = boss_testing::scratch::scratch_dir("prove-unattended-env");
        let bin = dir.join("pbin");
        std::fs::create_dir_all(&bin).unwrap();
        boss_testing::write_exec(&bin.join("tool-on-prefix"), "#!/bin/sh\necho prefix:ok\n");
        let shell = Shell {
            cwd: Some(dir.clone()),
            user: None,
            timeout_secs: None,
            env: vec![
                ("BOSS_JOBS_URL".into(), "http://sor.invalid:7900".into()),
                ("BOSS_SOR_USER".into(), reader_header(READER_ACTOR)),
                ("BOSS_SOR_PORTS".into(), "jobs=7900 events=7150".into()),
            ],
            strip: vec!["BOSS_PROVE_TEST_LEAK".into()],
            path_prefix: Some(bin),
        };
        let o = execute_with(
            "tool-on-prefix; printf '[%s][%s][%s][%s]\\n' \"$BOSS_JOBS_URL\" \"$BOSS_SOR_PORTS\" \
             \"${BOSS_PROVE_TEST_LEAK:-}\" \"$PWD\"; printf '%s\\n' \"$BOSS_SOR_USER\"",
            &shell,
        )
        .expect("bash runs");
        assert_eq!(o.exit, 0, "{o:?}");
        assert!(o.stdout.contains("prefix:ok"), "{o:?}");
        assert!(
            o.stdout.contains(&format!(
                "[http://sor.invalid:7900][jobs=7900 events=7150][][{}]",
                dir.display()
            )),
            "{o:?}"
        );
        let user: Value = serde_json::from_str(o.stdout.lines().last().unwrap()).unwrap();
        assert_eq!(user["id"], READER_ACTOR);
        assert_eq!(user["role"], READER_ROLE);
        assert_eq!(user["access_tier"], "auditor");
    }

    /// THE HAND DOOR RUNS THE SAME TEXT IN THE SAME ENVIRONMENT
    /// (backlog 18fee481, measured 2026-09-22). `--from-car` exists so
    /// an operator can rehearse locally what the forge will run, and
    /// its own help says so — but it ran that text with the operator's
    /// PATH and nothing else, while the unattended door put
    /// `infra/forge/probe-bin` first and exported the reader's identity
    /// and port table. So the one flag whose purpose is to make the two
    /// doors agree was the one that did not: every car whose probe
    /// reads the system of record answered
    /// `boss-sor-read not found` on the pod, and the shed sat at 8 of
    /// 12 cars open past 24 h with the forge's hourly recheck as its
    /// only worker. One construction, both doors (CLAUDE.md §9a): the
    /// env this test compares is BUILT once, so it cannot drift.
    #[test]
    fn both_doors_hand_a_probe_the_same_reader_environment() {
        let base = "http://sor.invalid:7900";
        let unattended = Shell::unattended(base).expect("the unattended door builds");
        // Its own checkout, so this holds wherever BOSS_PROBE_DIR points.
        let hand = Shell::here(None).with_probe_reader(unattended.cwd.as_deref(), base);
        assert_eq!(hand.env, unattended.env, "the promised env is one thing");
        assert_eq!(hand.path_prefix, unattended.path_prefix);
        // ...and the hand door is still the operator's own shell: here,
        // as them, with no timeout (`admit` says so too — a probe that
        // names an actor runs by hand and is only refused at the gate).
        assert_eq!(hand.cwd, None);
        assert_eq!(hand.user, None);
        assert_eq!(hand.timeout_secs, None);
    }

    /// THE SANCTIONED READER IS ON THE PROBE'S PATH AND THE READER IS
    /// NAMED. The measured refusal was `boss-sor-read not found`; the
    /// tool is in the tree beside its route table, so nothing has to be
    /// installed. And the identity it is handed is the read-scoped
    /// actor — never unset (the reader refuses that) and never
    /// `operator:unidentified`, which is answered with a narrower world
    /// silently and makes an absence assertion pass against a page the
    /// probe was never allowed to see (61085a9e).
    #[test]
    fn the_hand_door_puts_the_sanctioned_reader_on_the_probes_path() {
        let root = boss_testing::repo_root();
        let shell = Shell::here(None).with_probe_reader(Some(&root), "http://sor.invalid:7900");
        let o = execute_with(
            "command -v boss-sor-read > /dev/null || { echo no-reader; exit 1; }; \
             printf '[%s][%s]\\n' \"$BOSS_JOBS_URL\" \"$BOSS_SOR_PORTS\"; \
             printf '%s\\n' \"$BOSS_SOR_USER\"",
            &shell,
        )
        .expect("bash runs");
        assert_eq!(o.exit, 0, "{o:?}");
        assert!(o.missing_tools.is_empty(), "{o:?}");
        assert!(
            o.stdout.contains("[http://sor.invalid:7900][jobs=7900"),
            "{o:?}"
        );
        let user: Value = serde_json::from_str(o.stdout.lines().last().unwrap()).unwrap();
        assert_eq!(user["id"], READER_ACTOR);
        assert_eq!(user["role"], READER_ROLE);
        assert_eq!(user["access_tier"], "auditor");
        assert_ne!(user["id"], crate::identity::UNIDENTIFIED);
    }

    /// A DOOR WITH NO TREE STILL NAMES THE READER. Outside a worktree
    /// there is no `probe-bin` to put on PATH and no port table to
    /// read — a probe needing the reader then says `not found`, which
    /// is the honest DID-NOT-RUN. What must not happen is the read
    /// going out unidentified: the identity does not come from the
    /// tree, so it is handed over either way.
    #[test]
    fn without_a_tree_the_reader_is_still_named_rather_than_unidentified() {
        let shell = Shell::here(None).with_probe_reader(None, "http://sor.invalid:7900");
        assert_eq!(shell.path_prefix, None);
        let user = shell
            .env
            .iter()
            .find(|(k, _)| k == "BOSS_SOR_USER")
            .map(|(_, v)| v.clone())
            .expect("the reader identity is not the tree's to give");
        let user: Value = serde_json::from_str(&user).unwrap();
        assert_eq!(user["id"], READER_ACTOR);
        assert_ne!(user["id"], crate::identity::UNIDENTIFIED);
    }

    /// THE CAR'S OWN CONVERGED INSTANT IS PART OF THAT PROMISE
    /// (backlog a92571a6). A probe that needs a cutoff gets one that
    /// does NOT move: the commit time of the car's own merge, read
    /// from the checkout the probe runs in. A merge that is not in
    /// this checkout hands over no instant at all — the honest not-yet
    /// — and a ref that is not an object name is never handed to git.
    #[test]
    fn the_probe_is_handed_its_cars_own_converged_instant() {
        let dir = boss_testing::scratch::scratch_dir("prove-car-instant");
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .args(["-C", &dir.display().to_string()])
                .args(args)
                .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
                .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .output()
                .expect("git runs");
            assert!(o.status.success(), "git {args:?}: {o:?}");
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("f"), "x").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "landed"]);
        let sha = git(&["rev-parse", "HEAD"]);

        let shell = Shell::here(Some(&dir)).with_car_instant(Some(&sha[..12]));
        let at = |s: &Shell, k: &str| s.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(
            at(&shell, boss_jobs::probe::CAR_CONVERGED_AT_VAR).as_deref(),
            Some("1700000000"),
            "the merge's own commit time, in epoch seconds: {shell:?}"
        );
        assert_eq!(
            at(&shell, boss_jobs::probe::CAR_MERGE_REF_VAR).as_deref(),
            Some(&sha[..12])
        );
        // The probe sees it, and a stale one from the runner's own
        // environment cannot reach it.
        assert!(
            shell
                .strip
                .iter()
                .any(|n| n == boss_jobs::probe::CAR_CONVERGED_AT_VAR)
        );
        let o =
            execute_with("printf '[%s]\\n' \"$BOSS_CAR_CONVERGED_AT\"", &shell).expect("bash runs");
        assert!(o.stdout.contains("[1700000000]"), "{o:?}");

        // A merge this checkout does not have: the ref rides, the
        // instant does not, and the recipe's guard says not yet.
        let absent = Shell::here(Some(&dir)).with_car_instant(Some("0123456789ab"));
        assert_eq!(at(&absent, boss_jobs::probe::CAR_CONVERGED_AT_VAR), None);
        assert_eq!(
            at(&absent, boss_jobs::probe::CAR_MERGE_REF_VAR).as_deref(),
            Some("0123456789ab")
        );
        // Not an object name — never handed to git, so neither rides.
        for bad in ["--upload-pack=touch /tmp/x", "main", ""] {
            let s = Shell::here(Some(&dir)).with_car_instant(Some(bad));
            assert!(s.env.is_empty(), "{bad:?}: {s:?}");
        }
        assert!(
            Shell::here(Some(&dir))
                .with_car_instant(None)
                .env
                .is_empty()
        );
    }

    /// AND THE CAR SAYS WHICH MERGE THAT WAS — the `merge_ref` the
    /// conductor wrote on it, read the way every other door reads it.
    #[test]
    fn the_cars_merge_ref_is_read_off_its_metadata() {
        let car = json!({"metadata": {"merge_ref": "ead63bba8aff"}});
        assert_eq!(car_merge_ref(&car), Some("ead63bba8aff"));
        assert_eq!(car_merge_ref(&json!({"metadata": {}})), None);
        assert_eq!(
            car_merge_ref(&json!({"metadata": {"merge_ref": "not-a-sha"}})),
            None
        );
    }

    /// THE STARVED CUTOFF IS SAID AT THE DOOR A BUILDER IS STANDING AT,
    /// with the promised variable named (a92571a6). Car 372ac8fd's
    /// recorded probe is the live instance.
    #[test]
    fn a_probe_that_dates_its_cutoff_from_head_is_warned_about() {
        let probe = "c=$(git show HEAD:infra/cluster/dev-scratch-reclaim.sh | grep -c ls-remote); \
                     since=$(git log -1 --format=%ct HEAD); \
                     j=$(boss-sor-read '/api/jobs?kind=maintenance-dev-scratch-reclaim')";
        let w: Vec<String> = shape_warnings(probe).collect();
        let said = w
            .iter()
            .find(|w| w.contains("DATES ITS CUTOFF"))
            .unwrap_or_else(|| panic!("{w:?}"));
        assert!(
            said.contains(boss_jobs::probe::CAR_CONVERGED_AT_VAR),
            "{said}"
        );
        assert!(
            said.contains(boss_jobs::probe::MOVING_HEAD_EVIDENCE),
            "{said}"
        );
        assert!(said.contains("warning, not a refusal"), "{said}");
        // The rewrite it names is not warned about in turn.
        let fixed = "c=$(git show HEAD:infra/cluster/dev-scratch-reclaim.sh | grep -c ls-remote); \
                     since=${BOSS_CAR_CONVERGED_AT}; \
                     case ${since:-empty} in empty|*[!0-9]*) echo 'not yet'; exit 75;; esac";
        assert!(
            !shape_warnings(fixed).any(|w| w.contains("DATES ITS CUTOFF")),
            "{:?}",
            shape_warnings(fixed).collect::<Vec<_>>()
        );
    }

    /// A LIMIT IS NOT A FILTER, SAID AT THE DOOR (e7cf78c6). Car
    /// ead4a6ed's recorded probe is the live instance: 300 rows asked
    /// of a list of 345, with the row it waited on in the tail.
    #[test]
    fn a_probe_that_counts_a_page_is_warned_about() {
        let probe = "n=$(boss-sor-read '/api/jobs?kind=gate-run&limit=300' \
                     | jq '[.data[] | select(.metadata.flake == true)] | length'); \
                     [ \"$n\" -ge 1 ] && echo flake:seen";
        let w: Vec<String> = shape_warnings(probe).collect();
        let said = w
            .iter()
            .find(|w| w.contains("COUNTS A PAGE"))
            .unwrap_or_else(|| panic!("{w:?}"));
        assert!(said.contains("limit=300"), "{said}");
        assert!(
            said.contains(boss_jobs::probe::TRUNCATED_PAGE_EVIDENCE),
            "{said}"
        );
        // It says which way it fails, because that is what its reader
        // decides on.
        assert!(said.contains("CAN FAIL OPEN"), "{said}");
        // And the rewrite it names — one body, rows judged against the
        // total — is not warned about in turn.
        let fixed = "body=$(boss-sor-read '/api/jobs?kind=gate-run&limit=300'); \
                     rows=$(printf '%s' \"$body\" | jq '.data | length'); \
                     seen=$(printf '%s' \"$body\" | jq '.total'); \
                     [ \"$rows\" -eq \"$seen\" ] || { echo 'not yet: the page is not the list'; exit 75; }";
        assert!(
            !shape_warnings(fixed).any(|w| w.contains("COUNTS A PAGE")),
            "{:?}",
            shape_warnings(fixed).collect::<Vec<_>>()
        );
    }

    /// A MENTION IS NOT A DEFINITION, SAID AT THE SAME DOOR (e7cf78c6):
    /// the bare name matched the doc comment, the count came back
    /// nonzero, and nothing was defined.
    #[test]
    fn a_probe_that_greps_a_bare_name_is_warned_about() {
        let probe = "c=$(git show HEAD:crates/core/boss-jobs/src/claims.rs \
                     | grep -c in_flight_claims); \
                     [ \"$c\" -ge 1 ] && echo claim:ok";
        let w: Vec<String> = shape_warnings(probe).collect();
        let said = w
            .iter()
            .find(|w| w.contains("COUNTS A NAME"))
            .unwrap_or_else(|| panic!("{w:?}"));
        assert!(said.contains("in_flight_claims"), "{said}");
        assert!(
            said.contains(boss_jobs::probe::MENTION_NOT_DEFINITION_EVIDENCE),
            "{said}"
        );
        assert!(said.contains("warning, not a refusal"), "{said}");
        // The quoted definition it names is not warned about in turn —
        // the negative case that keeps this from warning about every
        // grep there is.
        let fixed = "c=$(git show HEAD:crates/core/boss-jobs/src/claims.rs \
                     | grep -c 'pub fn in_flight_claims'); \
                     [ \"$c\" -ge 1 ] && echo claim:ok";
        assert!(
            !shape_warnings(fixed).any(|w| w.contains("COUNTS A NAME")),
            "{:?}",
            shape_warnings(fixed).collect::<Vec<_>>()
        );
    }

    /// AND THE STRIP IS REAL. The door's `env_remove` cannot be shown
    /// from inside one process without mutating its environment (racy
    /// under the parallel runner), so the mechanism is proven through a
    /// wrapper: this test's own binary re-run under `BOSS_ACTOR=leak`
    /// executes the probe through the door and prints what the probe
    /// saw. The unattended shell names both spellings of the actor.
    #[test]
    fn a_stripped_name_does_not_reach_the_probe() {
        if std::env::var("BOSS_PROVE_STRIP_INNER").is_ok() {
            // Inner leg: the parent carries BOSS_ACTOR=leak.
            let shell = Shell {
                strip: vec!["BOSS_ACTOR".into()],
                ..Shell::here(None)
            };
            let o = execute_with("printf '%s' \"${BOSS_ACTOR:-unset}\"", &shell).unwrap();
            println!("STRIP-SAW={}", o.stdout);
            return;
        }
        let me = std::env::current_exe().unwrap();
        let out = std::process::Command::new(me)
            .args([
                "--exact",
                "prove::tests::a_stripped_name_does_not_reach_the_probe",
                "--nocapture",
            ])
            .env("BOSS_ACTOR", "leak")
            .env("BOSS_PROVE_STRIP_INNER", "1")
            .output()
            .unwrap();
        let printed = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{printed}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            printed.contains("STRIP-SAW=unset"),
            "the strip must remove it: {printed}"
        );
        let shell = Shell::unattended("http://sor.invalid:7900").unwrap();
        for name in ["BOSS_ACTOR", "BOSS_ACTOR_FILE"] {
            assert!(shell.strip.contains(&name.to_string()), "{:?}", shell.strip);
        }
    }

    /// THE PORT TABLE IS READ AS DATA: `name=port` lines become one
    /// space-separated table; comments, blanks and padding are not in
    /// it. The twin's `grep -Ev | tr -s | sed` extraction, in Rust.
    #[test]
    fn the_port_table_is_read_as_data() {
        assert_eq!(
            sor_ports_table("# a comment\n\njobs=7900\nevents=7150\n  people=7500  \n"),
            "jobs=7900 events=7150 people=7500"
        );
        assert_eq!(sor_ports_table(""), "");
        assert_eq!(sor_ports_table("# only\n# comments\n"), "");
        // The tree's own table parses to something a reader can route
        // by, and every entry is name=port.
        let tree =
            std::fs::read_to_string(boss_testing::repo_root().join("infra/forge/sor-ports.env"))
                .expect("the port table ships in the tree");
        let table = sor_ports_table(&tree);
        assert!(table.contains("jobs=7900"), "{table}");
        for entry in table.split(' ') {
            let (name, port) = entry.split_once('=').unwrap_or_else(|| panic!("{entry}"));
            assert!(!name.is_empty());
            port.parse::<u16>()
                .unwrap_or_else(|e| panic!("{entry}: {e}"));
        }
    }

    /// THE FACT THAT LIVES TWICE GETS AN EQUALITY TEST (CLAUDE.md §9a).
    /// The reader's role is named here and DEFINED in core policy's
    /// defaults; they cannot be collapsed, so they are pinned equal.
    /// `audit-readonly` must grant Read at Scope::All on what a probe
    /// reads — and no non-Read action anywhere, which is what makes
    /// handing it to program text a builder wrote safe (61085a9e).
    #[test]
    fn the_probes_reader_role_can_read_everything_and_write_nothing() {
        use boss_policy_client::{Action, Resource, Scope};

        let rules = boss_policy_client::defaults::default_rules();
        let mine: Vec<_> = rules.iter().filter(|r| r.role == READER_ROLE).collect();
        assert!(
            !mine.is_empty(),
            "the probe's reader role '{READER_ROLE}' is not seeded by core policy at all — \
             an unseeded role reads NOTHING, which is the defect with extra steps"
        );
        for resource in [Resource::job(), Resource::step(), Resource::event()] {
            assert!(
                mine.iter().any(|r| r.resource == resource
                    && r.action == Action::Read
                    && r.scope == Scope::All),
                "'{READER_ROLE}' has no Read/All on {resource:?} — a probe carrying it would \
                 see a narrower world than the operator, which is what 61085a9e measured"
            );
        }
        for rule in &mine {
            assert_eq!(
                rule.action,
                Action::Read,
                "'{READER_ROLE}' carries a non-Read grant ({:?} on {:?}) — it is handed to \
                 program text a builder wrote, so it must not be able to change anything",
                rule.action,
                rule.resource
            );
        }
    }

    /// THE ADMISSION, in the twin's order, each refusal changing
    /// nothing: kind, merged, probe (event-bound named), expect, step,
    /// status — and a completed `proven` is "nothing to run", because
    /// the daily recheck re-files for a car proven by hand meanwhile.
    #[test]
    fn the_unattended_door_admits_only_a_merged_probed_car_with_proven_open() {
        let car = |kind: &str, merged: Value, md: Value, status: &str| {
            let mut m = md;
            m["merged"] = merged;
            json!({
                "id": "aaaaaaaa-0000-4000-8000-000000000000", "kind": kind,
                "metadata": m,
                "steps": [{"id": "s-proven", "spec_slug": "proven", "title": PROVEN, "status": status}]
            })
        };
        let probed = json!({"proof_probe": "true && echo x:ok", "proof_expect": "x:ok", "summary": "the claim"});
        match admit_unattended(&car(
            "ship-a-change",
            json!("true"),
            probed.clone(),
            "ready",
        )) {
            Admitted::Run {
                probe,
                expect,
                step_id,
                verified,
            } => {
                assert_eq!(probe, "true && echo x:ok");
                assert_eq!(expect, "x:ok");
                assert_eq!(step_id, "s-proven");
                assert_eq!(verified, "the claim");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            admit_unattended(&car(
                "ship-a-change",
                json!("true"),
                probed.clone(),
                "completed"
            )),
            Admitted::AlreadyProven
        );
        let refused = |c: &Value| match admit_unattended(c) {
            Admitted::Refused(why) => why,
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert!(
            refused(&car("gate-run", json!("true"), probed.clone(), "ready"))
                .contains("is a gate-run, not a ship-a-change car")
        );
        assert!(
            refused(&car(
                "ship-a-change",
                json!("false"),
                probed.clone(),
                "ready"
            ))
            .contains("has not merged")
        );
        assert!(
            refused(&car("ship-a-change", Value::Null, probed.clone(), "ready"))
                .contains("has not merged")
        );
        assert!(
            refused(&car(
                "ship-a-change",
                json!("true"),
                json!({"proof_event": "the next red train"}),
                "ready"
            ))
            .contains("EVENT-BOUND")
        );
        assert!(
            refused(&car("ship-a-change", json!("true"), json!({}), "ready"))
                .contains("recorded no `proof_probe`")
        );
        assert!(
            refused(&car(
                "ship-a-change",
                json!("true"),
                json!({"proof_probe": "true"}),
                "ready"
            ))
            .contains("echo hi")
        );
        assert!(
            refused(&car(
                "ship-a-change",
                json!("true"),
                probed.clone(),
                "pending"
            ))
            .contains("proven step is \"pending\"")
        );
        let no_step = json!({"id": "aaaaaaaa-0000-4000-8000-000000000000", "kind": "ship-a-change",
            "metadata": {"merged": "true", "proof_probe": "true", "proof_expect": "x"}, "steps": []});
        assert!(refused(&no_step).contains("has no proven step"));
        // A car with no summary is still run, with the twin's default
        // prose; `verified` stays required on the step.
        let bare = json!({"proof_probe": "true && echo x:ok", "proof_expect": "x:ok"});
        match admit_unattended(&car("ship-a-change", json!("true"), bare, "active")) {
            Admitted::Run { verified, .. } => {
                assert_eq!(
                    verified,
                    "proven by the probe the car recorded at park time"
                )
            }
            other => panic!("{other:?}"),
        }
    }

    /// THE OUTPUT SHAPE THE PACKET'S READERS EXPECT: the verdict line
    /// leads with its ALL-CAPS word — PROVEN, NOT YET, NOT RUN, NOT
    /// PROVEN — then the car, then why; and the exit code is one of
    /// the three the ops-request carries, plus 0.
    #[test]
    fn the_unattended_verdict_line_leads_with_its_word() {
        let missing = vec!["kubectl".to_string()];
        let cases: [(Verdict, &str, i32); 4] = [
            (
                Verdict::Proven,
                "PROVEN c1 — exit 0 and printed \"x:ok\"",
                0,
            ),
            (
                Verdict::NotYet {
                    said: "not yet: none".into(),
                },
                "NOT YET c1 — why",
                NOT_YET_EXIT,
            ),
            (
                Verdict::Unrunnable {
                    missing: missing.clone(),
                },
                "NOT RUN c1 — why",
                UNRUNNABLE_EXIT,
            ),
            (
                Verdict::NotProven(anyhow::anyhow!("no")),
                "NOT PROVEN c1 — why",
                1,
            ),
        ];
        for (v, line, code) in &cases {
            assert_eq!(unattended_verdict_line("c1", v, "x:ok", "why"), *line);
            assert_eq!(unattended_exit(v), *code);
        }
    }

    /// The three non-zero codes are distinct from each other and from
    /// the refusal's, so a reader of the ops-request's `exit_code`
    /// knows what to do without opening the output.
    #[test]
    fn the_unattended_door_exits_one_of_three_codes() {
        let codes = [1, UNRUNNABLE_EXIT, NOT_YET_EXIT, REFUSED_EXIT];
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "{codes:?}");
        assert_eq!(REFUSED_EXIT, 2);
    }

    /// THE ATTEMPT RECORD CARRIES THE KEYS THE YARD READS
    /// (`apps/web/src/it/yard/yard.ts`, `proofAttempt`) and `--recheck`
    /// re-runs from: one shape from both doors.
    #[test]
    fn the_attempt_record_carries_the_keys_the_yard_reads() {
        let o = Outcome {
            exit: 75,
            stdout: "not yet: none\n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        };
        let a = attempt_json("true", Some("x:ok"), &o, "h", "now", "NOT YET: none", None);
        let keys: std::collections::BTreeSet<&str> =
            a.as_object().unwrap().keys().map(String::as_str).collect();
        let want: std::collections::BTreeSet<&str> = [
            "at",
            "exit",
            "stdout",
            "stderr",
            "host",
            "probe",
            "expect",
            "why",
            "unrunnable",
            "not_yet",
            "missing_tools",
            boss_jobs::car::NOT_YET_SINCE,
            boss_jobs::car::NOT_YET_RUNS,
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, want);
        assert_eq!(a["not_yet"], true);
        assert_eq!(a["unrunnable"], false);
    }

    /// THE ATTEMPT CARRIES ITS NOT-YET STREAK (backlog adef5ddf). Both
    /// doors write through here, so both hand the car's PRIOR attempt in
    /// and the record says how long this probe has been answering not
    /// yet — the one signal that tells a starved probe from a patient
    /// one. A run that is not a not-yet carries no streak (null, zero),
    /// so the shape stays one shape.
    #[test]
    fn a_not_yet_attempt_carries_the_streak_from_the_prior_one() {
        let o = Outcome {
            exit: 75,
            stdout: "not yet: none\n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
        };
        let prior = json!({
            "at": "2026-09-23T06:00:00Z", "exit": 75, "not_yet": true, "probe": "true",
            "not_yet_since": "2026-09-19T05:50:00Z", "not_yet_runs": 85,
        });
        let a = attempt_json(
            "true",
            Some("x:ok"),
            &o,
            "h",
            "2026-09-23T07:00:00Z",
            "NOT YET: none",
            Some(&prior),
        );
        assert_eq!(a[boss_jobs::car::NOT_YET_SINCE], "2026-09-19T05:50:00Z");
        assert_eq!(a[boss_jobs::car::NOT_YET_RUNS], 86);

        let red = Outcome {
            exit: 1,
            stdout: String::new(),
            stderr: "FAILED\n".into(),
            missing_tools: Vec::new(),
        };
        let a = attempt_json(
            "true",
            Some("x:ok"),
            &red,
            "h",
            "2026-09-23T07:00:00Z",
            "FAILED",
            Some(&prior),
        );
        assert_eq!(a[boss_jobs::car::NOT_YET_SINCE], Value::Null);
        assert_eq!(a[boss_jobs::car::NOT_YET_RUNS], 0);
    }

    /// THE DID-NOT-RUN SENTENCE at the unattended door names the forge's
    /// vantage — the user and the checkout — with the same lead word and
    /// disclaimer as the hand door's.
    #[test]
    fn the_unattended_unrunnable_sentence_names_the_vantage() {
        let missing = vec!["kubectl".to_string()];
        let why =
            unrunnable_why_unattended("forge", &missing, "david", Path::new("/home/david/boss"));
        assert!(
            why.starts_with("THE PROBE DID NOT RUN on forge: kubectl not found"),
            "{why}"
        );
        assert!(why.contains("as david in /home/david/boss"), "{why}");
        assert!(
            why.contains("This says nothing about whether the change works"),
            "{why}"
        );
        let hand = unrunnable_why("pod", &missing);
        assert!(
            hand.starts_with("THE PROBE DID NOT RUN on pod: kubectl not found"),
            "{hand}"
        );
        assert!(
            hand.contains("This says nothing about whether the change works"),
            "{hand}"
        );
    }
}
