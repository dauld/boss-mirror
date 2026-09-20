//! WHICH PROBES ARE LEGAL — one definition, every door.
//!
//! A car records a probe (`boss_jobs::car::PROOF_PROBE`) and several
//! things later RUN it: `boss gate --park-probe` admits the text,
//! `boss prove --probe / --from-car / --recheck` runs it by hand, the
//! `jobs.run-car-probes` rule ships it to the forge on arrival, and
//! `boss prove --from-car --unattended` executes it there. The rules about
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
//! ([`rewrites_its_exit_status`]), a `-lt` on a variable nothing checked
//! was a number ([`compares_an_unguarded_number`]) — which is also true
//! everywhere.
//!
//! Each door applies the first only about the host it is actually
//! sending the probe to, and the other two always. The second REFUSES,
//! because it fails open: the probe passes, the absence assertion is
//! green against a world it was never allowed to see, and a car closes
//! on a proof of nothing. The third only WARNS, because it fails
//! closed — `boss prove` records nothing on a nonzero exit, so the worst
//! it does is strand a car and misdescribe why (18 hours of that,
//! 4fccc595) — and because its detectors are coarse text scans a
//! false refusal would be too expensive for. The one third-kind shape
//! that REFUSES is a git date read with its offset
//! ([`reads_git_time_with_an_offset`]): a string compare of
//! mixed-offset timestamps lies in both directions (c0ac92b8), so it
//! is judged the way the second question is.
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
/// (`boss prove --from-car --unattended`, as david, in /home/david/boss, when
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

/// The two variables the CLI reads to learn WHO is running it
/// (boss-cli `identity.rs`: the env var, then the file the second
/// names). Spelled here rather than imported because that crate is an
/// orchestrator this one cannot depend on; the CLI's own test on the
/// pair is the pin.
const ACTOR_VARS: [&str; 2] = ["BOSS_ACTOR", "BOSS_ACTOR_FILE"];

/// A PROBE PROVES, IT DOES NOT ACT — and the variable a probe would
/// have to set to act, if it spells one.
///
/// `boss` left the forge's absence list on 2026-09-18 (H8 car 1,
/// 9f00a805, installed the CLI there; backlog 8a1fcd22 retired the
/// line), so a recorded probe may shell to a verb. What keeps that a
/// READ is not a list of read-safe verbs — the CLI holds no such
/// classification of its subcommands, and the split runs by flag as
/// often as by verb — but an actor: the probe's env names none
/// (the unattended door hands it exactly `BOSS_JOBS_URL`,
/// `BOSS_PROBE_NOTFOUND`, [`SOR_USER_VAR`], `BOSS_SOR_PORTS`,
/// [`CAR_MERGE_REF_VAR`], [`CAR_CONVERGED_AT_VAR`] and
/// `PATH`), and the CLI refuses an unnamed WRITE by its own rule while
/// an unnamed READ goes out signed `operator:unidentified` under the
/// platform's own read role — `audit-readonly`, the one [`SOR_USER_VAR`]
/// carries, full-width on every list and 403 on every write (backlog
/// d843abf2; until 2026-09-19 it was the operator's platform-admin
/// header) — not the header-less narrowed one 61085a9e measured. So
/// the probe's TEXT is
/// the only place an actor could come from, and a text that assigns
/// one is refused naming the variable. A mention is not an assignment:
/// the CLI's own refusal names `BOSS_ACTOR`, and a probe may grep for
/// it.
pub fn names_an_actor(probe: &str) -> Option<&'static str> {
    probe
        .split_whitespace()
        .map(|w| w.trim_start_matches(['"', '\'', '(', '{', ';', '&', '|']))
        .find_map(|w| {
            ACTOR_VARS
                .iter()
                .find(|v| w.strip_prefix(*v).is_some_and(|rest| rest.starts_with('=')))
                .copied()
        })
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

/// WHEN A PROBE COMPARES A NUMBER IT NEVER CHECKED WAS ONE — the fourth
/// shape, and like the two before it one that fails closed.
///
/// THE DEFECT (backlog 0df3af1c, measured 2026-09-14T00:00:25Z on
/// david-asus-minipc). Two landed cars (dd1d872d, 2e4d3bce) and the
/// daily recheck ran probes of the form
/// `b=$(… | jq -r '… | first | .build_s'); if [ "${b:-9999}" -lt 200 ]`.
/// The query matched nothing, so `first` was `null` and `jq -r` printed
/// the four-character string `null` — which is not empty, so the `:-`
/// default did nothing — and `[` wrote `[: null: integer expression
/// expected` to stderr, exited 2, and fell into the else branch. The
/// cars sat UNPROVEN behind a stderr nobody reads until an operator
/// rewrote both probes by hand at 16:36Z.
///
/// Returns the NAME of the variable under the first unguarded numeric
/// test, so the warning can say which one to guard, or `None` when
/// there is no such test or the text guards against a non-number
/// somewhere. A guard is one of the two things that actually stop the
/// string reaching `[`:
///
/// - a jq-side `// empty` or `select(. != null)` on the VALUE, so that
///   a missing number prints nothing at all; or
/// - a shell-side non-digit check — `case "$b" in ''|*[!0-9]*) …` or
///   `[[ "$b" =~ ^[0-9]+$ ]]` — before the test.
///
/// THREE THINGS DELIBERATELY NOT COUNTED, because both measured probes
/// carried all three and failed anyway: a `${b:-9999}` default (guards
/// EMPTY, not `null`); a `select(.field != null)` inside the array (the
/// array is then empty and `first` of it is still `null`); and an
/// `exit 75` in the else branch (reached only after the test has
/// already errored). Any one of them counting would have exempted the
/// probes this exists to catch.
///
/// DELIBERATELY COARSE, like its two siblings, and a warning because of
/// it: a `case` anywhere in the text counts for every test in it, and a
/// `[ "$n" -lt 3 ]` on a variable the probe set from `wc -l` is
/// reported though it cannot be `null`. The cost when it is wrong is one
/// line a builder reads and ignores; the cost of the alternative is a
/// shell parser, or a car unproven for a day.
pub fn compares_an_unguarded_number(probe: &str) -> Option<&str> {
    if guards_against_a_non_number(probe) {
        return None;
    }
    NUMERIC_TEST_OPERATORS.iter().find_map(|op| {
        probe
            .match_indices(op)
            .filter(|(i, _)| {
                // The operator as a whole word: `-lt` and not `-lte`,
                // and not the tail of `--lt`.
                let before = probe[..*i].chars().next_back();
                let after = probe[i + op.len()..].chars().next();
                before.is_some_and(char::is_whitespace) && after.is_none_or(char::is_whitespace)
            })
            .find_map(|(i, _)| tested_variable(&probe[..i]))
    })
}

/// The shell's six integer comparisons — the operators `[` and `[[`
/// refuse a non-integer operand for.
const NUMERIC_TEST_OPERATORS: [&str; 6] = ["-lt", "-gt", "-le", "-ge", "-eq", "-ne"];

/// The variable a test's left operand expands, if the operand IS a
/// variable and the word in front of it is a test command. `head` is
/// the text up to the operator.
///
/// `"${b:-9999}"`, `"$b"`, `${s}` and `$s` all name their variable; a
/// literal (`[ 1 -lt 2 ]`) or a substitution (`$(wc -l)`) names none. A
/// `!` in front of the operand is stepped over, and `test` counts
/// alongside the two brackets because it is the same builtin.
fn tested_variable(head: &str) -> Option<&str> {
    let mut words = head.split_whitespace().rev();
    let operand = words.next()?;
    let command = words.find(|w| *w != "!")?;
    if !matches!(command, "[" | "[[" | "test") {
        return None;
    }
    let name = operand
        .trim_matches('"')
        .strip_prefix('$')?
        .trim_start_matches('{');
    let end = name
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(name.len());
    (end > 0).then(|| &name[..end])
}

/// Does the text, anywhere, stop a non-number before it reaches a
/// test? Whitespace is dropped first so `select(. != null)` and
/// `select(.!=null)` read the same; `select(.field != null)` does not,
/// and must not (see [`compares_an_unguarded_number`]).
fn guards_against_a_non_number(probe: &str) -> bool {
    let packed: String = probe.split_whitespace().collect();
    packed.contains("//empty")
        || packed.contains("select(.!=null)")
        || packed.contains("[!0-9]")
        || packed.contains("[^0-9]")
        || (packed.contains("=~") && packed.contains("[0-9]"))
}

/// WHEN A PROBE READS GIT TIME AS A STRING WITH AN OFFSET — the fifth
/// shape, and unlike the three before it one that fails OPEN, so it is
/// REFUSED where they are warned about.
///
/// THE DEFECT (backlog c0ac92b8, measured 2026-09-18 on car 746a1fac).
/// The arrival probe asked whether the newest `retire` in the audit
/// tail came AFTER the train's commit, and asked it as a string
/// compare: `git log --format=%cI` on one side, the audit row's
/// timestamp on the other. The conductor writes the train commit with
/// a -07:00 offset (`2026-09-18T07:43:00-07:00`) and the audit log
/// writes UTC (`2026-09-18T12:18:29+00:00`), so `12:18` read as later
/// than `07:43` — though 07:43-07:00 is 14:43Z, two hours LATER. The
/// probe ran 17 s before the operator's retire, saw only a retire that
/// predated the fix, and should have said not-yet; it said FAILED, and
/// the car stood red in the shed until an operator re-ran it by hand.
/// With the offsets the other way round the same compare answers PASS
/// for an event that never happened — a string compare of mixed-offset
/// timestamps lies in BOTH directions, which is why this is a refusal
/// and not a warning: it can record a proof of nothing.
///
/// Returns the first offset-bearing git date spelling in the text.
/// DELIBERATELY THE TOKEN, NOT THE COMPARE: whether the string reaches
/// a `[ … \> … ]` would take a shell parser to know honestly, and the
/// fix is the same either way — `git log --format=%ct` is already an
/// epoch, `date -u -d "$ts" +%s` makes the other side one, and two
/// integers compare with `-gt`. Guard the empty case FIRST: `date -d ''`
/// answers today's midnight, not an error (the reclaim-refs builder hit
/// that sibling on the same day).
pub fn reads_git_time_with_an_offset(probe: &str) -> Option<&'static str> {
    GIT_TIME_WITH_AN_OFFSET
        .iter()
        .copied()
        .find(|t| probe.contains(t))
}

/// The `git log` spellings that print a timestamp carrying the
/// committer's or author's UTC offset — ISO strict (`%cI`), ISO-like
/// (`%ci`), their author twins, and the `--date=` forms that make `%cd`
/// / `%ad` print the same (`iso`, `iso-strict`, `iso8601`, `rfc2822`).
/// `%ct` / `%at` and `--date=unix` print epoch seconds and are not here.
pub const GIT_TIME_WITH_AN_OFFSET: [&str; 6] =
    ["%cI", "%ci", "%aI", "%ai", "--date=iso", "--date=rfc"];

/// THE REWRITE A DATED CLAIM TAKES, as a literal so the ONE copy can be
/// `concat!`ed into [`GIT_TIME_STRING_EVIDENCE`] as well as stand on
/// its own as [`CAR_INSTANT_RECIPE`] (CLAUDE.md §9a: a recipe that
/// lived in two texts would drift, and this one is what a builder
/// copies).
macro_rules! car_instant_recipe {
    () => {
        "  since=${BOSS_CAR_CONVERGED_AT}\n  \
case ${since:-empty} in empty|*[!0-9]*) echo 'not yet: this car has not converged here'; exit 75;; esac\n  \
seen=$(date -u -d \"$ts\" +%s)\n  \
[ \"$seen\" -gt \"$since\" ] && echo claim:ok"
    };
}

/// The measured evidence for [`reads_git_time_with_an_offset`], in one
/// copy, quoted by every door that refuses on it. The doors differ in
/// what to do instead; they must not differ on what happened
/// (CLAUDE.md §9a).
pub const GIT_TIME_STRING_EVIDENCE: &str = concat!(
    "\
Measured 2026-09-18 (c0ac92b8, car 746a1fac): the arrival probe compared `git log \
--format=%cI` — the train commit's committer date, which the conductor writes with a \
-07:00 offset (2026-09-18T07:43:00-07:00) — against the audit tail's UTC timestamps as \
STRINGS, so 12:18Z read as later than 07:43(-07:00), which is 14:43Z. The probe ran 17 s \
before the operator's act, saw a retire that predated the fix, and answered FAILED where \
not-yet was true; the car stood red in the shed until an operator re-ran it. With the \
offsets the other way round the same compare answers PASS for an event that never \
happened.\n\
Compare epochs, never ISO strings with mixed offsets — and date the cutoff from the car's \
own converged instant, never from a HEAD that moves with every train (a92571a6):\n  \
ts=$(boss-sor-read '/api/...' | jq -r '... // empty')\n  \
[ -n \"$ts\" ] || { echo 'not yet: no <event> recorded'; exit 75; }\n",
    car_instant_recipe!(),
    "\nThe empty guard comes FIRST: `date -d ''` answers today's midnight, not an error."
);

/// The rule id a door records when an operator overrides a refusal on
/// it. Short, stable, and greppable across recorded proofs — an
/// override nobody can find later is the defect it was meant to avoid.
pub const UNIDENTIFIED_RULE: &str = "reads-the-sor-unidentified";

/// The rule id for [`reads_git_time_with_an_offset`], recorded the same
/// way when overridden.
pub const GIT_TIME_RULE: &str = "reads-git-time-with-an-offset";

/// THE CAR'S OWN CONVERGENCE INSTANT, in epoch seconds — promised to
/// every recorded probe by the doors that run one (`boss prove`, both
/// unattended and by hand), resolved from the car's `merge_ref` in the
/// checkout the probe runs in. FIXED: a car converges once, and the
/// commit time of its own merge does not move afterwards.
///
/// A probe that needs "did the qualifying event happen after my change
/// landed?" compares against this and nothing else. The variable is
/// ABSENT when the car's merge is not in this checkout — which is the
/// honest not-yet, and the reason the recipe guards it first.
pub const CAR_CONVERGED_AT_VAR: &str = "BOSS_CAR_CONVERGED_AT";

/// The merge commit [`CAR_CONVERGED_AT_VAR`] was read from, handed over
/// beside it so a probe can name it in its own not-yet line.
pub const CAR_MERGE_REF_VAR: &str = "BOSS_CAR_MERGE_REF";

/// WHEN A PROBE DATES ITS CUTOFF FROM A TARGET THAT MOVES — the sixth
/// shape, and a warning rather than a refusal because it fails CLOSED:
/// the probe answers 75 (not yet) forever, never a false green.
///
/// THE DEFECT (backlog a92571a6, measured 2026-09-19). A probe of the
/// common shape asks whether its qualifying event happened after the
/// converged checkout's HEAD — `since=$(git log -1 --format=%ct HEAD)`
/// — and the forge's checkout converges on main after EVERY train,
/// about 28 a day. So the goalpost advances every ~50 minutes while
/// the car sits, and the claim silently becomes "this change worked
/// more recently than any other change landed", which is not what
/// proven means. Car 372ac8fd answered not-yet twice with its event
/// having fired both times, its message moving from a 2026-09-18T18:23
/// packet to a 2026-09-19T08:52 one because HEAD had moved further
/// each time. Worse for anything rarer than a train: car 1e7c5a98
/// waits on a DAILY sweep and compared against a HEAD forty minutes
/// old, so it is not slow to prove but effectively unprovable — any
/// car whose qualifying event is less frequent than convergence is
/// starved by construction.
///
/// Returns the offending command, as written. DELIBERATELY COARSE like
/// its siblings: a segment that runs `git`, prints an epoch (`%ct`,
/// `%at`, `--date=unix`) and names `HEAD` as a REVISION is reported.
/// `git show HEAD:<path>` reads a file and is left alone — the "has my
/// change converged?" leg of the same probe, which is correct.
pub fn compares_against_a_moving_head(probe: &str) -> Option<&str> {
    const EPOCH_FORMATS: [&str; 3] = ["%ct", "%at", "--date=unix"];
    probe
        .split(['|', '&', ';', '\n', '(', ')', '`', '{', '}'])
        .map(str::trim)
        .find(|segment| {
            segment.contains("git")
                && EPOCH_FORMATS.iter().any(|f| segment.contains(f))
                && names_head_as_a_revision(segment)
        })
}

/// Does this segment name `HEAD` as a revision rather than as the left
/// half of a `HEAD:<path>` file read? The word must stand alone: not
/// followed by `:`, and not part of a longer word (`AHEAD`, `HEADER`).
fn names_head_as_a_revision(segment: &str) -> bool {
    segment.match_indices("HEAD").any(|(i, _)| {
        let before = segment[..i].chars().next_back();
        let after = segment[i + 4..].chars().next();
        before.is_none_or(|c| !c.is_alphanumeric() && c != '_')
            && after.is_none_or(|c| c != ':' && !c.is_alphanumeric() && c != '_')
    })
}

/// The measured evidence for [`compares_against_a_moving_head`], in one
/// copy, quoted by every door that says it (CLAUDE.md §9a).
pub const MOVING_HEAD_EVIDENCE: &str = "\
Measured 2026-09-19 (a92571a6), running six shed cars by hand on the forge: 3 of 3 \
residual failures shared this root cause, and the frequency of the waited-on event \
predicted it exactly — hourly races the goalpost, daily loses it, twice-daily loses it. \
Two different cars reported the SAME cutoff instant, 2026-09-19T16:00:56, the converged \
HEAD of a train that had landed minutes earlier and had nothing to do with either car.";

/// The promised instant, guarded the way a missing number is guarded
/// everywhere else — because a car whose merge is not in this checkout
/// has not converged, and NOT YET is the true answer there.
pub const CAR_INSTANT_RECIPE: &str = car_instant_recipe!();

/// The measured evidence for [`counts_a_page_it_may_not_have_read`],
/// in one copy, quoted by every door that says it (CLAUDE.md §9a).
pub const TRUNCATED_PAGE_EVIDENCE: &str = "\
Measured 2026-09-20 (e7cf78c6) across every recorded probe: 49 read a list with a page \
size, and 33 of them never ask whether they saw all of it. Car ead4a6ed asked for 300 \
rows against a live total of 345 and the one qualifying row sat in the unread tail, so \
the probe could have answered not-yet forever while the event it waited on had already \
happened; the ops-runner's queue gauge (2cfb4562) reads 100 the same way, and a depth of \
exactly 100 cannot be told from at-least-100. Neither was found by a check. Both were \
found because somebody happened to read the two numbers next to each other.\n\
Take ONE body and judge the page against the list before counting anything in it:\n  \
body=$(boss-sor-read '/api/jobs?kind=gate-run&limit=300')\n  \
rows=$(printf '%s' \"$body\" | jq '.data | length'); seen=$(printf '%s' \"$body\" | jq '.total')\n  \
case ${rows:-empty}/${seen:-empty} in *empty*) echo 'not yet: the read answered nothing'; exit 75;; esac\n  \
[ \"$rows\" -eq \"$seen\" ] || { echo \"not yet: read $rows of $seen — widen the page\"; exit 75; }";

/// The measured evidence for
/// [`greps_a_name_where_a_definition_is_meant`], in one copy, quoted by
/// every door that says it (CLAUDE.md §9a).
pub const MENTION_NOT_DEFINITION_EVIDENCE: &str = "\
Measured 2026-09-20 (e7cf78c6): a probe counted a bare identifier in a source file to \
prove the function it names had landed, and matched the DOC COMMENT above the call site \
instead of the definition — a nonzero count, a green proof, and nothing defined. grep is \
line-based and a name appears on every line that mentions it: the import, the call, the \
comment, the test, the changelog.\n\
Quote what makes it a definition, keyword and all — 'pub fn <name>', 'pub const <NAME>', \
'^<name>()' for a shell function — so the one line that defines it is the only line that \
can match.";

/// WHEN A PROBE COUNTS A PAGE IT MAY NOT HAVE READ — the seventh
/// shape, and the first that can fail OPEN as well as closed.
///
/// THE DEFECT (backlog e7cf78c6, measured 2026-09-20 across all 49
/// recorded probes that read a list: 33 never ask whether they saw all
/// of it). `{"data":[…300 rows…],"total":345}` is a CORRECT response to
/// a page request, and it is byte-identical in shape to the whole list,
/// so a probe that counts the rows has silently answered a smaller
/// question. Car ead4a6ed asked for 300 against a live total of 345
/// with its one qualifying row in the unread tail: it could have said
/// not-yet forever about an event that had already happened. The same
/// read in the ops-runner's queue gauge (2cfb4562) reports 100 as the
/// depth, where a depth of exactly 100 and a depth of "at least 100"
/// are the same sentence.
///
/// Returns the page-size token as written, so the warning can name it.
/// Three conditions, all required, and each one is there to keep a
/// correct probe quiet:
///
/// - a `limit=` in QUERY POSITION (after `?` or `&`), because that is
///   the read that can be truncated;
/// - a COUNT taken client-side (`length`, `wc -l`, `grep -c`), because
///   counting a set is the claim a partial set breaks — a probe that
///   reads `limit=1 … .data[0].at` takes the newest row the server
///   ordered for it and is correct;
/// - and no mention of `total` anywhere, because the one body that
///   carries both numbers is the fix, and a probe that names it has
///   either done the comparison or is about to.
///
/// WARNING, NOT REFUSAL — and the argument is NOT its siblings'. This
/// shape does not always fail closed: a probe asserting an ABSENCE over
/// a truncated page ("no row since the cutoff matches the bad shape")
/// records a false GREEN, which is the direction that made
/// [`reads_git_time_with_an_offset`] a refusal. What keeps it a warning
/// is decidability: `limit=` is right far more often than it is wrong
/// (16 of the 49 measured probes pair it with `.total` already, and a
/// page read with no count at all is correct), the scan is a coarse
/// text match that cannot tell an API read from a `grep` for the
/// literal string `limit=`, and a refusal that fires on a correct probe
/// costs a builder a re-park and teaches them to route around the door.
/// So it is said LOUDLY, at the two doors with a human in front of them
/// — `boss gate --park-probe` and `boss prove` — where the cost of
/// being wrong is one line read and ignored.
pub fn counts_a_page_it_may_not_have_read(probe: &str) -> Option<&str> {
    if has_word(probe, "total") || !counts_the_rows(probe) {
        return None;
    }
    page_size_token(probe)
}

/// Does this text reduce rows to a NUMBER? The three spellings a probe
/// uses: jq's `length`, `wc -l` over the lines, and `grep -c` over
/// them.
fn counts_the_rows(probe: &str) -> bool {
    has_word(probe, "length") || probe.contains("wc -l") || probe.contains("grep -c")
}

/// The `limit=<n>` token in query position, as written. `?`/`&` in
/// front is what separates a page request from the word appearing in
/// prose or in a pattern.
fn page_size_token(probe: &str) -> Option<&str> {
    probe.match_indices("limit=").find_map(|(i, _)| {
        let before = probe[..i].chars().next_back()?;
        if before != '?' && before != '&' {
            return None;
        }
        let rest = &probe[i + "limit=".len()..];
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        (digits > 0).then(|| &probe[i..i + "limit=".len() + digits])
    })
}

/// WHEN A PROBE GREPS A NAME WHERE A DEFINITION IS MEANT — the eighth
/// shape, and the one that fails OPEN in one direction only.
///
/// THE DEFECT (backlog e7cf78c6, measured 2026-09-20). A probe proved a
/// function had landed with `grep -c <name>` over the source file, and
/// matched the DOC COMMENT that names it rather than the definition —
/// a nonzero count, a green proof, and nothing defined. grep is
/// line-based and a name appears on every line that mentions it: the
/// import, the call site, the comment above it, the test, the packet id
/// in a changelog. The count cannot tell them apart; `pub fn <name>`
/// can.
///
/// Returns the bare pattern, so the warning can say which grep to
/// quote. Conditions, each one keeping a correct probe quiet:
///
/// - the pipeline reads a FILE (`git show`), because a definition is a
///   thing a file has — a `grep -c` over an API body is a different
///   claim and is left alone;
/// - the grep JUDGES (`-c`, `-q`, `--count`, `--quiet`), because a grep
///   whose output a human reads is not asserting anything;
/// - and the pattern is a bare identifier with a lowercase letter in it
///   and either an underscore or a capital — `in_flight_claims`,
///   `navCatalog`. A phrase is already specific (`'integer
///   expression'`), a SCREAMING_SNAKE name is nearly always a mention
///   check (`BOSS_JOBS_URL`), and a pattern with punctuation in it
///   (`ls-remote`, `^fn `) is not the shape at all.
///
/// WARNING, NOT REFUSAL, though this one lies in the false-GREEN
/// direction only: a mention can make an absent definition read as
/// present, never the reverse. The reason is that the text does not say
/// which was meant. Counting a MENTION is a legitimate probe — that a
/// call site exists, that a literal still appears in a config, that a
/// name was NOT removed — and no scan can tell it from the defect, so a
/// refusal here would fire on correct probes and would have to carry an
/// override that becomes routine, which CLAUDE.md §Diagnosis says is
/// read by nobody. The cheap true thing is to say it where the author
/// is standing.
pub fn greps_a_name_where_a_definition_is_meant(probe: &str) -> Option<&str> {
    pipelines(probe).into_iter().find_map(|segment| {
        segment
            .contains("git show")
            .then(|| judging_grep_patterns(segment))?
            .into_iter()
            .find_map(bare_identifier)
    })
}

/// The pipelines in a probe: the text split where one command's output
/// STOPS feeding the next — newline, `;`, `&&`, `||` — and deliberately
/// not at `|`, which is the thing that joins `git show` to the `grep`
/// reading it.
fn pipelines(probe: &str) -> Vec<&str> {
    probe
        .split(['\n', ';'])
        .flat_map(|s| s.split("&&"))
        .flat_map(|s| s.split("||"))
        .map(str::trim)
        .collect()
}

/// The pattern of each grep in this segment that judges rather than
/// prints: one per invocation, the first word that is not a flag.
/// `grep -e <pattern>` lands on the same word, since the value of `-e`
/// is exactly the pattern.
fn judging_grep_patterns(segment: &str) -> Vec<&str> {
    let mut patterns = Vec::new();
    let mut words = segment.split_whitespace();
    while let Some(w) = words.next() {
        if w.rsplit('/').next() != Some("grep") {
            continue;
        }
        let mut judges = false;
        for word in words.by_ref() {
            match word.strip_prefix('-') {
                Some(flags) if !word.is_empty() && word != "-" => {
                    judges = judges
                        || match flags.strip_prefix('-') {
                            Some(long) => long == "count" || long == "quiet",
                            None => flags.contains('c') || flags.contains('q'),
                        };
                }
                _ => {
                    if judges {
                        patterns.push(word);
                    }
                    break;
                }
            }
        }
    }
    patterns
}

/// Is this grep pattern a bare NAME — the thing that matches its own
/// mentions? Quotes come off first, and a pattern whose opening quote
/// does not close in the same word is a phrase, not a name.
fn bare_identifier(pattern: &str) -> Option<&str> {
    // The substitution that wraps the pipeline leaves its bracket on
    // the last word: `c=$(git show … | grep -c in_flight_claims)`. None
    // of the three can be part of a name, so taking them off costs
    // nothing and reading the word without them is the whole point.
    let pattern = pattern.trim_matches(['(', ')', '`']);
    let name = match pattern.chars().next()? {
        q @ ('\'' | '"') => pattern
            .strip_prefix(q)
            .and_then(|rest| rest.strip_suffix(q))?,
        _ => pattern,
    };
    let bare = name.len() >= 3 && name.chars().all(|c| c.is_alphanumeric() || c == '_');
    let has_lower = name.chars().any(|c| c.is_lowercase());
    let code_shaped = name.contains('_') || name.chars().any(|c| c.is_uppercase());
    (bare && has_lower && code_shaped).then_some(name)
}

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

    /// And an invocation of a listed tool is still caught, in every
    /// position the shell would run it from. Pinned on `kubectl`, the
    /// tool still measured absent (f9304366), since 2026-09-18: until
    /// then this test ran on `boss`, which left the list that day.
    #[test]
    fn invoking_an_absent_tool_on_the_forge_is_refused() {
        for probe in [
            "kubectl -n boss get deploy boss-jobs",
            "cd /home/david/boss && kubectl get pods -A",
            "echo x $(kubectl get nodes)",
        ] {
            assert_eq!(
                needs_absent_tool(probe),
                Some("kubectl"),
                "the forge has no kubectl: {probe}"
            );
        }
    }

    /// `boss` LEFT the list on 2026-09-18 (backlog 8a1fcd22): H8 car 1
    /// (9f00a805) had the forge's converge install the CLI from the
    /// cluster image at /usr/local/bin/boss, and car 9ec955c3's probe
    /// proved it there at the converged sha. A probe may now shell to
    /// the CLI in command position — that is what the next H8 cars
    /// (shell twins retiring one verb at a time) need to write.
    #[test]
    fn invoking_the_boss_cli_on_the_forge_is_accepted() {
        for probe in [
            "boss receipt main",
            "cd /home/david/boss && boss merged feat/x && echo merged:ok",
            "echo x $(boss orient)",
            "/usr/local/bin/boss --version >/dev/null && echo cli:ok",
        ] {
            assert_eq!(
                needs_absent_tool(probe),
                None,
                "the forge has the CLI since 9f00a805: {probe}"
            );
        }
    }

    /// A PROBE PROVES, IT DOES NOT ACT. The one thing that turns the
    /// forge's `boss` into a writer is an actor: the probe's env names
    /// none (the unattended prove door hands it exactly BOSS_JOBS_URL,
    /// BOSS_PROBE_NOTFOUND, BOSS_SOR_USER, BOSS_SOR_PORTS, the car's own
    /// BOSS_CAR_MERGE_REF and BOSS_CAR_CONVERGED_AT, and PATH), and
    /// the CLI refuses an unnamed write by its own rule
    /// (boss-cli identity.rs). So the probe's TEXT is the only place an
    /// actor could come from, and a text that spells one is refused
    /// naming the variable — in either spelling the CLI reads.
    #[test]
    fn a_probe_that_names_an_actor_is_seen() {
        assert_eq!(
            names_an_actor("BOSS_ACTOR=emp-david boss job close x"),
            Some("BOSS_ACTOR")
        );
        assert_eq!(
            names_an_actor("export BOSS_ACTOR_FILE=/tmp/a; boss gate x"),
            Some("BOSS_ACTOR_FILE")
        );
        assert_eq!(
            names_an_actor("env BOSS_ACTOR=\"$BOSS_SOR_USER\" boss receipt main"),
            Some("BOSS_ACTOR")
        );
        // A MENTION is not an assignment: the CLI's own refusal text
        // names the variable, and a probe may grep for it.
        assert_eq!(
            names_an_actor("boss receipt main 2>&1 | grep -c BOSS_ACTOR"),
            None
        );
        assert_eq!(names_an_actor("boss-sor-read /api/yard/status"), None);
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
            "grep -c BOSS_JOBS_URL infra/ops/ops-runner.sh",
            "test -n \"$BOSS_JOBS_URL\" && echo claim-ok",
            "boss-sor-read /api/yard/status | grep -q dock_depth",
            "curl -fsS -H \"x-boss-user: $BOSS_SOR_USER\" $BOSS_JOBS_URL/api/yard/status | grep -q x",
            // The gateway reader sends no identity BY DESIGN and prints
            // only the status (backlog 240e03f3): the narrowed-world
            // defect this rule refuses is the direct port's, and the
            // gateway answers a stranger 401, loudly — which is the
            // fact such a probe asserts.
            "s=$(boss-gateway-read /api/jobs); case \"$s\" in 401) echo gateway-401:ok;; *) echo \"gateway answered $s\"; exit 1;; esac",
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
            "grep -c empty infra/ops/ops-runner.sh",
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

    /// THE TWO PROBES THAT SAT UNPROVEN ON `null`, verbatim off the
    /// cars' `proof_attempt.probe` (0df3af1c, run 2026-09-14T00:00:25Z on
    /// david-asus-minipc). Both read a number out of JSON with `jq -r`,
    /// both test it with `-lt`, and both carried every shape of guard
    /// that does NOT work: a `select(.build_s != null)` INSIDE the array
    /// (so `first` of an empty array is still `null`), a `${b:-9999}`
    /// default (which fills EMPTY, and `null` is four characters), and an
    /// `exit 75` in the else branch the test never reached. stderr, both:
    /// `bash: line 10: [: null: integer expression expected`.
    const THE_FLOOR_SWEEP_PROBE_THAT_FAILED_ON_NULL: &str = "b=$(boss-sor-read \"/api/jobs?kind=maintenance-cluster-converge&limit=6\" | jq -r \"[.data[] | .steps[] | select(.spec_slug==\\\"run\\\") | .metadata | select(.build_s != null)] | first | .build_s\"); echo \"build_s=$b\"; if [ \"${b:-9999}\" -lt 200 ]; then echo warm-build-kept:ok; else echo \"newest converge built in ${b}s — warm needs a sweep hour to pass without wiping the mounts, then a converge\"; exit 75; fi";

    const THE_IMAGE_BUILD_PROBE_THAT_FAILED_ON_NULL: &str = "b=$(boss-sor-read '/api/jobs?kind=maintenance-cluster-converge&limit=6' | jq -r '[.data[] | .steps[] | select(.spec_slug==\"run\") | .metadata | select(.build_s != null)] | first | \"build_s=\\(.build_s) head=\\(.build_head)\"'); echo \"$b\"; s=${b#build_s=}; s=${s%% *}; if [ \"${s:-9999}\" -lt 300 ]; then echo warm-image-build:ok; else echo \"the newest converge built in ${s}s — a cold cache fill after landing is expected once; the converge after it is the measurement (was 522s before)\"; exit 75; fi";

    /// And the two that REPLACED them by hand at 16:36Z, also verbatim
    /// off the cars' `proof_probe`: the first guards on both sides
    /// (`// empty` in jq AND `case … *[!0-9]*` in the shell), the second
    /// with `first | select(. != null)` in jq and the same `case`. A
    /// check that flags the correct shape teaches the next builder to
    /// ignore it.
    const THE_FLOOR_SWEEP_PROBE_REWRITTEN: &str = "b=$(boss-sor-read \"/api/jobs?kind=maintenance-cluster-converge&limit=60\" | jq -r \"[.data[] | .steps[] | select(.spec_slug==\\\"run\\\") | .metadata | select(.build_s != null)] | first | .build_s // empty\"); echo \"build_s=${b:-none}\"; case \"$b\" in ''|*[!0-9]*) echo \"not yet: no converge in the newest 60 packets recorded a build (every tick was unchanged)\"; exit 75;; esac; if [ \"$b\" -lt 200 ]; then echo warm-build-kept:ok; else echo \"newest converge built in ${b}s — warm needs a sweep hour to pass without wiping the mounts, then a converge\"; exit 75; fi";

    const THE_IMAGE_BUILD_PROBE_REWRITTEN: &str = "b=$(boss-sor-read '/api/jobs?kind=maintenance-cluster-converge&limit=60' | jq -r '[.data[] | .steps[] | select(.spec_slug==\"run\") | .metadata | select(.build_s != null)] | first | select(. != null) | \"build_s=\\(.build_s) head=\\(.build_head)\"'); echo \"${b:-build_s=none}\"; s=${b#build_s=}; s=${s%% *}; case \"$s\" in ''|*[!0-9]*) echo 'not yet: no converge in the newest 60 packets recorded a build (every tick was unchanged)'; exit 75;; esac; if [ \"$s\" -lt 300 ]; then echo warm-image-build:ok; else echo \"the newest converge built in ${s}s — a cold cache fill after landing is expected once; the converge after it is the measurement (was 522s before)\"; exit 75; fi";

    /// The measured instances are seen, and the variable each compares
    /// is NAMED, because the warning has to say which one to guard. The
    /// third text is the packet's own schematic of the pattern, which
    /// is what the daily recheck ran into again.
    #[test]
    fn the_probes_that_failed_on_null_are_seen_with_their_variable_named() {
        assert_eq!(
            compares_an_unguarded_number(THE_FLOOR_SWEEP_PROBE_THAT_FAILED_ON_NULL),
            Some("b"),
            "{THE_FLOOR_SWEEP_PROBE_THAT_FAILED_ON_NULL}"
        );
        assert_eq!(
            compares_an_unguarded_number(THE_IMAGE_BUILD_PROBE_THAT_FAILED_ON_NULL),
            Some("s"),
            "{THE_IMAGE_BUILD_PROBE_THAT_FAILED_ON_NULL}"
        );
        assert_eq!(
            compares_an_unguarded_number(
                "b=$(boss-sor-read /api/jobs | jq -r '.data | first | .build_s'); \
                 if [ \"$b\" -lt 200 ]; then echo claim:ok; fi"
            ),
            Some("b")
        );
    }

    /// The hand rewrites are clean — on either side of the pipe.
    #[test]
    fn the_probes_rewritten_with_a_guard_are_not_reported() {
        for probe in [
            THE_FLOOR_SWEEP_PROBE_REWRITTEN,
            THE_IMAGE_BUILD_PROBE_REWRITTEN,
            // Each guard alone is enough: the jq side …
            "b=$(boss-sor-read /api/x | jq -r '.n // empty'); [ \"$b\" -lt 200 ] && echo claim:ok",
            "b=$(boss-sor-read /api/x | jq -r 'first | select(. != null) | .n'); [ \"$b\" -lt 200 ] && echo claim:ok",
            // … or the shell side, as a glob or as a regex.
            "b=$(boss-sor-read /api/x | jq -r '.n'); case \"$b\" in ''|*[!0-9]*) echo 'not yet: no n'; exit 75;; esac; [ \"$b\" -lt 200 ] && echo claim:ok",
            "b=$(boss-sor-read /api/x | jq -r '.n'); [[ \"$b\" =~ ^[0-9]+$ ]] || { echo 'not yet: no n'; exit 75; }; [[ \"$b\" -lt 200 ]] && echo claim:ok",
        ] {
            assert_eq!(
                compares_an_unguarded_number(probe),
                None,
                "guarded: {probe}"
            );
        }
    }

    /// WHAT DOES NOT COUNT AS A GUARD, each because the failing probes
    /// HAD it. A `${b:-9999}` default fills the empty string and `jq -r`
    /// prints `null`, not nothing. A `select(.field != null)` inside the
    /// array leaves `first` of an empty array as `null`. An `exit 75` in
    /// the else branch is reached only after `[` has already errored on
    /// the string. Counting any of these would have exempted both
    /// measured probes.
    #[test]
    fn a_default_a_field_select_and_a_late_exit_75_are_not_guards() {
        for probe in [
            "b=$(boss-sor-read /api/x | jq -r '.n'); [ \"${b:-9999}\" -lt 200 ] && echo claim:ok",
            "b=$(boss-sor-read /api/x | jq -r '[.[] | select(.n != null)] | first | .n'); [ \"$b\" -lt 200 ] && echo claim:ok",
            "b=$(boss-sor-read /api/x | jq -r '.n'); if [ \"$b\" -lt 200 ]; then echo claim:ok; else echo 'not yet'; exit 75; fi",
        ] {
            assert_eq!(
                compares_an_unguarded_number(probe),
                Some("b"),
                "not a guard: {probe}"
            );
        }
    }

    /// No numeric test on a VARIABLE, no finding — whatever else the
    /// text does. A string test, a `grep -c` piped to nothing numeric, an
    /// operator mentioned in prose or in a jq filter, and a literal on
    /// both sides are all not the shape.
    #[test]
    fn a_probe_without_a_numeric_test_on_a_variable_is_not_reported() {
        for probe in [
            "boss-sor-read /api/workflows/x | grep -q claim:ok",
            "b=$(boss-sor-read /api/x | jq -r '.name'); [ \"$b\" = ready ] && echo claim:ok",
            "git show HEAD:infra/gate.sh | grep -c \"integer expression\" && echo claim:ok",
            "echo 'the -lt test is the shape'; [ 1 -lt 2 ] && echo claim:ok",
            "boss-sor-read /api/x | jq -e '.n < 200' >/dev/null && echo claim:ok",
        ] {
            assert_eq!(
                compares_an_unguarded_number(probe),
                None,
                "not the shape: {probe}"
            );
        }
    }

    /// THE PROBE THAT ANSWERED FAILED FOR A NOT-YET (c0ac92b8, car
    /// 746a1fac), and its spellings: the committer date with an offset,
    /// the author's, and the `--date=` forms that make `%cd` print the
    /// same. Each is named back so the refusal can say which token.
    #[test]
    fn a_probe_that_reads_a_git_date_with_an_offset_is_seen_by_its_token() {
        for (probe, token) in [
            (
                "since=$(git log -1 --format=%cI HEAD); \
                 last=$(boss-sor-read '/api/audit?kind=class.retired&limit=1' | jq -r '.data[0].at // empty'); \
                 [ \"$last\" \\> \"$since\" ] && echo retire:after-landing",
                "%cI",
            ),
            (
                "git log -1 --format='%ci' | grep -q 2026 && echo claim:ok",
                "%ci",
            ),
            (
                "git log -1 --format=%aI | grep -q T && echo claim:ok",
                "%aI",
            ),
            (
                "git log -1 --pretty=%ai | grep -q T && echo claim:ok",
                "%ai",
            ),
            (
                "git log -1 --date=iso-strict --format=%cd | grep -q T && echo claim:ok",
                "--date=iso",
            ),
            (
                "git log -1 --date=rfc2822 --format=%cd | grep -q 2026 && echo claim:ok",
                "--date=rfc",
            ),
        ] {
            assert_eq!(reads_git_time_with_an_offset(probe), Some(token), "{probe}");
        }
    }

    /// The rewrite the refusal names — epochs on both sides, the empty
    /// case guarded first — is clean, and so is a probe that reads no
    /// git date at all, or reads one as an integer.
    #[test]
    fn a_probe_that_compares_epochs_is_not_reported() {
        for probe in [
            "since=$BOSS_CAR_CONVERGED_AT; \
             ts=$(boss-sor-read '/api/audit?kind=class.retired&limit=1' | jq -r '.data[0].at // empty'); \
             [ -n \"$ts\" ] || { echo 'not yet: no retire recorded'; exit 75; }; \
             seen=$(date -u -d \"$ts\" +%s); \
             [ \"$seen\" -gt \"$since\" ] && echo retire:after-landing",
            "git log -1 --format=%at | grep -q . && echo claim:ok",
            "git log -1 --date=unix --format=%cd | grep -q . && echo claim:ok",
            "git show HEAD:infra/gate.sh | grep -c 'integer expression' && echo claim:ok",
            "boss-sor-read /api/yard/status | jq -e '.dock_depth == 1' >/dev/null && echo claim:ok",
        ] {
            assert_eq!(reads_git_time_with_an_offset(probe), None, "clean: {probe}");
        }
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

    /// THE STARVED CUTOFF, NAMED WHERE IT IS STILL CHEAP (backlog
    /// a92571a6). The idiom on three live cars dates the cutoff from
    /// the converged checkout's CURRENT HEAD, which advances with every
    /// train. The scan names the command and leaves the shapes beside
    /// it alone: reading a FILE at HEAD says nothing about time, and a
    /// cutoff taken from the promised variable is the fix itself.
    #[test]
    fn a_cutoff_dated_from_the_moving_head_is_named() {
        let starved = "c=$(git show HEAD:infra/cluster/dev-scratch-reclaim.sh | grep -c ls-remote); \
                       since=$(git log -1 --format=%ct HEAD); \
                       ts=$(boss-sor-read '/api/jobs?kind=x' | jq -r '.data[0].at // empty')";
        assert_eq!(
            compares_against_a_moving_head(starved),
            Some("git log -1 --format=%ct HEAD"),
            "{starved}"
        );
        for probe in [
            "git show -s --format=%ct HEAD~1 | cat",
            "git log -1 --date=unix --format=%cd HEAD",
        ] {
            assert!(compares_against_a_moving_head(probe).is_some(), "{probe}");
        }
        for clean in [
            "git show HEAD:infra/gate.sh | grep -c 'integer expression' && echo claim:ok",
            "since=$BOSS_CAR_CONVERGED_AT; git log -1 --format=%ct $BOSS_CAR_MERGE_REF",
            "boss-sor-read /api/yard/status | jq -e '.dock_depth == 1' >/dev/null",
        ] {
            assert_eq!(
                compares_against_a_moving_head(clean),
                None,
                "clean: {clean}"
            );
        }
    }

    /// AND THE CORPUS DOES NOT TEACH THE SHAPE IT WARNS ABOUT. The
    /// git-date evidence every door quotes prescribed
    /// `commit=$(git log -1 --format=%ct HEAD)` as its rewrite until
    /// a92571a6 — the starved idiom, recommended in the one text a
    /// builder reads while typing a probe.
    #[test]
    fn the_quoted_rewrite_does_not_date_its_cutoff_from_head() {
        assert_eq!(
            compares_against_a_moving_head(GIT_TIME_STRING_EVIDENCE),
            None,
            "{GIT_TIME_STRING_EVIDENCE}"
        );
        assert!(
            GIT_TIME_STRING_EVIDENCE.contains(CAR_CONVERGED_AT_VAR),
            "the rewrite names the promised instant"
        );
        assert_eq!(compares_against_a_moving_head(MOVING_HEAD_EVIDENCE), None);
    }

    /// A LIMIT IS NOT A FILTER (backlog e7cf78c6, measured 2026-09-20).
    /// The shape is a page taken with `limit=` and then COUNTED
    /// client-side, with nothing ever asking whether the page was the
    /// whole list.
    #[test]
    fn a_counted_page_with_no_total_comparison_is_named() {
        for (probe, token) in [
            (
                "n=$(boss-sor-read '/api/jobs?kind=gate-run&limit=300' | jq '[.data[] | select(.metadata.flake == true)] | length'); \
                 [ \"$n\" -ge 1 ] && echo flake:seen",
                "limit=300",
            ),
            (
                "boss-sor-read \"/api/jobs?kind=ship-a-change&status=open&limit=100\" | jq '.data | length'",
                "limit=100",
            ),
            (
                "boss-sor-read '/api/stations/loading-dock/queue?limit=50' | jq -r '.data[].id' | grep -c . ",
                "limit=50",
            ),
        ] {
            assert_eq!(
                counts_a_page_it_may_not_have_read(probe),
                Some(token),
                "{probe}"
            );
        }
    }

    /// The rewrite the warning names — one body, rows judged against
    /// `.total` — is clean, and so are the two shapes beside it: a page
    /// that is never counted, and a read with no page at all.
    #[test]
    fn a_page_judged_against_its_total_is_not_reported() {
        for clean in [
            "body=$(boss-sor-read '/api/jobs?kind=gate-run&limit=300'); \
             rows=$(printf '%s' \"$body\" | jq '.data | length'); \
             total=$(printf '%s' \"$body\" | jq '.total'); \
             [ \"$rows\" -eq \"$total\" ] || { echo 'not yet: the page is not the list'; exit 75; }",
            "boss-sor-read '/api/audit?kind=class.retired&limit=1' | jq -r '.data[0].at // empty'",
            "boss-sor-read /api/yard/status | jq -e '.dock_depth == 1' >/dev/null && echo claim:ok",
            "git show HEAD:infra/gate.sh | grep -c 'integer expression' && echo claim:ok",
        ] {
            assert_eq!(
                counts_a_page_it_may_not_have_read(clean),
                None,
                "clean: {clean}"
            );
        }
    }

    /// A MENTION IS NOT A DEFINITION (backlog e7cf78c6). A bare
    /// identifier counted in a source file matches the doc comment that
    /// names it, so the count is nonzero whether or not the thing was
    /// ever defined.
    #[test]
    fn a_bare_identifier_counted_in_a_source_file_is_named() {
        for (probe, pattern) in [
            (
                "c=$(git show HEAD:crates/core/boss-jobs/src/claims.rs | grep -c in_flight_claims); \
                 [ \"$c\" -ge 1 ] && echo claim:ok",
                "in_flight_claims",
            ),
            (
                "git show HEAD:crates/core/boss-jobs/src/claims.rs | grep -c \"in_flight_claims\" ",
                "in_flight_claims",
            ),
            (
                "git show HEAD:apps/web/src/it/nav.ts | grep -q navCatalog && echo claim:ok",
                "navCatalog",
            ),
        ] {
            assert_eq!(
                greps_a_name_where_a_definition_is_meant(probe),
                Some(pattern),
                "{probe}"
            );
        }
    }

    /// The rewrite the warning names — the definition quoted, keyword
    /// and all — is clean, and so is every grep beside it: a phrase, a
    /// SCREAMING_SNAKE name (a mention check is what that usually is),
    /// a path-shaped pattern, and a grep over something that is not a
    /// source file at all.
    #[test]
    fn a_quoted_definition_is_not_reported() {
        for clean in [
            "git show HEAD:crates/core/boss-jobs/src/claims.rs | grep -c 'pub fn in_flight_claims'",
            "git show HEAD:infra/gate.sh | grep -c 'integer expression' && echo claim:ok",
            "git show HEAD:infra/ops/ops-runner.sh | grep -c BOSS_JOBS_URL",
            "git show HEAD:infra/cluster/dev-scratch-reclaim.sh | grep -c ls-remote",
            "boss-sor-read /api/estate/nodes | grep -c kubectl",
        ] {
            assert_eq!(
                greps_a_name_where_a_definition_is_meant(clean),
                None,
                "clean: {clean}"
            );
        }
    }

    /// AND NEITHER NEW SHAPE IS TAUGHT BY THE TEXT THAT WARNS ABOUT IT
    /// — the same check the git-date evidence carries, for the same
    /// reason: the evidence is the one text a builder reads while
    /// typing a probe.
    #[test]
    fn the_new_evidence_does_not_teach_the_shapes_it_warns_about() {
        for text in [
            TRUNCATED_PAGE_EVIDENCE,
            MENTION_NOT_DEFINITION_EVIDENCE,
            GIT_TIME_STRING_EVIDENCE,
            MOVING_HEAD_EVIDENCE,
            CAR_INSTANT_RECIPE,
        ] {
            assert_eq!(counts_a_page_it_may_not_have_read(text), None, "{text}");
            assert_eq!(
                greps_a_name_where_a_definition_is_meant(text),
                None,
                "{text}"
            );
        }
    }
}
