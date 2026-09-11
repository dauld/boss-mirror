//! THE MECHANICAL HALF OF docs/design/a-probe-shape-follows-the-car.md.
//!
//! That document is mostly judgement — which shape of evidence a given
//! claim admits is a call, and the doc says so in as many words. But it
//! rests on four facts that are NOT judgement, and a reference whose
//! facts have quietly gone false is worse than no reference: on
//! 2026-09-11 ten builder briefs carried a probe rule that had stopped
//! being true, and every one of them was believed.
//!
//! So each fact is asserted here against its own authority — the Rust
//! predicate, a real `jq`, the forge's absence manifest, the gate's
//! receipt writer — and paired with the marker the doc states it by. If
//! an authority moves, the test fails and names the line of the document
//! to fix. The taxonomy itself gets no assertion, because a weak one
//! would be a worse lie than the honest label; what IS pinned is that
//! the honest label is still there.
//!
//! THE DOC AND THE TWO INFRA FILES ARE READ AT RUN TIME, not through
//! `include_str!`. An `include_str!` is read while compiling INSIDE the
//! image's build stage, which is a subset of the repo, so
//! `infra/lint/the-image-carries-what-build-scripts-read.sh` refuses one
//! whose target the Dockerfile never COPYs — correctly, and this test's
//! three targets have no business in a runtime image. Runtime reads from
//! `CARGO_MANIFEST_DIR` are what every other infra-reading test here
//! does. They are plain reads, never git, so uid 65534 can make them in
//! a root-owned checkout (the ownership refusal in gate_sh.rs is git's,
//! not the filesystem's). Only `jq` is executed.

use boss_jobs::probe::{forge_absent_tools, reads_the_sor_unidentified};
use std::path::PathBuf;
use std::process::Command;

const DOC_PATH: &str = "docs/design/a-probe-shape-follows-the-car.md";
const GATE_SH_PATH: &str = "infra/gate.sh";
const CI_TOOLS_PATH: &str = "infra/forge/boss-ci/required-tools.txt";

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

/// Assert the document still STATES the fact just proven, and say which
/// fact went stale if it does not.
///
/// `marker` is matched with `str::contains`, not by line, so it must be
/// short enough to never wrap in the prose — the same trap rule 1 of the
/// document warns about for `grep`, which would silently stop matching
/// instead.
///
/// Case is folded, which the first run of this test earned: the doc
/// states rule 1 as a HEADING, so the phrase is capitalised there and a
/// case-sensitive match reported the fact missing when it was on the
/// page in front of it. A pin that cries wolf gets edited away.
fn doc_states(marker: &str, fact: &str) {
    assert!(
        read(DOC_PATH)
            .to_lowercase()
            .contains(&marker.to_lowercase()),
        "{DOC_PATH} no longer states {fact}: the marker {marker:?} is gone.\n\
         The fact is still true (this test just proved it). Either the doc \n\
         dropped it or it was reworded — put it back, in one unwrapped line."
    );
}

/// FACT 1 — THE READER DOES THE READ; ANYTHING MAY PARSE ITS STDOUT.
///
/// `jq` is not in `HTTP_CLIENTS` and must never be added: it cannot
/// make a network request, so it is a parser, not a client. What that
/// means in practice is the thing the document has to get right, and it
/// is three separate behaviours, not one:
///
///   - an unidentified `python3` read of the SoR is refused (python3 is
///     a client, and the first measured instance was `urllib.urlopen`);
///   - `curl … | jq` is STILL refused, on the `curl` — piping into jq
///     launders nothing;
///   - `boss-sor-read … | jq` is clean, because the identified reader
///     did the read and jq only parsed its stdout.
///
/// The third is why five of the corpus's ten overrides were spent on
/// `boss-api … | python3`: that shape trips the rule on `python3`
/// although `boss-api` is an identified reader, which is a FALSE
/// POSITIVE in the predicate, not a rule the builder should be arguing
/// with. Rewriting the parse to `jq` sidesteps it; the rule itself is
/// untouched either way.
#[test]
fn the_reader_does_the_read_and_anything_may_parse_its_stdout() {
    assert_eq!(
        reads_the_sor_unidentified(
            "python3 -c \"import urllib.request; urllib.request.urlopen('$BOSS_JOBS_URL/api/jobs')\""
        ),
        Some("python3"),
        "python3 is a client and an unidentified read through it must stay refused"
    );
    assert_eq!(
        reads_the_sor_unidentified(
            "curl -fsS $BOSS_JOBS_URL/api/yard/status | jq -e '.dock_depth == 1'"
        ),
        Some("curl"),
        "piping into jq does not launder an unidentified curl: the refusal is on the READ"
    );
    assert_eq!(
        reads_the_sor_unidentified("boss-sor-read /api/yard/status | jq -e '.dock_depth == 1'"),
        None,
        "the identified reader did the read; jq only parsed its stdout"
    );
    doc_states("HTTP_CLIENTS", "which commands count as the reader");
    doc_states(
        "the reader does the read",
        "the rule that supersedes \"write it in jq\"",
    );
}

/// FACT 2 — `jq -e` EXITS 4 WHEN ITS FILTER PRODUCES NO OUTPUT.
///
/// Asserted by RUNNING jq, not by citing it. This is the fact behind
/// the document's third rule, and the one that cost 18 hours on car
/// a0ab90a5: a success branch of `empty` under `-e` makes the claim
/// HOLDING the failing case. The positive shape is checked alongside,
/// because "4" is only useful next to the 0 it is supposed to be.
///
/// A missing `jq` FAILS here rather than skipping. It is declared in
/// infra/forge/boss-ci/required-tools.txt, so the gate's image has it,
/// and a skip that prints reassurance is the defect 5942f205 recorded
/// (CLAUDE.md §Diagnosis — a check nobody reads is a check that is not
/// running).
#[test]
fn jq_dash_e_exits_four_when_the_claim_holds_and_the_branch_is_empty() {
    assert!(
        read(CI_TOOLS_PATH).lines().any(|l| l.trim() == "jq"),
        "jq left required-tools.txt; this test and two lints run it"
    );

    let inverted = jq_exit("if (.a == 1) then empty else error(\"no\") end");
    assert_eq!(
        inverted,
        Some(4),
        "`jq -e` must exit 4 on empty output — that is why a success branch \n\
         of `empty` asserts the negation of what its author meant (a0ab90a5)"
    );
    let positive = jq_exit(".a == 1 or error(\"no\")");
    assert_eq!(
        positive,
        Some(0),
        "the shape that cannot invert asserts positively and exits 0 when the claim holds"
    );

    doc_states("exits 4", "jq -e's exit status on empty output");
}

/// `jq -e <filter>` over `{"a":1}`, returning its exit status.
fn jq_exit(filter: &str) -> Option<i32> {
    let out = Command::new("jq")
        .args(["-e", filter])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|mut child| {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(b"{\"a\":1}\n");
            }
            child.wait()
        });
    match out {
        Ok(Ok(status)) => status.code(),
        Ok(Err(e)) => panic!("jq ran but could not be waited on: {e}"),
        Err(e) => panic!("jq is declared in required-tools.txt and did not run: {e}"),
    }
}

/// FACT 3 — THE FORGE HOST HAS NO `kubectl`, AND THE MANIFEST IS THE
/// AUTHORITY.
///
/// A `--park-probe` is authored on the dev pod, where the cluster is one
/// hop away, and RUN on the forge, which is outside it and holds no
/// kubeconfig. The document must point at the manifest rather than
/// restate its contents, because the manifest is the thing a new
/// measurement gets added to — so what is pinned here is the POINTER
/// plus the entry the pointer is worth following for.
#[test]
fn the_forge_lacks_kubectl_and_the_manifest_says_so() {
    let tools = forge_absent_tools();
    assert!(
        tools.contains(&"kubectl"),
        "host-absent-tools.txt lost kubectl (measured f9304366): {tools:?}"
    );
    doc_states(
        "infra/forge/host-absent-tools.txt",
        "where a probe's host facts are authoritative",
    );
    doc_states("kubectl", "the tool the forge was measured to lack");
}

/// FACT 4 — A GATE RECEIPT NAMES ITS CHECKS INDIVIDUALLY.
///
/// This is what makes "a named check in a green gate receipt" a probe
/// shape at all: if the receipt only carried a verdict, a lint car could
/// be proven no more precisely than "something was green". The writer in
/// infra/gate.sh is the authority — it emits one object per check with
/// its own `name` and `result` into a `checks` array — and a car's probe
/// must assert the NAME and the result together, because an absent name
/// would otherwise pass silently.
///
/// AND NAME-PLUS-RESULT IS NOT SUFFICIENT, which is why the refusal path
/// is pinned here too: `write_receipt "refused"` writes a receipt as
/// well, and the disk floor is checked before every PHASE rather than
/// only at startup — so a refusal can carry checks that genuinely ran.
/// A probe that greps only for a name and a result can therefore be
/// satisfied by a run that never judged the branch at all. The document
/// says to assert `verdict: green` and an empty `unverifiable` alongside
/// it; these two assertions are what make that advice a fact.
#[test]
fn a_gate_receipt_names_its_checks_individually() {
    assert!(
        read(GATE_SH_PATH)
            .contains("{\\\"name\\\":\\\"${name}\\\",\\\"result\\\":\\\"${result}\\\""),
        "infra/gate.sh's write_receipt no longer emits a per-check name+result object; \n\
         a probe asserting a NAMED check in a receipt has lost its footing"
    );
    assert!(
        read(GATE_SH_PATH).contains("\"checks\": [${checks}]"),
        "infra/gate.sh's receipt no longer carries a `checks` array"
    );
    assert!(
        read(GATE_SH_PATH).contains("write_receipt \"refused\""),
        "infra/gate.sh no longer writes a receipt on a REFUSAL; the doc's advice to \n\
         assert verdict:green alongside the named check was earned by that path existing"
    );
    assert!(
        read(GATE_SH_PATH).contains("\"unverifiable\": [${unver}]"),
        "infra/gate.sh's receipt no longer carries `unverifiable`; a probe told to \n\
         assert it empty would be asserting a field that is not there"
    );
    doc_states(
        "\"verdict\": \"green\"",
        "what else a lint car's probe must assert",
    );
    doc_states(
        "\"checks\"",
        "the receipt field a lint car's probe reads a named check out of",
    );
}

/// THE HONEST LABEL IS PART OF THE DOCUMENT.
///
/// The taxonomy is judgement. No test can hold it, and the worst
/// outcome here would be a weak assertion that let the doc read as
/// machine-checked when it is not. So the one thing pinned about the
/// table is that it still says which half of the document is which — a
/// reader who cannot tell a measured fact from a considered opinion is
/// exactly the reader these four facts were pinned for.
#[test]
fn the_document_marks_its_judgement_as_judgement() {
    doc_states(
        "Judgement, not fact",
        "the label separating the taxonomy from the pinned facts",
    );
    doc_states(
        "a_probe_shape_follows_the_car.rs",
        "the pointer from the doc to this test",
    );
}
