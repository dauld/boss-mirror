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

use boss_jobs::bundle_seed::{Declared, SeedOutcome, decide};

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
        let live = crate::gate::api(http, reqwest::Method::GET, &versions(spec.name()), None)
            .await
            .and_then(crate::train::rows)
            .and_then(|rows| {
                rows.into_iter()
                    .map(serde_json::from_value::<S>)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(anyhow::Error::from)
            });
        match live {
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
