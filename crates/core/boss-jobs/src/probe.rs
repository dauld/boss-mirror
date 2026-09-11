//! WHICH PROBES ARE LEGAL — one definition, every door.
//!
//! A car records a probe (`boss_jobs::car::PROOF_PROBE`) and several
//! things later RUN it: `boss gate --park-probe` admits the text,
//! `boss prove --probe / --from-car / --recheck` runs it by hand, the
//! `jobs.run-car-probes` rule ships it to the forge on arrival, and
//! `infra/forge/run-car-probe.sh` executes it there. The rules about
//! which text is worth running live HERE, in one place all of those can
//! reach, because the alternative is what backlog 23b2dffa found: the
//! gate refused two shapes of bad probe, the hand verb refused neither,
//! and the only trace of the rule at the second door was a comment
//! describing the first (CLAUDE.md §9a in its behavioural form — one
//! rule, two enforcement sites, one of which is prose about the other).
//!
//! THREE QUESTIONS, AND THEY ARE NOT THE SAME KIND OF QUESTION. The
//! first asks whether the text can RUN where it is going, which is
//! host-relative ([`needs_absent_tool`]). The second asks whether its
//! answer can be TRUSTED, which is true everywhere
//! ([`reads_the_sor_unidentified`]). The third asks whether the text
//! says what its author meant — a `jq -e` whose success branch is
//! `empty` ([`asserts_its_own_negation`]), a bare `|| exit n` that
//! throws away the status naming the cause
//! ([`rewrites_its_exit_status`]) — which is also true everywhere.
//!
//! Each door applies the first only about the host it is actually
//! sending the probe to, and the other two always. The second REFUSES,
//! because it fails open: the probe passes, the absence assertion is
//! green against a world it was never allowed to see, and a car closes
//! on a proof of nothing. The third only WARNS, because it fails
//! closed — `boss prove` records nothing on a nonzero exit, so the worst
//! it does is strand a car and misdescribe why (18 hours of that,
//! 4fccc595) — and because both its detectors are coarse text scans a
//! false refusal would be too expensive for.
//!
//! The predicates below are shared; the wording of a refusal or a
//! warning belongs to the door, because what to do instead differs by
//! door.
//!
//! WHICH SHAPE OF PROBE A GIVEN CLAIM ADMITS — a different question from
//! any of the three above, and the one that actually cost a session:
//! docs/design/a-probe-shape-follows-the-car.md, keyed to what the car
//! changed.

use serde_json::Value;

/// WHERE A RECORDED PROBE RUNS — the forge host's absence manifest.
///
/// A `--park-probe` is written on the dev pod (kubectl, a kubeconfig,
/// the cluster one hop away) and RUN on the forge
/// (`infra/forge/run-car-probe.sh`, as david, in /home/david/boss, when
/// the car's train arrives). Two machines. The forge is outside the
/// cluster and holds no kubeconfig, so a probe that reaches for
/// `kubectl` is correct and unrunnable — and its failure at arrival is
/// an exit code, hours later, on a car.
///
/// This file lists the tools MEASURED absent from that host, so the
/// refusal happens at gate time on the builder's terminal instead. It
/// is data, not code: an absence measured next month is a line, not a
/// release. Backlog f9304366.
const FORGE_ABSENT_TOOLS: &str = include_str!("../../../../infra/forge/host-absent-tools.txt");

/// The manifest's live lines: one tool name each, comments and blanks
/// dropped.
pub fn forge_absent_tools() -> Vec<&'static str> {
    FORGE_ABSENT_TOOLS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// The commands a shell line would RUN, in command position — the first
/// word of the probe and of every segment after a `|`, `&&`, `||`, `;`,
/// a newline, a subshell or a substitution. Leading environment
/// assignments and flags are stepped over, as are the words that stand
/// in front of a program rather than being one (`if`, `sudo`, `env`, …),
/// and a path is reduced to its basename so `/usr/bin/kubectl` reads as
/// `kubectl`.
///
/// Deliberately not a shell parser: it does not know quoting, so a tool
/// name inside a quoted string that follows a separator can be read as
/// a command. The cost of that is a refusal the builder can reword; the
/// cost of the alternative is a shell parser to maintain.
pub fn commands_invoked(probe: &str) -> Vec<&str> {
    const NOT_THE_PROGRAM: [&str; 17] = [
        "if", "then", "elif", "else", "fi", "while", "until", "for", "do", "done", "case", "esac",
        "!", "time", "sudo", "env", "command",
    ];
    probe
        .split(['|', '&', ';', '\n', '(', ')', '`', '{', '}'])
        .filter_map(|segment| {
            segment
                .split_whitespace()
                .map(|w| w.trim_matches(['"', '\'', '$', '\\']))
                .find(|w| {
                    !w.is_empty()
                        && !w.contains('=')
                        && !w.starts_with('-')
                        && !NOT_THE_PROGRAM.contains(w)
                })
                .map(|w| w.rsplit('/').next().unwrap_or(w))
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// The tool the forge does not have that this probe would need, if any.
pub fn needs_absent_tool(probe: &str) -> Option<&'static str> {
    let absent = forge_absent_tools();
    commands_invoked(probe)
        .into_iter()
        .find_map(|c| absent.iter().find(|a| **a == c).copied())
}

/// HOW A PROBE READS THE SYSTEM OF RECORD — `infra/forge/probe-bin`,
/// first on the probe's PATH, holding exactly this one reader.
///
/// THE DEFECT IT REPLACES (backlog 61085a9e, measured 2026-09-10). The
/// probe's env carried only `BOSS_JOBS_URL` — never an identity header —
/// so `curl $BOSS_JOBS_URL/api/...` read as `operator:unidentified` and
/// policy answered a NARROWER WORLD in silence. Same backend, same
/// commit: `?kind=ship-a-change&status=open` was 21 rows for the
/// operator and 0 unidentified; `/api/yard/status` came back with
/// trains, dock, held, recent and `dock_depth` ALL ZERO — a confident,
/// well-formed, completely idle yard. `/api/workflows` was identical
/// between the two readers, so the narrowing is per-surface and nothing
/// in an answer says which kind you hit.
pub const SOR_READER: &str = "boss-sor-read";

/// The env var the forge runner exports the probe's READ-SCOPED actor
/// under. A probe that sends it is identified (and, being
/// `audit-readonly`, can change nothing), so it is allowed.
pub const SOR_USER_VAR: &str = "BOSS_SOR_USER";

/// The spellings that reach the system of record DIRECTLY — past the
/// gateway, on the jobs API's own port, which is where policy answers a
/// nameless reader with a narrower world IN SILENCE. All three were used
/// by real probes: the env var the runner exports, the in-cluster
/// service DNS (the second measured instance) and the LAN address and
/// port (the first).
const DIRECT_SOR_SPELLINGS: [&str; 3] = ["BOSS_JOBS_URL", "boss-jobs-internal", ":7900"];

/// The fourth spelling, and the loose one: a bare `/api/` path, reaching
/// the system of record through whatever host the probe computed —
/// usually the gateway. It stays, because a URL assembled in a variable
/// (`G=http://…; curl "$G/api/jobs"`) names nothing else a text scan can
/// see. It is SEPARATE from the three above because the two hosts fail
/// differently, and the session carve-out below turns on which one a
/// probe is talking to.
const SOR_PATH_SPELLING: &str = "/api/";

/// THE GATEWAY IS A THIRD IDENTITY, and the one the rule could not see
/// until 2026-09-11 (backlog 5dc5159d, nine overrides on correct probes,
/// four of them in one afternoon).
///
/// MEASURED, one moment, three readers. A session obtained from this
/// endpoint read `total` 22 / 56 / 703 on open ship-a-change, open
/// backlog-item and gate-run — IDENTICAL to the operator's own door —
/// while the same queries unidentified against the jobs API's own port
/// answered 0 / 0 / 0. The session's own `/api/auth/me` reads
/// `role: audit-readonly`, the same read-scoped role [`SOR_USER_VAR`]
/// carries. So a session is a full-width READ identity, not a narrowed
/// one.
///
/// AND IT IS NOT THE FORGEABLE-HEADER HOLE. This rule deliberately keys
/// on [`SOR_USER_VAR`] rather than a header NAME, so a probe cannot
/// satisfy it by writing its own privileged literal. A cookie is a
/// different object: measured the same day, `-b
/// 'boss_session=iamtheoperator'` answered **401**, byte-identical to
/// sending no cookie at all. A forged header is accepted by a trusting
/// upstream; a forged session is refused at the door — so admitting a
/// session cannot produce a false PASS, only a loud failure. That
/// asymmetry is the whole argument, and it is why the carve-out needs
/// the text to OBTAIN a session here as well as send one.
const GATEWAY_AUTH_PATH: &str = "/api/auth/";

/// The flags that SEND a cookie on the read. Receiving one into a jar
/// (`-c`) is not enough — the read has to carry it.
const COOKIE_SEND_FLAGS: [&str; 2] = ["-b", "--cookie"];

/// Does this probe hold a session the gateway ISSUED it, and send it?
/// Both halves are required: the POST is what makes the value
/// server-issued, the flag is what puts it on the read.
fn holds_a_gateway_session(probe: &str) -> bool {
    probe.contains(GATEWAY_AUTH_PATH)
        && probe
            .split_whitespace()
            .any(|w| COOKIE_SEND_FLAGS.contains(&w))
}

/// Commands that can perform the read. `python`/`python3` are here
/// because the first measured instance was `urllib.request.urlopen`,
/// not a curl.
const HTTP_CLIENTS: [&str; 4] = ["curl", "wget", "python", "python3"];

/// The facilities a `python` would have to name to reach the network at
/// all. A `python3` on the right of a pipe reads STDIN; it cannot open a
/// socket without one of these appearing in the text, so their ABSENCE
/// is what tells a parser from a client.
///
/// This is the gap that bit hardest: `boss-api GET … | python3` is the
/// shape CLAUDE.md §Doors steers an operator toward, it is the MOST
/// correct way to read the system of record, and it read as unidentified
/// — measured 28 times across the 449 cars that have ever recorded a
/// probe. Deliberately coarse (whole-text, not per-invocation) and
/// deliberately erring toward refusal: a python that names one of these
/// is a client again whether or not a door is also present, so a door
/// cannot launder a python that reads.
const PYTHON_CAN_READ: [&str; 14] = [
    "urllib",
    "import requests",
    "requests.get",
    "requests.post",
    "requests.request",
    "http.client",
    "httplib",
    "httpx",
    "aiohttp",
    "pycurl",
    "socket",
    "subprocess",
    "os.system",
    "os.popen",
];

/// Could the `python` in this text perform the read itself?
fn python_performs_the_read(probe: &str) -> bool {
    PYTHON_CAN_READ.iter().any(|f| probe.contains(f))
}

/// Does this probe read the system of record WITHOUT saying who it is?
/// Returns the client it would read with.
///
/// Two conditions, both required, because either alone is a false
/// refusal: an HTTP client in COMMAND POSITION (the same scan
/// [`needs_absent_tool`] uses, so `grep -c BOSS_JOBS_URL <file>` and
/// `test -n "$BOSS_JOBS_URL"` are mentions, not reads), and the text
/// naming the system of record at all.
///
/// THREE IDENTITIES COUNT, and they are recognised in three different
/// ways. The runner's read-scoped actor ([`SOR_USER_VAR`]) — keyed on
/// the env var, not the header name, for the reason written at
/// [`GATEWAY_AUTH_PATH`]. A gateway session the server ISSUED
/// ([`holds_a_gateway_session`]) — admitted only for a gateway-shaped
/// read, because a cookie identifies a reader at the gateway and says
/// nothing about a read on the jobs API's own port. And a sanctioned
/// door (`boss-api`, [`SOR_READER`]) doing the read while something else
/// parses its stdout — recognised not by naming the door but by
/// [`python_performs_the_read`] finding nothing in the text that could
/// open a socket.
///
/// A reader identified in a FOURTH way still reads as a refusal here,
/// which is why every door that refuses on this rule carries a stated
/// override. What the rule no longer does is refuse the three above —
/// nine such overrides were recorded before 2026-09-11, and an override
/// that routine is read by nobody (CLAUDE.md §Diagnosis).
pub fn reads_the_sor_unidentified(probe: &str) -> Option<&'static str> {
    let direct = DIRECT_SOR_SPELLINGS.iter().any(|s| probe.contains(s));
    if !direct && !probe.contains(SOR_PATH_SPELLING) {
        return None;
    }
    if probe.contains(SOR_USER_VAR) {
        return None;
    }
    if !direct && holds_a_gateway_session(probe) {
        return None;
    }
    commands_invoked(probe).into_iter().find_map(|c| {
        let client = HTTP_CLIENTS.iter().find(|h| **h == c).copied()?;
        // A python handed a pipe is a parser, not a reader.
        if matches!(client, "python" | "python3") && !python_performs_the_read(probe) {
            return None;
        }
        Some(client)
    })
}

/// WHY AN UNIDENTIFIED READ IS REFUSED RATHER THAN DOCUMENTED — the
/// measured evidence, in one copy, quoted by every door that refuses on
/// it. The doors differ in what to do instead; they must not differ on
/// what happened (CLAUDE.md §9a).
pub const UNIDENTIFIED_READ_EVIDENCE: &str = "\
Measured 2026-09-10 (61085a9e), one backend, one commit: \
?kind=ship-a-change&status=open was 21 rows for the operator and 0 unidentified; \
/api/yard/status came back trains, dock, held, recent and dock_depth ALL ZERO — a \
confident, well-formed, completely idle yard. /api/workflows was identical for both, \
so nothing in an answer tells you which kind you hit.\n\n\
Why that is refused rather than documented: a PRESENCE assertion fails for the wrong \
reason and someone investigates, but an ABSENCE assertion PASSES FALSELY — 'no open job \
of kind X remains' is green against an empty page the probe was never allowed to see — \
and a recorded proof of nothing closes a car. Prefer asserting the PRESENCE of a named \
thing over the absence of any thing: a count-is-zero or flag-is-false claim against a \
policy-scoped surface is what a narrowed read produces anyway.\n\n\
Measured 2026-09-11 (5dc5159d), the same three queries, three readers: a gateway session \
read 22 / 56 / 703 rows — IDENTICAL to the operator's own door — while unidentified \
against the jobs API's own port they were 0 / 0 / 0. So the silent narrowing is a \
property of the DIRECT port; the gateway answers an unidentified reader 401, loudly. \
Three identities this check can see: the runner's BOSS_SOR_USER, a gateway session the \
server issued (POST /api/auth/guest, sent with curl -b), and a door doing the read while \
something else parses its stdout.";

/// WHEN A PROBE REPORTS FAILURE PRECISELY BECAUSE THE CLAIM HOLDS —
/// the third shape, and the only one whose lie points the other way.
///
/// THE DEFECT (backlog 4fccc595, measured 2026-09-11). Car a0ab90a5 sat
/// unproven for 18 hours on a probe that COULD NOT PASS. Its success
/// path was `jq -e '… | if (<claim holds>) then empty else error(…) end'`
/// — and `jq -e` exits **4** when the filter produces no output, while
/// `empty` is not output at all. So the claim holding made jq exit 4,
/// the trailing `|| exit 1` turned that into 1, and the recorded verdict
/// read "the claim it makes is not holding, or the probe is wrong". The
/// filter was re-run against correct live data the next day and exited 4
/// on all three rows it checks: the probe asserted the negation of what
/// its author meant.
///
/// It is a WARNING wherever it is checked, not a refusal — the doors
/// argue that themselves (`boss prove`'s `admit`), but the fact behind
/// the argument is here: this shape cannot record a false proof. It
/// fails CLOSED, and `boss prove` records nothing on a nonzero exit, so
/// the damage is a stranded car and a misread verdict, never a car
/// closed on evidence of nothing. That is the opposite direction from
/// [`reads_the_sor_unidentified`], which is why the two doors treat them
/// differently.
pub const SELF_CONTRADICTORY_RULE: &str = "jq-e-whose-success-is-empty";

/// The measured evidence for [`asserts_its_own_negation`], in one copy,
/// quoted by every door that says anything about it. The doors differ in
/// what to do instead; they must not differ on what happened
/// (CLAUDE.md §9a).
pub const SELF_CONTRADICTORY_EVIDENCE: &str = "\
Measured 2026-09-11 (4fccc595): `jq -e` exits 4 when its filter produces NO output, and \
`empty` produces none — so `if <claim> then empty else error(…) end` under `-e` exits \
nonzero exactly when the claim HOLDS. Car a0ab90a5 sat unproven for 18 hours on that \
shape; the filter was re-run against correct live data and exited 4 on every row.\n\
The shape that cannot invert asserts positively and prints a token: \
`jq -e '<claim> or error(\"…\")' >/dev/null && echo claim:ok`.";

/// Does this `jq -e` assert the negation of what its author meant?
///
/// Two conditions: a `jq` invocation carrying an exit-status flag
/// (`-e`, a bundled `-re`, or `--exit-status`), and the word `empty`
/// somewhere after it. `-e` means "exit nonzero when the output is
/// false or absent", so a branch that emits `empty` makes success the
/// failing case.
///
/// DELIBERATELY COARSE, and a warning because of it: this does not
/// parse jq, so `jq -e '.name == "empty"'` reads as the shape and gets
/// warned about. The cost when it is wrong is one line a builder reads
/// and ignores; the cost of the alternative is a jq parser to maintain,
/// or 18 hours (4fccc595). A refusal could not be spent this cheaply,
/// which is a second reason this one only warns.
pub fn asserts_its_own_negation(probe: &str) -> bool {
    jq_tails(probe)
        .into_iter()
        .any(|tail| jq_has_exit_status_flag(tail) && has_word(tail, "empty"))
}

/// The text after each `jq` token, where `jq` stands in command
/// position. Not `commands_invoked`'s split: a jq filter is full of
/// `|`, `(` and `)`, so splitting on those takes the flags and the
/// filter into different pieces.
fn jq_tails(probe: &str) -> Vec<&str> {
    let bytes = probe.as_bytes();
    let boundary = |i: usize| -> bool {
        i == 0 || (!(bytes[i - 1] as char).is_alphanumeric() && bytes[i - 1] != b'_')
    };
    probe
        .match_indices("jq")
        .filter(|(i, _)| boundary(*i))
        .filter(|(i, _)| {
            // `jq` itself, not the tail of `jqlang` — and not a path
            // component, which `rsplit('/')` already reduces elsewhere.
            probe[i + 2..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        })
        .map(|(i, _)| &probe[i + 2..])
        .collect()
}

/// Is there an exit-status flag among this invocation's flags?
///
/// Walks the words in front of the filter, stepping over the values of
/// the flags that take them — `--arg k v` is two words that are not
/// flags, and stopping at the first of them would miss a `-e` written
/// after it.
fn jq_has_exit_status_flag(tail: &str) -> bool {
    const TWO_VALUES: [&str; 2] = ["--arg", "--argjson"];
    const ONE_VALUE: [&str; 6] = [
        "--slurpfile",
        "--rawfile",
        "--indent",
        "-f",
        "--from-file",
        "--jsonarg",
    ];
    let mut words = tail.split_whitespace();
    while let Some(w) = words.next() {
        if !w.starts_with('-') {
            // The filter. Flags are over.
            return false;
        }
        if w == "--exit-status" || (!w.starts_with("--") && w.contains('e')) {
            return true;
        }
        if TWO_VALUES.contains(&w) {
            words.next();
            words.next();
        } else if ONE_VALUE.contains(&w) {
            words.next();
        }
    }
    false
}

/// Does `word` appear in `text` as a whole word?
fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        let after = text[i + word.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        before && after
    })
}

/// Does this probe throw away the exit status that would explain its own
/// failure? Returns the status it substitutes.
///
/// The shape is a BARE `|| exit <n>`: the left-hand command's status is
/// replaced by `n` and nothing is printed, so jq's 4-versus-5 (filter
/// produced nothing / filter called `error`) and curl's 7-versus-22
/// (could not connect / the server said no) all arrive as the same
/// number. That is the reduction CLAUDE.md §Diagnosis names: it
/// suppresses OUTPUT, not work, and the cost is paid by whoever is next
/// in front of the failure.
///
/// `|| { echo "…$?"; exit 1; }` is NOT this shape and is not reported:
/// it keeps the evidence before collapsing the status, which is the
/// form the replacement probe on a0ab90a5 used.
pub fn rewrites_its_exit_status(probe: &str) -> Option<i32> {
    probe.split("||").skip(1).find_map(|tail| {
        let mut words = tail.split_whitespace();
        if words.next()? != "exit" {
            return None;
        }
        words
            .next()?
            .trim_end_matches([';', '}', ')'])
            .parse::<i32>()
            .ok()
            .filter(|n| *n != 0)
    })
}

/// The rule id a door records when an operator overrides a refusal on
/// it. Short, stable, and greppable across recorded proofs — an
/// override nobody can find later is the defect it was meant to avoid.
pub const UNIDENTIFIED_RULE: &str = "reads-the-sor-unidentified";

/// The override a door records when it ran a probe its own rule
/// refused: which rule, and the operator's stated reason. Recorded in
/// the proof itself, because that is the record every later reader —
/// `--recheck` included — already opens.
pub fn override_record(rule: &str, reason: &str) -> Value {
    serde_json::json!({"rule": rule, "reason": reason})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest is DATA, and the measured absence is in it. A list
    /// that lost its one measured entry would make the check silently
    /// green (CLAUDE.md §Diagnosis: a check nobody reads is a check
    /// that is not running).
    #[test]
    fn the_forge_absence_manifest_carries_the_measured_tool() {
        let tools = forge_absent_tools();
        assert!(
            tools.contains(&"kubectl"),
            "host-absent-tools.txt lost the tool f9304366 measured: {tools:?}"
        );
        for t in &tools {
            assert!(
                !t.contains('#') && !t.contains(' ') && !t.contains('/'),
                "a manifest line must be a bare tool name, got {t:?}"
            );
        }
    }

    /// The check reads COMMAND POSITION, not the whole line: a probe
    /// that merely greps for the word runs fine on the forge and is not
    /// refused. A refusal a builder cannot act on is worse than the
    /// arrival failure it replaces.
    #[test]
    fn a_probe_that_only_mentions_an_absent_tool_is_allowed() {
        assert_eq!(
            needs_absent_tool("boss-sor-read /api/estate/nodes | grep -c kubectl"),
            None
        );
    }

    /// `boss` joined the absent list on 2026-09-09, and the risk it
    /// brings is that the forge's checkout LIVES at /home/david/boss —
    /// so nearly every honest probe names that path as an ARGUMENT.
    /// Command position is what keeps those legal; if this ever
    /// regresses, the refusal lands on almost every probe anyone
    /// writes, which is far worse than the arrival failure it exists
    /// to prevent.
    #[test]
    fn the_forge_checkout_path_is_an_argument_not_a_command() {
        for probe in [
            "grep -m1 -o \"one run in twenty-five\" /home/david/boss/crates/core/boss-jobs/tests/station_boot_log.rs",
            "grep -c '^COPY infra/estate' /home/david/boss/infra/oss-quickstart/Dockerfile",
            "test -f /home/david/boss/infra/forge/checkout-lock.sh && echo claim-ok",
            "/home/david/boss/infra/lint/a-boot-check-cannot-fail-the-boot.sh",
        ] {
            assert_eq!(
                needs_absent_tool(probe),
                None,
                "the checkout path is not an invocation: {probe}"
            );
        }
    }

    /// And the invocation it was added for is still caught. Measured
    /// on the forge the same day: `boss rerail --help` exited 127 with
    /// `bash: line 1: boss: command not found`, on a car the arrival
    /// rule had probed unattended.
    #[test]
    fn invoking_the_boss_binary_on_the_forge_is_refused() {
        for probe in [
            "boss rerail --help",
            "cd /home/david/boss && boss receipt main",
            "echo x $(boss orient)",
        ] {
            assert_eq!(
                needs_absent_tool(probe),
                Some("boss"),
                "the forge has no boss binary: {probe}"
            );
        }
    }

    /// Every spelling the measured instances used: the env var, the
    /// in-cluster service DNS, the LAN address and port, and a bare
    /// `/api/` path through the gateway.
    #[test]
    fn an_unidentified_read_is_seen_in_every_spelling_that_was_measured() {
        for probe in [
            "curl -fsS $BOSS_JOBS_URL/api/jobs?kind=pr-train&status=open | grep -q b9302f90",
            "curl -sf http://boss-jobs-internal.boss.svc.cluster.local:7900/api/yard/status | grep -q trains",
            "curl -fsS http://10.20.0.34:7900/api/jobs/health | grep -q ok",
            "python3 -c \"import urllib.request,os; print(urllib.request.urlopen(os.environ['BOSS_JOBS_URL']+'/api/yard/status').read())\" | grep -q trains",
            "wget -qO- $BOSS_JOBS_URL/api/stations/loading-dock/queue | grep -q x",
        ] {
            assert!(
                reads_the_sor_unidentified(probe).is_some(),
                "an unidentified read of the system of record: {probe}"
            );
        }
    }

    /// THE MENTION-ONLY GUARD, the same one the absent-tool scan needs.
    /// Naming the URL is not reading it, and the named reader is not
    /// this rule's business.
    #[test]
    fn a_mention_and_a_named_reader_are_both_allowed() {
        for probe in [
            "grep -c BOSS_JOBS_URL infra/forge/run-car-probe.sh",
            "test -n \"$BOSS_JOBS_URL\" && echo claim-ok",
            "boss-sor-read /api/yard/status | grep -q dock_depth",
            "curl -fsS -H \"x-boss-user: $BOSS_SOR_USER\" $BOSS_JOBS_URL/api/yard/status | grep -q x",
        ] {
            assert_eq!(
                reads_the_sor_unidentified(probe),
                None,
                "not an unidentified read: {probe}"
            );
        }
    }

    /// A DOOR-FED PARSER, verbatim off car 4ef79606. `boss-api` does the
    /// read; `python3` is handed its stdout on a pipe and only parses.
    const THE_DOOR_FED_PARSER: &str = r#"boss-api GET "/api/jobs?kind=maintenance-ml-inference-batch&limit=5" | python3 -c "
import json,sys
d=json.load(sys.stdin)
rows=d.get(\"data\",d.get(\"jobs\",[]))
ok=[j for j in rows if j.get(\"status\")==\"closed\" and any(s[\"spec_slug\"]==\"run\" and s[\"status\"]==\"completed\" and (s.get(\"metadata\") or {}).get(\"result\")==\"ok\" for s in j.get(\"steps\",[]))]
print(\"mlbatch:closed-ok\" if ok else \"mlbatch:NONE\")
""#;

    /// A GATEWAY SESSION, verbatim off car 9e2372cf: POST the auth
    /// endpoint, keep the `Set-Cookie`, send it with `-b` on the read.
    const THE_SESSION_PROBE: &str = r#"G=http://10.20.0.30
SESS=$(curl -s -m 10 -i -X POST "$G/api/auth/guest" | grep -i '^set-cookie: boss_session=' | sed 's/^[Ss]et-[Cc]ookie: //; s/;.*//')
[ -n "$SESS" ] || { echo "PROBE CANNOT IDENTIFY ITSELF: no guest session from $G/api/auth/guest - a reachability/identity failure, NOT a verdict on the claim"; exit 1; }
code=$(curl -s -m 15 -b "$SESS" -o /tmp/rulesprobe.$$ -w '%{http_code}' "$G/api/dispatcher/rules") || { echo "PROBE CANNOT READ $G/api/dispatcher/rules (curl exit $?)"; rm -f /tmp/rulesprobe.$$; exit 1; }
[ "$code" = "200" ] || { echo "PROBE CANNOT READ the rules surface: HTTP $code - NOT a verdict on the claim"; rm -f /tmp/rulesprobe.$$; exit 1; }
echo "ONE-HOME-FOR-A-DISPATCHER-RULE""#;

    /// THE SHAPE THE DOORS STEER PEOPLE TOWARD MUST NOT BE REFUSED. A
    /// door does the read and a parser is fed its stdout — measured 28
    /// times across 449 cars that recorded a probe, five of which needed
    /// `--probe-anyway` on 2026-09-11 (backlog 5dc5159d). `python3` is
    /// in [`HTTP_CLIENTS`] because the first measured unidentified read
    /// was `urllib.request.urlopen`; a `python3` handed a pipe is not
    /// that, and refusing it made the escape hatch routine.
    #[test]
    fn a_parser_fed_by_an_identified_door_is_not_performing_the_read() {
        for probe in [
            THE_DOOR_FED_PARSER,
            "boss-api GET /api/yard/status | python3 -c 'import json,sys; print(json.load(sys.stdin)[\"conductor\"][\"last_verb\"])'",
            "body=$(boss-api GET '/api/jobs?kind=gate-run&limit=40'); printf '%s' \"$body\" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d[\"data\"]))'",
            "boss-sor-read /api/workflows | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))'",
        ] {
            assert_eq!(
                reads_the_sor_unidentified(probe),
                None,
                "the door does the read; the parser is fed its stdout: {probe}"
            );
        }
    }

    /// AND THE PYTHON THAT DOES ITS OWN READ IS STILL REFUSED — the
    /// first measured instance (61085a9e) and every other spelling of a
    /// python that can reach the network. The carve-out above keys on
    /// the ABSENCE of any such facility in the text, so a python with
    /// one of them is a client again whether or not a door is also
    /// present.
    #[test]
    fn a_python_that_can_reach_the_network_is_still_a_client() {
        for probe in [
            "python3 -c \"import urllib.request,os; print(urllib.request.urlopen(os.environ['BOSS_JOBS_URL']+'/api/yard/status').read())\" | grep -q trains",
            "python3 -c 'import requests,os; print(requests.get(os.environ[\"BOSS_JOBS_URL\"]+\"/api/jobs\").json())' | grep -q x",
            "python3 -c 'import http.client; c=http.client.HTTPConnection(\"boss-jobs-internal\",7900); c.request(\"GET\",\"/api/yard/status\")'",
            // A door in the text does not launder a python that reads.
            "boss-api GET /api/workflows >/dev/null; python3 -c \"import urllib.request; urllib.request.urlopen('$BOSS_JOBS_URL/api/yard/status')\"",
        ] {
            assert!(
                reads_the_sor_unidentified(probe).is_some(),
                "this python performs the read itself: {probe}"
            );
        }
    }

    /// A GATEWAY SESSION IS AN IDENTITY, and a measured one. 2026-09-11,
    /// one moment, three readers: a guest session read `total` 22 / 56 /
    /// 703 on open ship-a-change, open backlog-item and gate-run —
    /// IDENTICAL to the operator's own door — while the same queries
    /// unidentified against the jobs API's own port answered 0 / 0 / 0.
    #[test]
    fn a_gateway_session_obtained_from_the_server_is_an_identity() {
        assert_eq!(
            reads_the_sor_unidentified(THE_SESSION_PROBE),
            None,
            "a session the gateway ISSUED is a named reader: {THE_SESSION_PROBE}"
        );
    }

    /// WHY THAT IS NOT THE FORGEABLE-HEADER HOLE. The rule keys on
    /// [`SOR_USER_VAR`] rather than a header NAME so a probe cannot
    /// satisfy it by writing its own privileged literal. A cookie is a
    /// different object: measured the same day, `-b
    /// 'boss_session=iamtheoperator'` against the gateway answered
    /// **401**, byte-identical to sending no cookie at all. A forged
    /// header is accepted by a trusting upstream; a forged session is
    /// refused at the door, so this carve-out cannot produce a false
    /// PASS — only a loud failure. That is what makes it safe, and it
    /// is why the carve-out requires the text to OBTAIN a session
    /// (`/api/auth/…`) as well as send one.
    #[test]
    fn a_cookie_flag_alone_is_not_a_session() {
        for probe in [
            "curl -fsS -b 'boss_session=iamtheoperator' $BOSS_JOBS_URL/api/yard/status | grep -q trains",
            "curl -fsS -b \"$SESS\" http://10.20.0.30/api/dispatcher/rules | grep -q authored_registry",
        ] {
            assert!(
                reads_the_sor_unidentified(probe).is_some(),
                "nothing in this text obtained a session: {probe}"
            );
        }
    }

    /// AND A SESSION DOES NOT EXCUSE A READ THAT BYPASSES THE GATEWAY.
    /// The cookie identifies a reader AT THE GATEWAY; the jobs API's own
    /// port does not read cookies, so the same text pointed at
    /// `$BOSS_JOBS_URL` is the 61085a9e defect with a session fetched
    /// beside it. The carve-out is therefore refused the moment a
    /// DIRECT spelling appears.
    #[test]
    fn a_session_does_not_excuse_a_read_past_the_gateway() {
        let probe = "SESS=$(curl -s -i -X POST http://10.20.0.30/api/auth/guest | sed -n 's/^set-cookie: //p'); \
                     curl -fsS -b \"$SESS\" $BOSS_JOBS_URL/api/jobs?kind=pr-train&status=open | grep -q '\"total\":0'";
        assert_eq!(
            reads_the_sor_unidentified(probe),
            Some("curl"),
            "a gateway session says nothing about a read on the jobs API's own port"
        );
    }

    /// WHAT THIS RULE STILL REFUSES ON PURPOSE, pinned so the next
    /// reader knows it is a decision and not an oversight (5dc5159d
    /// asked for three carve-outs and got two).
    ///
    /// Both texts are route-SHAPE probes: they read 401-vs-404 through
    /// the gateway, where an unidentified reader cannot turn a routed
    /// path into an unrouted one, and both guard that with a control
    /// that fails closed. They are correct, and they keep their
    /// override — because what makes them safe is their CONTROL, which
    /// is program logic, not a target a text scan can recognise. A
    /// carve-out wide enough to admit them (a `curl` whose assertions
    /// are status codes) would also admit an absence assertion over
    /// policy-scoped data, which is the false pass this whole rule
    /// exists to stop.
    #[test]
    fn a_route_shape_probe_still_carries_an_override() {
        let shape = "G=http://10.20.0.30\n\
                     ctl=$(curl -s -m 10 -o /dev/null -w '%{http_code}' \"$G/api/yard/status\"); \
                     [ \"$ctl\" = \"401\" ] || exit 1\n\
                     code=$(curl -s -m 10 -o /tmp/p.$$ -w '%{http_code}' \"$G/api/design/flush-jobs\"); \
                     [ \"$code\" = \"404\" ] || exit 1\n\
                     echo FLUSH-PIPELINE-ROUTES-ARE-GONE";
        assert_eq!(
            reads_the_sor_unidentified(shape),
            Some("curl"),
            "left deliberately refused: the control is logic, not a target"
        );
    }

    /// THE PROBE THAT COST 18 HOURS, verbatim off car a0ab90a5's
    /// `proof_probe` (4fccc595). Both shapes are in it: a `jq -e` whose
    /// success branch is `empty`, and a bare `|| exit 1` that overwrote
    /// jq's 4 with a 1.
    const THE_INVERTED_PROBE: &str = "for k in maintenance-backup maintenance-audit-integrity maintenance-ledger-replay; do curl -fsS \"$BOSS_JOBS_URL/api/workflows/$k\" | jq -e --arg k \"$k\" '(.data // .) as $w | if ($w.category==\"platform\" and ($w.description|type)==\"string\" and ($w.description|length)>0) then empty else error(\"\\($k): category=\\($w.category) description=\\($w.description)\") end' || exit 1; done; echo MAINTENANCE-PROTOCOLS-STATE-THEIR-CATEGORY";

    /// THE PROBE THAT REPLACED IT, also verbatim off the car's
    /// `reproof`. It asserts positively (`or error(…)`), prints a token,
    /// and keeps the status before collapsing it — so neither check may
    /// fire on it. A check that flags the CORRECT shape teaches the next
    /// builder to ignore it.
    const THE_PROBE_THAT_REPLACED_IT: &str = "for k in maintenance-backup maintenance-audit-integrity maintenance-ledger-replay; do body=$(boss-api GET \"/api/workflows/$k\" 2>/dev/null) || { echo \"PROBE CANNOT READ $k via boss-api (exit $?) - a reachability failure, NOT a verdict on the claim\"; exit 1; }; printf '%s' \"$body\" | jq -e --arg k \"$k\" '(.data // .) as $w | (($w.category==\"platform\") and (($w.description|type)==\"string\") and (($w.description|length)>0)) or error(\"\\($k): category=\\($w.category) description=\\($w.description)\")' >/dev/null || { echo \"CLAIM FAILS for $k (jq exit $?)\"; exit 1; }; done; echo MAINTENANCE-PROTOCOLS-STATE-THEIR-CATEGORY";

    /// The measured instance is seen, in the text the car actually
    /// carried — flags before the filter, `--arg` values in between, and
    /// the whole thing inside a `for` loop and a pipeline.
    #[test]
    fn the_inverted_jq_e_that_cost_eighteen_hours_is_seen() {
        assert!(
            asserts_its_own_negation(THE_INVERTED_PROBE),
            "the shape 4fccc595 measured must be detectable: {THE_INVERTED_PROBE}"
        );
        assert_eq!(
            rewrites_its_exit_status(THE_INVERTED_PROBE),
            Some(1),
            "`|| exit 1` replaced jq's 4, which is the number that named the cause"
        );
    }

    /// And the probe that FIXED it is clean on both checks.
    #[test]
    fn the_probe_that_replaced_it_trips_neither_check() {
        assert!(
            !asserts_its_own_negation(THE_PROBE_THAT_REPLACED_IT),
            "`or error(…)` asserts positively: {THE_PROBE_THAT_REPLACED_IT}"
        );
        assert_eq!(
            rewrites_its_exit_status(THE_PROBE_THAT_REPLACED_IT),
            None,
            "`|| {{ echo …; exit 1; }}` keeps the evidence before collapsing the status"
        );
    }

    /// `-e` is what makes `empty` a failure, so neither half alone is
    /// the shape. A `jq` without the flag prints nothing and exits 0; an
    /// `-e` over a filter that emits a value is the normal, correct use.
    #[test]
    fn neither_half_of_the_inverted_shape_is_the_shape_alone() {
        for probe in [
            "curl -fsS x | jq 'if (.a==1) then empty else error(\"no\") end'",
            "curl -fsS x | jq -e '.a == 1'",
            "curl -fsS x | jq -e --arg k v '.a == $k'",
            "grep -c empty infra/forge/run-car-probe.sh",
        ] {
            assert!(
                !asserts_its_own_negation(probe),
                "not the inverted shape: {probe}"
            );
        }
    }

    /// Every spelling of the flag that makes `empty` fail: bare, bundled
    /// with another short flag, long, and written AFTER an `--arg` pair
    /// (which is why the flag walk steps over flag values instead of
    /// stopping at the first non-flag word).
    #[test]
    fn the_exit_status_flag_is_seen_in_every_spelling() {
        for probe in [
            "jq -e 'if .a then empty else error(\"x\") end' f.json",
            "jq -re 'if .a then empty else error(\"x\") end' f.json",
            "jq --exit-status 'if .a then empty else error(\"x\") end' f.json",
            "jq --arg k v -e 'if .a == $k then empty else error(\"x\") end' f.json",
        ] {
            assert!(asserts_its_own_negation(probe), "the shape: {probe}");
        }
    }

    /// A bare `|| exit n` is the reduction; anything that SPEAKS first
    /// is not. `exit 0` is not reported either — it is a probe choosing
    /// to pass, which is a different defect and not this one's business.
    #[test]
    fn only_a_bare_rewrite_of_the_exit_status_is_reported() {
        assert_eq!(rewrites_its_exit_status("false || exit 7"), Some(7));
        assert_eq!(
            rewrites_its_exit_status("a && b || exit 1; echo x"),
            Some(1)
        );
        assert_eq!(
            rewrites_its_exit_status("false || { echo \"jq said $?\"; exit 1; }"),
            None
        );
        assert_eq!(rewrites_its_exit_status("false || echo claim:no"), None);
        assert_eq!(rewrites_its_exit_status("true || exit 0"), None);
        assert_eq!(
            rewrites_its_exit_status("boss-sor-read /api/x | grep -q y"),
            None
        );
    }

    /// The override record is the same shape wherever a door writes it,
    /// so a reader looking for overridden proofs has one thing to look
    /// for.
    #[test]
    fn an_override_record_names_its_rule_and_its_reason() {
        let r = override_record(UNIDENTIFIED_RULE, "measured: the header is supplied");
        assert_eq!(r["rule"], UNIDENTIFIED_RULE);
        assert_eq!(r["reason"], "measured: the header is supplied");
    }
}
