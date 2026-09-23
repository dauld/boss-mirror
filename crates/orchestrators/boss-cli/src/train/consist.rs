//! The consist check — proving the ASSEMBLED tree before spending CI on it.

use super::*;

// ---------------------------------------------------------------------------
// The consist check — proving the ASSEMBLED tree before spending CI on it
//
// Pre-flight above checks the LOCOMOTIVE. This checks the CONSIST, and
// it exists because of a number: train arrival went 100% (08-20) → 40%
// (08-23) → 0% (08-24) as cars-per-train rose. Every failure in the last
// two days was a COMBINATION failure — invisible to a per-branch gate,
// because on each branch alone there was nothing wrong:
//
//   - two cars each added `infra/postgres/schema/153-*.sql`. Unique on
//     either branch; a duplicate the moment they were assembled.
//   - a new lint (`infra/lint/one-palette.sh`) and the mocked spec that
//     has to NAME the forbidden pattern in order to test it rode the
//     same train. The lint failed on the spec.
//
// Each cost roughly 90 minutes of CI to learn ONE bit, plus a cancel,
// plus a strike on every innocent car aboard. So the conductor asks the
// cheap questions itself, against the tree it just assembled, before it
// spends anything. Three rules keep it from becoming the thing it is
// meant to save:
//
//   - CHEAP ONLY. Text lints, run out of the assembled `infra/lint/`.
//     No cargo, no bun, no database. Measured: the 23 included scripts
//     total ~9 seconds. The full gate is what CI is for; this is not a
//     second gate and must never grow into one.
//   - DISCOVERED, NOT LISTED. Every `infra/lint/*.sh` in the ASSEMBLED
//     tree runs, minus a named exclusion set — which is the whole point
//     of the second failure above: the lint that catches the next
//     combination failure may be arriving ON THE TRAIN, and no
//     hand-picked pair in this file could have seen it.
//   - TAME WHEN IT BREAKS ITSELF. Missing, unrunnable, or over budget
//     is a logged warning and the train departs. A preflight that
//     becomes a new way to block every train costs more than it saves.
//
// What it deliberately does NOT do is decide whose fault the failure
// was. Nobody's: each car was green alone. So a refusal opens no PR,
// strikes no car, and leaves every one of them boardable carrying a
// reason that names the lint and the files.
// ---------------------------------------------------------------------------

// The roster, the exclusions and the three budgets used to be four
// constants here. They are policy — every one of them is a question
// somebody could reasonably answer differently tomorrow — and they now
// arrive as a `DeliveryPolicy` resolved once per invocation from the
// registry (`crate::delivery_policy`). What stayed here is the
// mechanism: walk the tree, run bash, read exit codes, decide.

/// What one cheap lint said about the assembled tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LintResult {
    Passed,
    /// Non-zero exit: the tree is bad. Carries the combined
    /// stdout+stderr, because half these scripts report on stderr.
    Failed(String),
    /// The check itself could not run. Never a refusal — see the
    /// third rule above.
    Unrunnable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LintRun {
    pub(crate) name: String,
    pub(crate) result: LintResult,
}

/// A lint that disagreed with the assembled tree, with the files its
/// own output named (best effort — a hint on the car, not a claim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LintFailure {
    pub(crate) name: String,
    pub(crate) output: String,
    pub(crate) files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConsistVerdict {
    Proceed {
        ran: usize,
        warnings: Vec<String>,
    },
    Refuse {
        failed: Vec<LintFailure>,
        ran: usize,
        warnings: Vec<String>,
    },
}

impl ConsistVerdict {
    pub(crate) fn ran(&self) -> usize {
        match self {
            ConsistVerdict::Proceed { ran, .. } | ConsistVerdict::Refuse { ran, .. } => *ran,
        }
    }

    pub(crate) fn warnings(&self) -> &[String] {
        match self {
            ConsistVerdict::Proceed { warnings, .. } | ConsistVerdict::Refuse { warnings, .. } => {
                warnings
            }
        }
    }
}

/// The files a lint's output names, so a refusal can say WHICH files
/// collided rather than only which check complained. Deliberately a
/// text heuristic over every lint's output rather than a parser per
/// lint: the checks are free to say whatever they say, and a hint that
/// is occasionally empty is worth more than a parser that must be
/// extended for every new script.
///
/// A token counts as a filename when it ends in a short alphabetic
/// extension — which keeps `Cargo.toml` and `153-a.sql` and drops
/// `v1.2`, `0.8`, and sentences ending in a full stop.
pub(crate) fn files_named_in(output: &str, budget: usize) -> Vec<String> {
    let mut named: Vec<String> = Vec::new();
    for token in output.split_whitespace() {
        let token =
            token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/' && c != '.');
        let Some((stem, ext)) = token.rsplit_once('.') else {
            continue;
        };
        let looks_like_a_file = !stem.is_empty()
            && (1..=6).contains(&ext.len())
            && ext.starts_with(|c: char| c.is_ascii_alphabetic())
            && ext.chars().all(|c| c.is_ascii_alphanumeric());
        if !looks_like_a_file {
            continue;
        }
        if !named.iter().any(|f| f == token) {
            named.push(token.to_string());
        }
        if named.len() == budget {
            break;
        }
    }
    named
}

/// ONE reason string, journal and Job alike — the chip the yard
/// renders and the line the operator greps must never tell different
/// stories (the rule `skip_reason_conflict` already follows, down to
/// the file budget: this lands on `metadata.skip_reason`, which
/// PacketCard renders as "LEFT BEHIND — `<reason>`", so the reason does
/// not repeat the words the chip already says).
///
/// The last clause is the point of the whole car. A car that reads
/// this did nothing wrong, and must not be treated — by a person or by
/// the boarding rules — as if it had.
pub(crate) fn consist_refusal_reason(failed: &[LintFailure], file_budget: usize) -> String {
    let Some(first) = failed.first() else {
        return "consist check refused, no failing check named".to_string();
    };
    let mut files = first.files.join(", ");
    if files.len() > file_budget {
        files = format!("{} files", first.files.len());
    }
    let named = if files.is_empty() {
        String::new()
    } else {
        format!(" ({files})")
    };
    let others = match failed.len() {
        0 | 1 => String::new(),
        n => format!(" +{} more check(s)", n - 1),
    };
    format!(
        "consist check: {} failed on the assembled tree{named}{others} — a combination failure, \
         not this car's fault",
        first.name
    )
}

/// The lint script names the assembled tree's OWN gate leaves out of
/// its pre-flight, asked of `infra/gate.sh --exclusions` in that tree.
///
/// gate.sh derives them from each lint's `# consist: skip — <why>`
/// header, and this asks gate.sh rather than reading the headers
/// itself: a second parser of the same line is the pair reopening.
/// Until 2026-09-18 the conductor subtracted the delivery policy's
/// `consist_excluded_lints` instead — a copy of the same four names
/// that nothing held equal to gate.sh's, one of five (tech-debt audit
/// H9, backlog 6fa15484). The tree's gate.sh is also the RIGHT copy:
/// a lint arriving on this very train with the header is left out on
/// this boarding, where a registry row could only have learned of it
/// after the train landed.
///
/// A refusal (a declaration with no reason) or a tree with no gate.sh
/// is an error the caller turns into a warning on a `Proceed`, by
/// name — never a silent "then run everything".
fn gate_exclusions(tree: &Path) -> Result<BTreeSet<String>> {
    let out = Command::new("bash")
        .arg("infra/gate.sh")
        .arg("--exclusions")
        .current_dir(tree)
        .output()
        .with_context(|| format!("running infra/gate.sh --exclusions in {}", tree.display()))?;
    if !out.status.success() {
        bail!(
            "infra/gate.sh --exclusions rc={}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split('\t').next())
        .filter_map(|path| Path::new(path).file_name())
        .map(|name| name.to_string_lossy().to_string())
        .collect())
}

/// Which `infra/lint/*.sh` of the assembled tree this check runs, in
/// a deterministic order (sorted, so two runs over one tree ask the
/// same questions in the same sequence). The roster is the directory
/// minus what the tree's gate declares out — nothing in code to edit
/// when a lint lands, and nothing anywhere but the lint's own header
/// to edit when one needs more than a tree.
fn cheap_lints(tree: &Path) -> Result<Vec<PathBuf>> {
    let excluded = gate_exclusions(tree)?;
    let dir = tree.join("infra/lint");
    let mut scripts: Vec<PathBuf> = fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sh"))
        .filter(|p| {
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            !excluded.contains(&name)
        })
        .collect();
    scripts.sort();
    Ok(scripts)
}

/// Run one lint against `tree`. `bash <script>` rather than executing
/// it directly: the checkout may not carry the executable bit, and
/// every one of these scripts is a bash script that locates the repo
/// root from its own path.
fn run_one_lint(tree: &Path, script: &Path, output_budget: usize) -> LintResult {
    if !script.is_file() {
        return LintResult::Unrunnable("not a readable file".to_string());
    }
    let out = Command::new("bash").arg(script).current_dir(tree).output();
    let out = match out {
        Ok(out) => out,
        Err(e) => return LintResult::Unrunnable(format!("could not spawn bash: {e}")),
    };
    match out.status.code() {
        Some(0) => LintResult::Passed,
        // The shell's own "I could not run that" codes. 127 is what a
        // dangling script name produces, and reading that as "the tree
        // is bad" would turn a deleted file into a stopped railway.
        Some(126) => LintResult::Unrunnable("not executable by the shell (126)".to_string()),
        Some(127) => LintResult::Unrunnable("command not found (127)".to_string()),
        _ => {
            let mut text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            text.truncate(
                text.char_indices()
                    .nth(output_budget)
                    .map_or(text.len(), |(i, _)| i),
            );
            LintResult::Failed(text.trim().to_string())
        }
    }
}

/// The decision. Pure over what the lints said, so the verdict is
/// testable without a tree and the tree-walking stays in one place.
pub(crate) fn consist_verdict(runs: &[LintRun], files_named: usize) -> ConsistVerdict {
    let mut warnings = Vec::new();
    let mut failed = Vec::new();
    let mut ran = 0;
    for run in runs {
        match &run.result {
            LintResult::Passed => ran += 1,
            LintResult::Failed(output) => {
                ran += 1;
                failed.push(LintFailure {
                    name: run.name.clone(),
                    output: output.clone(),
                    files: files_named_in(output, files_named),
                });
            }
            // Named with the `.sh` back on: what could not run is a
            // FILE, and the operator's next move is to look for it.
            LintResult::Unrunnable(why) => {
                warnings.push(format!("{}.sh could not run ({why})", run.name));
            }
        }
    }
    if failed.is_empty() {
        ConsistVerdict::Proceed { ran, warnings }
    } else {
        ConsistVerdict::Refuse {
            failed,
            ran,
            warnings,
        }
    }
}

/// Why a `Proceed` is UNCHECKED rather than clean: `Some` when it ran
/// nothing, carrying every warning that explains the zero. A verdict
/// that ran at least one lint is checked, however partially, and
/// answers `None`.
///
/// This is the line between "the tree passed" and "the tree was never
/// looked at" (backlog 699145ac, 2026-09-19): after a could-not-list
/// warning the conductor logged '0 cheap lint(s) clean' and departed,
/// and a reader of the journal took a check that never ran for a
/// check that passed. Green-with-warnings fails the same way as
/// permanently-red (CLAUDE.md §Diagnosis), so the zero has to say
/// what it is.
pub(crate) fn consist_unchecked_reason(verdict: &ConsistVerdict) -> Option<String> {
    if verdict.ran() > 0 {
        return None;
    }
    let warnings = verdict.warnings();
    Some(if warnings.is_empty() {
        "no lint ran and nothing said why".to_string()
    } else {
        warnings.join("; ")
    })
}

/// The one journal line a departing consist gets. UNCHECKED names its
/// reason and still departs — a broken check must not hold a train —
/// but it never says "clean" about a tree nothing looked at.
pub(crate) fn consist_departure_line(verdict: &ConsistVerdict) -> String {
    match consist_unchecked_reason(verdict) {
        Some(why) => format!(
            "consist check: UNCHECKED — {why}; departing, a broken check must not hold a train"
        ),
        None => format!(
            "consist check: {} cheap lint(s) clean on the assembled tree",
            verdict.ran()
        ),
    }
}

/// The same fact for the train packet's `assemble` step — the step
/// that records the assembled branch, which is what the consist
/// check tested — so the yard reads what the journal said. Step
/// fields are strings (`complete_step`), hence "true"/"false"; a
/// checked consist carries its count and no reason.
pub(crate) fn consist_step_fields(verdict: &ConsistVerdict) -> Vec<(&'static str, Option<String>)> {
    let unchecked = consist_unchecked_reason(verdict);
    vec![
        ("consist_checked", Some(unchecked.is_none().to_string())),
        ("consist_lints_ran", Some(verdict.ran().to_string())),
        ("consist_unchecked_reason", unchecked),
    ]
}

/// Point the clone's `origin/main` at CURRENT forge main before the
/// consist lints resolve their baseline against it.
///
/// THE FALSE POSITIVE THIS CLOSES (2026-09-06). Several cheap lints
/// (`a-kind-bundle-does-not-tighten.sh`, `migrations-append-only.sh`)
/// compute their baseline as `git merge-base(<trunk ref>, HEAD)` in
/// the conductor's OWN clone, where the trunk ref is `origin/main`.
/// A car merge pulls the car's ancestry — the last-landed main — into
/// the assembled HEAD, but the clone's `origin/main` ref only advances
/// on a fetch. When a prior train has landed and this board has not
/// re-fetched since, that ref lags behind the tree it is being
/// compared against, so the merge-base falls to a commit BEFORE the
/// last train's changes and the lint reads those already-landed
/// changes as if this consist introduced them. Observed: a
/// `bill-approval.po_id is now required` refusal on a consist whose
/// `step_types.toml` was byte-identical to main — nobody's car at
/// fault, the whole train refused, boarding blocked until the next
/// reconcile-fetch happened to freshen the ref.
///
/// BEST-EFFORT, NON-FATAL. A broken fetch must never hold a train (the
/// rule: conductor loop writes must not be fatal). A failed freshen
/// (a network blip, a missing remote) LOGS and returns; the lints then
/// resolve against the ref as it already stands, which is exactly
/// today's behaviour — so a failed freshen is never worse than not
/// trying. The failure is logged, not `.ok()`-swallowed, so it stays
/// visible. `origin` is the remote the conductor's clone fetches in
/// `ensure_clone` and checks out the train branch from, and the remote
/// the lints' `origin/main` trunk ref is fed by.
pub(super) fn freshen_trunk(clone: &str) {
    match sh_unchecked(&[
        "git",
        "-C",
        clone,
        "fetch",
        "--quiet",
        "origin",
        "+refs/heads/main:refs/remotes/origin/main",
    ]) {
        Ok(out) if out.status.success() => {}
        Ok(out) => log(format!(
            "consist check: could not freshen origin/main (git fetch rc={}) — the lints will \
             resolve their baseline against the trunk ref as it stands: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => log(format!(
            "consist check: could not run the trunk fetch — the lints will resolve their \
             baseline against the trunk ref as it stands: {e}"
        )),
    }
}

/// Ask every cheap lint in the assembled tree what it thinks, then
/// decide. Every failure mode of this function itself lands as a
/// warning on a `Proceed`.
///
/// All the checks run, not just up to the first failure: they are
/// seconds each, and learning ONE bit per attempt is precisely the
/// cost this exists to stop paying.
pub(crate) fn consist_check(tree: &Path, policy: &DeliveryPolicy) -> ConsistVerdict {
    let scripts = match cheap_lints(tree) {
        Ok(s) if s.is_empty() => {
            return ConsistVerdict::Proceed {
                ran: 0,
                warnings: vec![
                    "no lint scripts in the assembled tree — nothing cheap to ask".to_string(),
                ],
            };
        }
        Ok(s) => s,
        Err(e) => {
            return ConsistVerdict::Proceed {
                ran: 0,
                warnings: vec![format!("could not list the tree's lints ({e})")],
            };
        }
    };

    let started = std::time::Instant::now();
    let total = scripts.len();
    let mut runs = Vec::with_capacity(total);
    for (done, script) in scripts.iter().enumerate() {
        let name = script
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .trim_end_matches(".sh")
            .to_string();
        runs.push(LintRun {
            name,
            result: run_one_lint(tree, script, policy.consist_output_budget),
        });
        if started.elapsed() > policy.consist_budget && done + 1 < total {
            let mut verdict = consist_verdict(&runs, policy.consist_files_named);
            let spent = started.elapsed().as_secs();
            let note = format!(
                "budget spent ({spent}s) after {} of {total} checks — going on what ran",
                done + 1
            );
            match &mut verdict {
                ConsistVerdict::Proceed { warnings, .. }
                | ConsistVerdict::Refuse { warnings, .. } => warnings.push(note),
            }
            return verdict;
        }
    }
    consist_verdict(&runs, policy.consist_files_named)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::train::test_support::*;

    // -- a machine cancellation says why -------------------------------

    /// Measured 2026-09-05 across every cancelled train on the record:
    /// the 11 a HUMAN cancelled all carry a reason (from `--reason`),
    /// and the 5 the MACHINE cancelled carry none. Both of the previous
    /// night's consist refusals were in the silent five, which is why a
    /// jammed yard could only be explained by reading a pod log.
    ///
    /// The terminal fires off the `empty` predicate, so a self-cancel
    /// used to complete it with no reason at all. Every abandonment now
    /// names its cause on the same field a human fills — and the causes
    /// must stay DISTINGUISHABLE, because "nothing was ready" is a
    /// healthy idle window while "a check could not run" is an outage.
    #[test]
    fn every_abandonment_reason_names_its_cause() {
        // An idle window must not read like a failure.
        let idle =
            "no car was parked and ready when the window opened — an idle window, not a failure";
        assert!(idle.contains("idle window"));
        assert!(!idle.to_lowercase().contains("refus"));

        // A consist refusal must name the check, not merely that one failed.
        let failed = vec![LintFailure {
            name: "a-kind-bundle-does-not-tighten".into(),
            files: vec!["crates/core/boss-jobs/seeds/step_types.toml".into()],
            output: "python3: command not found".into(),
        }];
        let reason = consist_refusal_reason(&failed, 200);
        assert!(
            reason.contains("a-kind-bundle-does-not-tighten"),
            "a refusal that does not name the check is one an operator must go re-derive: {reason}"
        );

        // With nothing to name, say so rather than implying a verdict.
        let empty = consist_refusal_reason(&[], 200);
        assert!(empty.contains("no failing check named"), "{empty}");
    }

    // ---- the consist check --------------------------------------
    //
    // The combination failures of 2026-08-22..24, reproduced. These
    // drive the REAL lint script out of `infra/lint/`, for the same
    // reason the publish_car_branch fixtures drive real git: the whole
    // claim is "the conductor can answer this question from the
    // assembled tree in seconds", and a faked lint would only prove
    // this file agrees with itself.

    /// Twelve numbered migrations — one over the scrape guard
    /// `migration-numbers-unique.sh` uses to refuse to report on a
    /// directory it clearly failed to read.
    fn twelve_migrations() -> Vec<String> {
        (140..152).map(|n| format!("{n}-thing.sql")).collect()
    }

    /// An assembled tree as the consist check meets it: `infra/lint/`
    /// carrying the real migration-numbers lint, and whatever the cars
    /// dropped into `infra/postgres/schema/`.
    fn consist_fixture(name: &str, migrations: &[String]) -> (Scratch, std::path::PathBuf) {
        let root = boss_testing::scratch_dir(&format!("boss-consist-{name}"));
        let guard = Scratch(root.clone());
        let lint = root.join("infra/lint");
        let schema = root.join("infra/postgres/schema");
        std::fs::create_dir_all(&lint).expect("mkdir infra/lint");
        std::fs::create_dir_all(&schema).expect("mkdir schema");
        let real = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../infra/lint/migration-numbers-unique.sh");
        std::fs::copy(&real, lint.join("migration-numbers-unique.sh"))
            .unwrap_or_else(|e| panic!("copy {}: {e}", real.display()));
        // And what the lint sources: every lint reads infra/lint/lib/
        // (lib/scanned.sh since 2026-09-18), so a fixture carrying the
        // script without the lib runs a lint that cannot start, and the
        // verdict is about the fixture, not the tree.
        boss_testing::copy_lint_libs(&root);
        // And the tree's gate.sh: the consist check asks it which lints
        // declare themselves out (`--exclusions`), so a fixture without
        // one is a tree whose roster cannot be derived — and gate.sh
        // REFUSES without any helper it sources, so it is carried with
        // all of them, gate.sh being the one that says which (955c99b6).
        boss_testing::copy_gate_sh(&root);
        for m in migrations {
            std::fs::write(schema.join(m), "-- fixture\n").expect("write migration");
        }
        (guard, root)
    }

    #[test]
    fn a_clean_consist_lets_the_train_go_on_to_the_pr() {
        let (_g, tree) = consist_fixture("clean", &twelve_migrations());
        let verdict = consist_check(&tree, &policy());
        assert!(
            matches!(verdict, ConsistVerdict::Proceed { .. }),
            "a tree with no duplicate numbers must not stop a train: {verdict:?}"
        );
        assert_eq!(verdict.ran(), 1, "the one lint in the fixture tree ran");
        assert!(
            verdict.warnings().is_empty(),
            "nothing to warn about: {verdict:?}"
        );
    }

    /// THE FAILURE THAT COST 90 MINUTES OF CI TO LEARN ONE BIT. Two
    /// cars each added `infra/postgres/schema/153-*.sql`. Unique on
    /// each branch, so both passed their own gate; a duplicate the
    /// moment the conductor merged them together.
    #[test]
    fn two_cars_that_both_took_number_153_are_refused_before_the_pr() {
        let mut migrations = twelve_migrations();
        migrations.push("153-dispatcher-rule-cluster-conformance.sql".to_string());
        migrations.push("153-estate-subjects.sql".to_string());
        let (_g, tree) = consist_fixture("dupe-153", &migrations);

        let verdict = consist_check(&tree, &policy());
        let ConsistVerdict::Refuse { failed, .. } = &verdict else {
            panic!("a duplicated migration number must refuse the consist: {verdict:?}");
        };
        assert_eq!(
            failed.len(),
            1,
            "one lint disagreed with this tree: {verdict:?}"
        );
        assert_eq!(
            failed[0].name, "migration-numbers-unique",
            "the refusal names the check, so nobody has to guess"
        );
        // The valuable half: the reason names the lint AND the files,
        // because a combination failure is nobody's car's fault and the
        // cars stay boardable carrying only this string.
        let reason = consist_refusal_reason(failed, policy().skip_reason_file_budget);
        assert!(
            reason.contains("migration-numbers-unique"),
            "reason names the lint: {reason}"
        );
        assert!(
            reason.contains("153-estate-subjects.sql"),
            "reason names a file the lint's own output named: {reason}"
        );
        assert!(
            failed[0]
                .files
                .contains(&"153-dispatcher-rule-cluster-conformance.sql".to_string()),
            "both colliding files are derivable from the output: {:?}",
            failed[0].files
        );
    }

    /// THE COLLISION THAT SURVIVED THE TIMESTAMP (bc7cac00). Two
    /// builders picked `202609082130` in the same minute on
    /// 2026-09-08, so the prefix carries SECONDS now. The lint is the
    /// backstop, and a backstop that only says "you collided" makes
    /// the reader derive the fix: it must name the file to renumber
    /// and the stamp to renumber it to.
    #[test]
    fn the_refusal_names_the_renumber_it_implies() {
        let mut migrations = twelve_migrations();
        migrations.push("20260908213000-sign-off-plugin-v3.sql".to_string());
        migrations.push("20260908213000-arrival-runs-the-probe.sql".to_string());
        let (_g, tree) = consist_fixture("dupe-same-second", &migrations);

        let verdict = consist_check(&tree, &policy());
        let ConsistVerdict::Refuse { failed, .. } = &verdict else {
            panic!("a duplicated timestamp must refuse the consist: {verdict:?}");
        };
        let out = &failed[0].output;
        assert!(
            out.contains("%Y%m%d%H%M%S"),
            "the fix is a fresh SECONDS stamp, and the message must hand over the \
             command that takes one: {out}"
        );
        assert!(
            out.contains("20260908213000"),
            "the message must name the prefix a fresh stamp has to beat, which is \
             the latest one in the tree: {out}"
        );
        assert!(
            out.contains("has NOT been applied"),
            "and which of the two to rename: {out}"
        );
    }

    /// A broken preflight must not become a new way to block every
    /// train. A lint that cannot be run is a logged warning and the
    /// train departs — the check is an accelerant, never a gate.
    #[test]
    fn a_lint_that_cannot_run_warns_and_the_train_still_departs() {
        let (_g, tree) = consist_fixture("ghost", &twelve_migrations());
        // A dangling symlink: the name is in the directory, the script
        // is not on disk. `bash` would exit 127 on it, which must read
        // as "could not run", never as "the tree is bad".
        std::os::unix::fs::symlink(
            tree.join("infra/lint/deleted-by-some-car.sh"),
            tree.join("infra/lint/ghost.sh"),
        )
        .expect("symlink");

        let verdict = consist_check(&tree, &policy());
        assert!(
            matches!(verdict, ConsistVerdict::Proceed { .. }),
            "an unrunnable check must not refuse a consist: {verdict:?}"
        );
        assert!(
            verdict.warnings().iter().any(|w| w.contains("ghost.sh")),
            "and it must say so by name: {:?}",
            verdict.warnings()
        );
        assert_eq!(verdict.ran(), 1, "the runnable lint still ran");
    }

    /// The tamest failure mode of all: a tree with no lints in it.
    #[test]
    fn a_tree_with_no_lint_directory_proceeds_with_a_warning() {
        let root = boss_testing::scratch_dir("boss-consist-bare");
        let _g = Scratch(root.clone());
        let verdict = consist_check(&root, &policy());
        assert!(
            matches!(verdict, ConsistVerdict::Proceed { ran: 0, .. }),
            "no lints is not a reason to hold a train: {verdict:?}"
        );
        assert!(!verdict.warnings().is_empty(), "but it is worth a line");
    }

    /// THE FALSE POSITIVE THIS FIXES (2026-09-06). The consist lints
    /// resolve their baseline as `merge-base(origin/main, HEAD)` in the
    /// conductor's clone; a prior train landing leaves that ref stale
    /// until a fetch, and the lint then reads already-landed changes as
    /// this consist's own. `freshen_trunk` points `origin/main` at
    /// CURRENT forge main before the lints run. Simulate a prior train
    /// landing (a second clone advances the forge) while this clone's
    /// `origin/main` lags, then prove one freshen catches it up.
    #[test]
    fn freshen_trunk_catches_origin_main_up_to_the_forge() {
        let (_g, clone) = clone_fixture("freshen-ok");
        let root = clone.parent().expect("root").to_path_buf();
        let origin = root.join("origin.git");

        // Whoever landed the last train, standing in: a second clone
        // advances the forge's main. THIS clone has not fetched since,
        // so its origin/main is now stale.
        let other = root.join("other");
        git_ok(
            &root,
            &[
                "clone",
                "-q",
                origin.to_str().expect("utf8"),
                other.to_str().expect("utf8"),
            ],
        );
        git_ok(&other, &["config", "user.email", "t@example.com"]);
        git_ok(&other, &["config", "user.name", "t"]);
        std::fs::write(other.join("LANDED"), "a prior train").expect("write");
        git_ok(&other, &["add", "-A"]);
        git_ok(&other, &["commit", "-qm", "prior train landed"]);
        git_ok(&other, &["push", "-q", "origin", "main"]);
        let forge_main = rev(&other, "HEAD");

        // Before: the conductor clone's origin/main lags the forge.
        assert_ne!(
            rev(&clone, "origin/main"),
            forge_main,
            "precondition: origin/main is stale"
        );

        freshen_trunk(clone.to_str().expect("utf8"));

        assert_eq!(
            rev(&clone, "origin/main"),
            forge_main,
            "one freshen catches origin/main up to current forge main — which is the ref the \
             consist lints' merge-base baseline is resolved against"
        );
    }

    /// BEST-EFFORT, NON-FATAL. A freshen that cannot reach the remote
    /// must LOG and return, never abort — the consist then proceeds on
    /// the trunk ref as it stands, exactly today's fallback. A missing
    /// `origin` remote makes the fetch exit non-zero; `freshen_trunk`
    /// returning `()` at all is the guarantee (it cannot bail or
    /// panic), and it must leave the tree untouched.
    #[test]
    fn a_failed_freshen_does_not_abort_and_changes_nothing() {
        let root = boss_testing::scratch_dir("boss-freshen-noremote");
        let _g = Scratch(root.clone());
        git_ok(&root, &["init", "-q", "-b", "main"]);
        git_ok(&root, &["config", "user.email", "t@example.com"]);
        git_ok(&root, &["config", "user.name", "t"]);
        std::fs::write(root.join("README"), "no origin remote here").expect("write");
        git_ok(&root, &["add", "-A"]);
        git_ok(&root, &["commit", "-qm", "base"]);
        let head_before = rev(&root, "HEAD");

        // No `origin` remote: the fetch exits non-zero. freshen_trunk
        // must swallow it and leave the tree exactly as it was.
        freshen_trunk(root.to_str().expect("utf8"));

        assert_eq!(
            rev(&root, "HEAD"),
            head_before,
            "a failed freshen holds no train and changes nothing"
        );
    }

    /// Discovery over the REAL `infra/lint/`, which is the claim that
    /// matters: the roster is the directory, so a lint arriving ON a
    /// train is asked without anyone editing this file. Only the
    /// listing is exercised here — running the set costs ~9 seconds
    /// and its verdict depends on the working tree, neither of which
    /// belongs in a unit test.
    ///
    /// What the conductor leaves out is what the tree's own gate leaves
    /// out of its pre-flight (`infra/gate.sh --exclusions`, read off
    /// each lint's `# consist: skip — <why>` header). Until 2026-09-18
    /// this test held the conductor's roster against the delivery
    /// policy's copy of that set — one of five copies (audit H9, backlog
    /// 6fa15484), and the one the consist check actually ran on, held
    /// equal to gate.sh's by nothing. Now there is one derivation and
    /// the conductor asks it; this pins that it does.
    #[test]
    fn the_roster_is_the_lint_directory_minus_what_the_gate_declares_out() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let names: Vec<String> = cheap_lints(&root)
            .expect("the tree has an infra/lint and a gate.sh")
            .iter()
            .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into())
            .collect();
        assert!(
            names.len() > 15,
            "the whole cheap set runs, not a hand-picked pair: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "migration-numbers-unique.sh"),
            "the lint that catches duplicate migration numbers is in: {names:?}"
        );
        let declared_out = gate_exclusions(&root).expect("the tree's gate.sh answers --exclusions");
        assert!(
            !declared_out.is_empty(),
            "the lints that need a live database, a built workspace or a package manager \
             still declare themselves out"
        );
        for script in &declared_out {
            assert!(
                !names.iter().any(|n| n == script),
                "{script} declares itself out of the pre-flight and must stay out: {names:?}"
            );
        }
    }

    /// THE PROPERTY THE DESIGN IS BOUGHT FOR, one notch better than the
    /// policy row it replaces. A lint that needs more than a tree says
    /// so in its own header, so a lint arriving ON the train with that
    /// header is left out on the same boarding — no registry edit, no
    /// migration, no train ahead of it. Exercised on a fixture whose
    /// declaring lint would FAIL if run: the only way the consist
    /// proceeds is by honouring the declaration.
    #[test]
    fn a_lint_that_declares_a_consist_skip_is_left_out_on_the_same_boarding() {
        let (_g, tree) = consist_fixture("declared-skip", &twelve_migrations());
        std::fs::write(
            tree.join("infra/lint/needs-a-database.sh"),
            "#!/usr/bin/env bash\n\
             # consist: skip — psql against a live database, which the assembled tree has not got\n\
             echo 'this lint must never have been run by the consist check' >&2\n\
             exit 1\n",
        )
        .expect("write the declaring lint");
        let names: Vec<String> = cheap_lints(&tree)
            .expect("the fixture carries gate.sh and infra/lint")
            .iter()
            .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into())
            .collect();
        assert_eq!(
            names,
            vec!["migration-numbers-unique.sh".to_string()],
            "the roster is the directory MINUS what the lints themselves declare: {names:?}"
        );
        let verdict = consist_check(&tree, &policy());
        assert!(
            matches!(verdict, ConsistVerdict::Proceed { .. }),
            "the declaring lint was not run, so it could not refuse: {verdict:?}"
        );
        assert_eq!(verdict.ran(), 1, "the one undeclared lint ran");
    }

    /// The one way the derivation can go wrong is now loud. A
    /// declaration with no reason is refused by gate.sh; the conductor
    /// must carry that refusal — by name — onto the verdict as a
    /// warning and let the train go, never run every lint as if
    /// nothing had been declared and never hold the track over it.
    #[test]
    fn a_consist_skip_with_no_reason_warns_by_name_and_the_train_departs() {
        let (_g, tree) = consist_fixture("mute-skip", &twelve_migrations());
        std::fs::write(
            tree.join("infra/lint/mute.sh"),
            "#!/usr/bin/env bash\n# consist: skip\nexit 0\n",
        )
        .expect("write the mute lint");
        let verdict = consist_check(&tree, &policy());
        assert!(
            matches!(verdict, ConsistVerdict::Proceed { ran: 0, .. }),
            "a roster that cannot be derived is a warning, not a verdict on the tree: {verdict:?}"
        );
        assert!(
            verdict.warnings().iter().any(|w| w.contains("mute.sh")),
            "the warning names the lint whose declaration is broken: {:?}",
            verdict.warnings()
        );
    }

    /// Left by the builder of 13700f6f (backlog 699145ac, 2026-09-19):
    /// after a could-not-list warning the conductor still logged
    /// 'consist check: 0 cheap lint(s) clean on the assembled tree' and
    /// departed — a green-sounding line after zero lints ran, which
    /// CLAUDE.md files with permanently-red ("green-with-warnings fails
    /// the same way"). A Proceed that ran nothing is UNCHECKED: the
    /// journal line says so and the train packet's assemble step
    /// carries the same fact, so the yard and the journal cannot
    /// disagree about whether the tree was looked at.
    #[test]
    fn a_proceed_that_ran_no_lints_says_unchecked_on_the_line_and_the_packet() {
        let unchecked = ConsistVerdict::Proceed {
            ran: 0,
            warnings: vec!["could not list the tree's lints (boom)".to_string()],
        };
        let line = consist_departure_line(&unchecked);
        assert_eq!(
            line,
            "consist check: UNCHECKED — could not list the tree's lints (boom); departing, a \
             broken check must not hold a train"
        );
        assert!(
            !line.contains("clean"),
            "nothing ran, so nothing is clean: {line}"
        );
        let fields = consist_step_fields(&unchecked);
        assert!(
            fields.contains(&("consist_checked", Some("false".to_string()))),
            "{fields:?}"
        );
        assert!(
            fields.contains(&(
                "consist_unchecked_reason",
                Some("could not list the tree's lints (boom)".to_string())
            )),
            "{fields:?}"
        );

        // The real ran=0 tree — a skip declaration with no reason —
        // reads the same way, naming the lint.
        let (_g, tree) = consist_fixture("mute-skip-unchecked", &twelve_migrations());
        std::fs::write(
            tree.join("infra/lint/mute.sh"),
            "#!/usr/bin/env bash\n# consist: skip\nexit 0\n",
        )
        .expect("write the mute lint");
        let verdict = consist_check(&tree, &policy());
        let line = consist_departure_line(&verdict);
        assert!(line.starts_with("consist check: UNCHECKED — "), "{line}");
        assert!(line.contains("mute.sh"), "{line}");

        // And a consist that DID run still reads clean, with the count
        // on the packet.
        let clean = ConsistVerdict::Proceed {
            ran: 82,
            warnings: vec![],
        };
        assert_eq!(
            consist_departure_line(&clean),
            "consist check: 82 cheap lint(s) clean on the assembled tree"
        );
        let fields = consist_step_fields(&clean);
        assert!(
            fields.contains(&("consist_checked", Some("true".to_string()))),
            "{fields:?}"
        );
        assert!(
            fields.contains(&("consist_lints_ran", Some("82".to_string()))),
            "{fields:?}"
        );
        assert!(
            fields.contains(&("consist_unchecked_reason", None)),
            "a checked consist carries no unchecked reason: {fields:?}"
        );
    }

    #[test]
    fn a_lints_output_gives_up_the_files_it_names() {
        assert_eq!(
            files_named_in(
                "  153:\n    153-a.sql\n    153-b.sql\n",
                policy().consist_files_named
            ),
            vec!["153-a.sql", "153-b.sql"]
        );
        assert_eq!(
            files_named_in(
                "VIOLATION: infra/postgres/schema/100-a.sql was M-changed",
                policy().consist_files_named
            ),
            vec!["infra/postgres/schema/100-a.sql"]
        );
        assert!(
            files_named_in(
                "one-palette: 3 offences found, see above. e.g. below",
                policy().consist_files_named
            )
            .is_empty(),
            "prose is not a file list"
        );
        assert!(
            files_named_in(
                "bumped to v1.2 in Cargo.toml 0.8",
                policy().consist_files_named
            )
            .iter()
            .all(|f| f == "Cargo.toml"),
            "version numbers are not filenames"
        );
    }
}
