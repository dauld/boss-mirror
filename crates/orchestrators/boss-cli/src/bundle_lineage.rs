//! Where a registry bundle FILE stands against its LIVE lineage — said
//! before an author bumps a version from the file (backlog 5449111c).
//!
//! WHY THIS EXISTS. A versioned bundle file may sit behind its live
//! lineage by design: an operator who re-versions a rule live leaves
//! the file behind, and the seed reports it as information, never a
//! refusal (`boss_jobs::cadence_seed`). The consequence nobody drew is
//! at the OTHER end. The next author bumps the version FROM THE FILE,
//! because the file is the declared home and the file is what a gate
//! reads — and when the file is behind, "bump the version" produces a
//! version that already exists live with different columns, the one
//! thing the seed refuses. Measured 2026-09-21 on the boarding
//! cooldown (3ec04168): the bundle said v6, the live lineage was at
//! v7, the car bumped 6 -> 7 and declared cooldown 30, gated green,
//! rode a train and landed — and the live row still read cooldown 45
//! at v7 active. Every check in the gate reads files; a green gate
//! proves the file, and nothing proved the row. `train-window` sat
//! the same way that day (bundle v2, live v3 active), the next change
//! to it one bump from the same collision.
//!
//! THE JUDGEMENT IS THE SEED'S, NOT A SECOND COPY. Each bundle row is
//! put through `boss_jobs::bundle_seed::decide` — the decision table
//! the boot runs — against the lineage the system of record answers
//! today: a dry run of the next boot, read at the moment it matters.
//! This module only says which of the seed's outcomes an AUTHOR must
//! hear about, and in what words. It changes no refusal and writes
//! nothing; letting a boot reconcile a behind file upward is ruled
//! out on the record in the packet, since that is a boot overwriting
//! an operator's live decision.
//!
//! GENERIC OVER `Declared`, and the sweep joined through it: cadence
//! first (the registry where the collision was measured), then
//! stations and step plugins, whose files declare their version the
//! same way and so can collide the same way — each read by its own
//! loader and its own lineage route, never a guessed key.
//!
//! AND AT THE ONE DOOR EVERY CAR PASSES (the closing car of 5449111c).
//! orient says it to whoever reads orient; the 3ec04168 car was built
//! by a briefed builder, who never does. The gap the packet named is
//! that a green gate proves the file and nothing proves the row — so
//! `boss gate`, which already reads the system of record before it
//! files anything, puts every bundle row the CAR CHANGES through the
//! same `lineage_line` against the live lineage, and refuses the launch
//! when live will not take the row as written: it contradicts a live
//! version, sits behind the live newest, or names a lineage an operator
//! retired. Only a changed DECLARATION is judged (a comment edit on a
//! file that is behind by design stays possible), only the car's own
//! files (nothing another car or an operator did can red this one), and
//! an unread lineage PROCEEDS with a line — "cannot answer" is not
//! "collides", and this is the door every car passes. Every refusal
//! has a way through that is not an override: declare the live newest
//! plus one, or publish live first so the file writes back a row that
//! is already present. Shape 3 of the packet, scoped to the change
//! rather than the tree; shape 2 (a boot filing a packet) is answered
//! by these two readers instead of a third, since a boot reporting
//! through the API it boots is the arm that needs the patient.

use std::path::Path;

use boss_jobs::bundle_seed::{Declared, SeedOutcome, decide, differing_fields};
use boss_jobs::seed_loader::SeedLoaderError;

/// What an author must hear about one bundle row, or `None` when a
/// change to the file would publish the way it reads: the row is live
/// as declared, the file is ahead (the seed publishes it), or the name
/// is not live yet (the seed inserts it).
///
/// `registry` names the bundle directory in the line, so a reader can
/// open the file it is about.
pub(crate) fn lineage_line<S: Declared>(registry: &str, spec: &S, live: &[S]) -> Option<String> {
    let name = spec.name();
    let declared = spec.version();
    let newest = live.iter().map(Declared::version).max()?;
    let next = newest + 1;
    match decide(live, spec, true) {
        Err(refusal) => Some(format!(
            "{registry}/{name}: bundle v{declared} CONTRADICTS live v{declared} in {} — the \
             next boot's seed refuses it and the change it carries has had no effect; \
             declare v{next} (live newest is v{newest})",
            refusal.fields.join(", ")
        )),
        Ok(SeedOutcome::Behind { live_active } | SeedOutcome::Superseded { live_active }) => {
            Some(format!(
                "{registry}/{name}: bundle v{declared} is BEHIND the live lineage \
                 (newest v{newest}, v{live_active} active) — an edit here must declare \
                 v{next} or above, over what v{live_active} says, or it collides or is ignored"
            ))
        }
        Ok(SeedOutcome::Retired { newest }) => Some(format!(
            "{registry}/{name}: bundle v{declared}, live lineage RETIRED (newest v{newest}) — \
             the seed leaves it alone at any version; re-activation is an explicit publish"
        )),
        Ok(SeedOutcome::Inserted | SeedOutcome::Published { .. } | SeedOutcome::Present) => None,
    }
}

/// Where each versioned bundle lives in the tree — the directories the
/// boot seed publishes from (siblings of `infra/platform/workflows`).
const CADENCE_BUNDLE: &str = "infra/platform/cadence";
const STATIONS_BUNDLE: &str = "infra/platform/stations";
const STEP_PLUGINS_BUNDLE: &str = "infra/platform/step-plugins";

/// Each registry's own lineage route, keyed by the name its seed keys
/// by (`Declared::name` — a step plugin's `kind`).
fn cadence_versions(name: &str) -> String {
    format!("/api/cadence/rules/{name}/versions")
}
fn station_versions(name: &str) -> String {
    format!("/api/stations/{name}/versions")
}
fn step_plugin_versions(name: &str) -> String {
    format!("/api/jobs/step-plugins/{name}/versions")
}

/// orient's BUNDLES section for one registry, from what was read: how
/// many rows were compared, how many the registry has never held (the
/// window between a car's merge and the seed — said as a count,
/// because an empty lineage is also what a wrong target answers), the
/// lines an author must hear, and the rows whose lineage could not be
/// read. Pure, so the words are pinned without a socket.
pub(crate) fn section_lines(
    registry: &str,
    compared: usize,
    not_live: usize,
    flagged: &[String],
    unread: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    if flagged.is_empty() {
        out.push(format!(
            "\n  BUNDLES — {registry}: {} of {compared} row(s) judged against their live \
             lineage, none behind or contradicted ({not_live} not live yet)",
            compared - unread.len()
        ));
    } else {
        out.push(format!(
            "\n  BUNDLES — {} {registry} row(s) whose FILE a version bump would collide with \
             or be ignored against ({compared} compared, {not_live} not live yet). Bump from \
             the LIVE newest, never the file (5449111c):",
            flagged.len()
        ));
        out.extend(flagged.iter().map(|l| format!("    {l}")));
    }
    out.extend(
        unread
            .iter()
            .map(|u| format!("    UNREAD — {u} (not judged; an unread lineage is not agreement)")),
    );
    out
}

/// A name's whole live lineage, read from its registry's versions
/// route and decoded as the bundle's own type.
async fn live_lineage<S: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    route: &str,
) -> anyhow::Result<Vec<S>> {
    let rows =
        crate::train::rows(crate::gate::api(http, reqwest::Method::GET, route, None).await?)?;
    rows.into_iter()
        .map(serde_json::from_value::<S>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(anyhow::Error::from)
}

/// The rows of one bundle file whose DECLARATION a car changes: every
/// tip row the base did not declare, or declared with any column —
/// version included — different. A comment-only edit is none of them.
pub(crate) fn changed_rows<S: Declared>(base: &[S], tip: Vec<S>) -> Vec<S> {
    tip.into_iter()
        .filter(|t| {
            base.iter()
                .find(|b| b.name() == t.name())
                .is_none_or(|b| !differing_fields(b, t).is_empty())
        })
        .collect()
}

/// The changed paths that are ROW files of `bundle` — a `.toml`
/// directly in its directory, the shape `load_bundle_dir` reads.
pub(crate) fn paths_in<'a>(bundle: &str, changed: &'a [String]) -> Vec<&'a str> {
    changed
        .iter()
        .map(String::as_str)
        .filter(|p| {
            p.strip_prefix(bundle)
                .and_then(|rest| rest.strip_prefix('/'))
                .is_some_and(|file| !file.contains('/') && file.ends_with(".toml"))
        })
        .collect()
}

/// The launch refusal: every row the car changes that the live lineage
/// will not take as written, and the ways through that are not an
/// override.
pub(crate) fn car_refusal(sha: &str, lines: &[String]) -> String {
    let mut out = format!(
        "boss gate: REFUSED — {} bundle row(s) this car changes at {sha} would land with no \
         effect on the system of record, which is how the boarding cooldown landed green and \
         changed nothing (3ec04168, backlog 5449111c):\n",
        lines.len()
    );
    for l in lines {
        out.push_str(&format!("  {l}\n"));
    }
    out.push_str(
        "  Bump from the LIVE newest, never the file (`boss orient` BUNDLES says it for every \
         bundle); or publish the row live first, so the file writes back a row that is already \
         present. Nothing was filed.",
    );
    out
}

/// What `boss gate` learned about the bundle rows a car changes.
#[derive(Debug, Default)]
pub(crate) struct CarJudgement {
    /// One line per changed row the live lineage will not take.
    pub(crate) refused: Vec<String>,
    /// What could not be read, and so was not judged.
    pub(crate) unread: Vec<String>,
}

/// One registry's changed rows, judged against their live lineage.
#[allow(clippy::too_many_arguments)]
async fn judge_registry<S: Declared + serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    registry: &str,
    bundle: &str,
    parse: fn(&str, &str) -> Result<Vec<S>, SeedLoaderError>,
    versions: fn(&str) -> String,
    changed: &[String],
    read_base: &dyn Fn(&str) -> Option<String>,
    read_tip: &dyn Fn(&str) -> Option<String>,
    out: &mut CarJudgement,
) {
    for path in paths_in(bundle, changed) {
        // A file the car deleted declares nothing to judge.
        let Some(text) = read_tip(path) else { continue };
        let tip = match parse(&text, path) {
            Ok(tip) => tip,
            Err(e) => {
                out.unread.push(format!("{path}: {e}"));
                continue;
            }
        };
        let base = read_base(path)
            .and_then(|t| parse(&t, path).ok())
            .unwrap_or_default();
        for spec in changed_rows(&base, tip) {
            match live_lineage::<S>(http, &versions(spec.name())).await {
                Ok(live) => out.refused.extend(lineage_line(registry, &spec, &live)),
                Err(e) => out
                    .unread
                    .push(format!("{registry}/{}: {e:#}", spec.name())),
            }
        }
    }
}

/// Every versioned bundle row the car changes between `base` (its
/// merge-base with main) and `tip`, judged against the live lineage.
/// Never fails: what git or the system of record cannot answer comes
/// back in `unread`, and the caller proceeds on it.
pub(crate) async fn judge_car(
    http: &reqwest::Client,
    repo: &Path,
    base: &str,
    tip: &str,
) -> CarJudgement {
    use boss_jobs::seed_loader::{parse_cadence_rules, parse_stations, parse_step_plugins};
    let mut out = CarJudgement::default();
    if base.is_empty() {
        out.unread
            .push("the car's base could not be read, so no bundle row was judged".into());
        return out;
    }
    let diff = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["diff", "--name-only", base, tip, "--"])
        .args([CADENCE_BUNDLE, STATIONS_BUNDLE, STEP_PLUGINS_BUNDLE])
        .output();
    let changed: Vec<String> = match diff {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
        Ok(o) => {
            out.unread.push(format!(
                "git diff {base} {tip}: {}",
                String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .next()
                    .unwrap_or("no stderr")
            ));
            return out;
        }
        Err(e) => {
            out.unread.push(format!("could not run git: {e}"));
            return out;
        }
    };
    if changed.is_empty() {
        return out;
    }
    let read_base = crate::prove::git_show_reader(repo, base);
    let read_tip = crate::prove::git_show_reader(repo, tip);
    judge_registry(
        http,
        "cadence",
        CADENCE_BUNDLE,
        parse_cadence_rules,
        cadence_versions,
        &changed,
        &read_base,
        &read_tip,
        &mut out,
    )
    .await;
    judge_registry(
        http,
        "stations",
        STATIONS_BUNDLE,
        parse_stations,
        station_versions,
        &changed,
        &read_base,
        &read_tip,
        &mut out,
    )
    .await;
    judge_registry(
        http,
        "step-plugins",
        STEP_PLUGINS_BUNDLE,
        parse_step_plugins,
        step_plugin_versions,
        &changed,
        &read_base,
        &read_tip,
        &mut out,
    )
    .await;
    out
}

/// One registry's half of orient's BUNDLES section: every row this
/// checkout's bundle declares, put through the seed's decision against
/// the lineage the system of record answers now. Never fatal — a tree
/// or a read that fails prints why, and the approach still prints.
async fn registry_section<S: Declared + serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    registry: &str,
    bundle: &str,
    load: fn(&std::path::Path) -> Result<Vec<S>, boss_jobs::seed_loader::SeedLoaderError>,
    versions: fn(&str) -> String,
) -> Vec<String> {
    let dir = match crate::brief::repo_root() {
        Ok(root) => root.join(bundle),
        Err(e) => return vec![format!("\n  BUNDLES — {registry}: skipped: {e}")],
    };
    let specs = match load(&dir) {
        Ok(specs) => specs,
        Err(e) => {
            return vec![format!(
                "\n  BUNDLES — {registry}: skipped: could not read {}: {e}",
                dir.display()
            )];
        }
    };
    let (mut flagged, mut unread, mut not_live) = (Vec::new(), Vec::new(), 0);
    for spec in &specs {
        match live_lineage::<S>(http, &versions(spec.name())).await {
            Ok(live) if live.is_empty() => not_live += 1,
            Ok(live) => flagged.extend(lineage_line(registry, spec, &live)),
            Err(e) => unread.push(format!("{registry}/{}: {e:#}", spec.name())),
        }
    }
    section_lines(registry, specs.len(), not_live, &flagged, &unread)
}

/// orient's BUNDLES section: every versioned bundle whose registry
/// answers its lineage — cadence (where the collision was measured),
/// stations and step plugins (the sweep, 5449111c). Delivery policy
/// declares a version too, but its registry serves only the ACTIVE row
/// (`/api/delivery/policy/{name}`), and a lineage read as one row
/// would call a superseded declaration "behind" when it is the
/// operator's own history — so it waits for a versions route rather
/// than being judged from half the facts.
pub(crate) async fn bundles_section(http: &reqwest::Client) -> Vec<String> {
    use boss_jobs::seed_loader::{load_cadence_rules, load_stations, load_step_plugins};
    let mut out = registry_section(
        http,
        "cadence",
        CADENCE_BUNDLE,
        |d| load_cadence_rules(d),
        cadence_versions,
    )
    .await;
    out.extend(
        registry_section(
            http,
            "stations",
            STATIONS_BUNDLE,
            |d| load_stations(d),
            station_versions,
        )
        .await,
    );
    out.extend(
        registry_section(
            http,
            "step-plugins",
            STEP_PLUGINS_BUNDLE,
            |d| load_step_plugins(d),
            step_plugin_versions,
        )
        .await,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_jobs::cadence::CadenceRuleSpec;
    use serde_json::json;

    /// A cadence row as the wire carries it — the live lineage's shape
    /// (`GET /api/cadence/rules/{name}/versions`) and the bundle's.
    fn rule(version: i32, status: &str, cooldown: i32) -> CadenceRuleSpec {
        serde_json::from_value(json!({
            "name": "train-board-on-dock-depth",
            "version": version,
            "status": status,
            "verb": "board",
            "basis": "queue-depth",
            "min_dock_depth": 1,
            "cooldown_minutes": cooldown,
            "created_at": "2026-09-21T00:00:00Z",
        }))
        .unwrap()
    }

    /// The measured collision, 3ec04168: the bundle bumped 6 -> 7 with
    /// cooldown 30 while live v7 was already active at cooldown 45.
    /// The line names the field and the version that would publish.
    #[test]
    fn a_bundle_row_contradicting_its_live_twin_names_the_field_and_the_next_version() {
        let live = [rule(6, "retired", 45), rule(7, "active", 45)];
        let line = lineage_line("cadence", &rule(7, "active", 30), &live).unwrap();
        assert!(line.contains("cadence/train-board-on-dock-depth"), "{line}");
        assert!(line.contains("CONTRADICTS live v7"), "{line}");
        assert!(line.contains("cooldown_minutes"), "{line}");
        assert!(line.contains("declare v8"), "{line}");
    }

    /// `train-window` on 2026-09-21: bundle v2, live v2 retired and v3
    /// active. Nothing is wrong YET, and that is the point — the next
    /// author bumps 2 -> 3 and collides, so the line says v4.
    #[test]
    fn a_bundle_behind_its_lineage_says_the_version_an_edit_must_declare() {
        let live = [rule(2, "retired", 45), rule(3, "active", 30)];
        let line = lineage_line("cadence", &rule(2, "active", 45), &live).unwrap();
        assert!(line.contains("BEHIND"), "{line}");
        assert!(line.contains("newest v3, v3 active"), "{line}");
        assert!(line.contains("v4 or above"), "{line}");
        // The same when the declared version was never live at all.
        let live = [rule(5, "active", 45)];
        let line = lineage_line("cadence", &rule(3, "active", 45), &live).unwrap();
        assert!(
            line.contains("BEHIND") && line.contains("v6 or above"),
            "{line}"
        );
    }

    #[test]
    fn a_retired_lineage_is_named_as_the_operators_decision() {
        let live = [rule(1, "retired", 45), rule(2, "retired", 45)];
        let line = lineage_line("cadence", &rule(2, "active", 45), &live).unwrap();
        assert!(line.contains("RETIRED (newest v2)"), "{line}");
    }

    /// Silence is only for rows a change would publish as written:
    /// live as declared, ahead of live, or not live yet.
    #[test]
    fn a_bundle_that_publishes_as_written_says_nothing() {
        let live = [rule(1, "retired", 45), rule(2, "active", 45)];
        assert_eq!(lineage_line("cadence", &rule(2, "active", 45), &live), None);
        assert_eq!(lineage_line("cadence", &rule(3, "active", 30), &live), None);
        assert_eq!(lineage_line("cadence", &rule(1, "active", 45), &[]), None);
    }

    /// The section says what it compared even when nothing is wrong,
    /// and an unread lineage is named, never folded into agreement.
    #[test]
    fn the_section_counts_what_it_compared_and_names_what_it_could_not_read() {
        let quiet = section_lines("cadence", 3, 0, &[], &[]).join("\n");
        assert!(quiet.contains("cadence: 3 of 3 row(s) judged"), "{quiet}");
        assert!(quiet.contains("none behind"), "{quiet}");
        let loud = section_lines(
            "cadence",
            3,
            1,
            &["cadence/train-window: BEHIND".into()],
            &[],
        )
        .join("\n");
        assert!(loud.contains("1 cadence row(s)"), "{loud}");
        assert!(loud.contains("1 not live yet"), "{loud}");
        assert!(loud.contains("    cadence/train-window: BEHIND"), "{loud}");
        let unread =
            section_lines("cadence", 3, 0, &[], &["cadence/x: HTTP 502".into()]).join("\n");
        assert!(unread.contains("UNREAD — cadence/x: HTTP 502"), "{unread}");
        assert!(unread.contains("2 of 3 row(s) judged"), "{unread}");
    }

    /// The car-side half (5449111c): only a row whose DECLARATION the
    /// car changes is judged at launch. A comment edit on a file that
    /// sits behind live by design changes nothing the seed reads, so it
    /// stays possible; a new file, a column or a version is judged.
    #[test]
    fn only_a_row_whose_declaration_the_car_changes_is_judged() {
        let base = [rule(2, "active", 45)];
        assert!(changed_rows(&base, vec![rule(2, "active", 45)]).is_empty());
        assert_eq!(changed_rows(&base, vec![rule(3, "active", 45)]).len(), 1);
        assert_eq!(changed_rows(&base, vec![rule(2, "active", 30)]).len(), 1);
        assert_eq!(changed_rows(&[], vec![rule(2, "active", 45)]).len(), 1);
    }

    /// Only a bundle's own row files are read — a README edit in the
    /// same directory, or a file in another bundle, is not this one's.
    #[test]
    fn a_car_path_belongs_to_the_bundle_whose_directory_holds_its_toml() {
        let changed = [
            "infra/platform/cadence/train-window.toml".to_string(),
            "infra/platform/cadence/README.md".to_string(),
            "infra/platform/stations/loading-dock.toml".to_string(),
            "infra/platform/cadence-old/x.toml".to_string(),
        ];
        assert_eq!(
            paths_in(CADENCE_BUNDLE, &changed),
            vec!["infra/platform/cadence/train-window.toml"]
        );
    }

    /// The measured incident as the launch now sees it: 3ec04168's car
    /// bumped the boarding rule 6 -> 7 at cooldown 30 while live v7 was
    /// active at 45. It gated green and landed with no effect; here it
    /// is refused before a gate slot is spent, and the refusal names
    /// the row, the version that would publish, and the doors through.
    #[test]
    fn the_launch_refusal_names_every_row_and_the_way_through() {
        let live = [rule(6, "retired", 45), rule(7, "active", 45)];
        let base = [rule(6, "active", 45)];
        let lines: Vec<String> = changed_rows(&base, vec![rule(7, "active", 30)])
            .iter()
            .filter_map(|s| lineage_line("cadence", s, &live))
            .collect();
        let text = car_refusal("abc1234", &lines);
        assert!(text.contains("REFUSED"), "{text}");
        assert!(text.contains("abc1234"), "{text}");
        assert!(text.contains("CONTRADICTS live v7"), "{text}");
        assert!(text.contains("declare v8"), "{text}");
        assert!(text.contains("boss orient"), "{text}");
    }

    /// The packet's sweep (5449111c): stations and step plugins declare
    /// their version in the file exactly as cadence does, so they can
    /// collide the same way, and orient judges them too. Each directory
    /// is loaded by ITS OWN loader — the packet's hand measurement
    /// resolved two station names to a nested `kind`, which is why the
    /// sweep is code. A constant pointing at a moved directory would
    /// print "skipped" forever; this loads each from the tree.
    #[test]
    fn every_bundle_orient_judges_loads_from_the_tree_by_its_own_loader() {
        let root = crate::brief::repo_root().expect("repo root");
        let dir = |d: &str| root.join(d);
        let cadence = boss_jobs::seed_loader::load_cadence_rules(dir(CADENCE_BUNDLE)).unwrap();
        let stations = boss_jobs::seed_loader::load_stations(dir(STATIONS_BUNDLE)).unwrap();
        let plugins = boss_jobs::seed_loader::load_step_plugins(dir(STEP_PLUGINS_BUNDLE)).unwrap();
        assert!(!cadence.is_empty() && !stations.is_empty() && !plugins.is_empty());
        // The lineage path each is read from is the registry's own
        // versions route, keyed by the name the seed keys by.
        assert_eq!(
            station_versions("loading-dock"),
            "/api/stations/loading-dock/versions"
        );
        assert_eq!(
            step_plugin_versions("sign-off"),
            "/api/jobs/step-plugins/sign-off/versions"
        );
        assert!(stations.iter().any(|s| Declared::name(s) == "loading-dock"));
        assert!(plugins.iter().any(|p| Declared::name(p) == "sign-off"));
    }
}
