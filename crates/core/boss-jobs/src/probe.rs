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
//! TWO RULES, AND THEY ARE NOT THE SAME KIND OF RULE. One asks whether
//! the text can RUN where it is going, which is host-relative; the
//! other asks whether its answer can be TRUSTED, which is true
//! everywhere. Each door applies the first only about the host it is
//! actually sending the probe to, and the second always. The predicates
//! below are shared; the wording of a refusal belongs to the door,
//! because what to do instead differs by door.

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

/// The spellings a probe reaches the system of record by. All four were
/// used by real probes: the env var the runner exports, the in-cluster
/// service DNS (the second measured instance), the LAN address and port
/// (the first), and a bare `/api/` path through any of them.
const SOR_SPELLINGS: [&str; 4] = ["BOSS_JOBS_URL", "boss-jobs-internal", ":7900", "/api/"];

/// Commands that can perform the read. `python`/`python3` are here
/// because the first measured instance was `urllib.request.urlopen`,
/// not a curl.
const HTTP_CLIENTS: [&str; 4] = ["curl", "wget", "python", "python3"];

/// Does this probe read the system of record WITHOUT saying who it is?
/// Returns the client it would read with.
///
/// Two conditions, both required, because either alone is a false
/// refusal: an HTTP client in COMMAND POSITION (the same scan
/// [`needs_absent_tool`] uses, so `grep -c BOSS_JOBS_URL <file>` and
/// `test -n "$BOSS_JOBS_URL"` are mentions, not reads), and the text
/// naming the system of record at all.
///
/// Identified reads are not this check's business: a probe that sends
/// the runner's read-scoped actor is already a named reader. That is
/// keyed on [`SOR_USER_VAR`] and not on the header name, so a probe
/// cannot satisfy it by writing its own privileged header literal — the
/// honest bound being that the forge can forge any header it likes, so
/// this steers an accident rather than stopping an intent. A reader
/// that is identified some other way (an operator's own door, a
/// profile-supplied header) reads as a refusal here, which is why every
/// door that refuses on this rule carries a stated override.
pub fn reads_the_sor_unidentified(probe: &str) -> Option<&'static str> {
    if !SOR_SPELLINGS.iter().any(|s| probe.contains(s)) {
        return None;
    }
    if probe.contains(SOR_USER_VAR) {
        return None;
    }
    commands_invoked(probe)
        .into_iter()
        .find_map(|c| HTTP_CLIENTS.iter().find(|h| **h == c).copied())
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
policy-scoped surface is what a narrowed read produces anyway.";

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
