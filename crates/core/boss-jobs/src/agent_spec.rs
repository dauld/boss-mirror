//! The typed `agent` block on a protocol step — design c87fb59b
//! (decided 2026-09-18), car 1 (backlog 028891cf).
//!
//! An agent executes a step the way a person does: it claims it, does
//! the procedure, completes it. What the protocol could not say until
//! this landed was HOW the agent should run — which model, at what
//! effort, under what spend, in which profile. Measured on 028891cf:
//! `StepSpec` carried no execution-profile field at all; the steps
//! agents complete today (backlog-item `build`, protocol-retro
//! `collect`/`analyze`/`gaps`/`report`) name a procedure and a role and
//! nothing else, so every run took the session's own defaults and the
//! record could not say whether that was the protocol's choice.
//!
//! [`AgentSpec`] is the one declaration:
//! `agent = { profile = "builder", model = "opus-5[1m]", budget_usd = 5,
//! effort = "high" }`. The prompt is NOT here — it is the step's own
//! `procedure`, which already exists. Like [`crate::audience`] it is
//! projected onto the materialised step's metadata by ONE function
//! ([`projection`]) as four plain keys — `agent_profile`, `agent_model`,
//! `agent_budget_usd`, `agent_effort` — so a station's
//! `step.metadata_equals` and the claim door (a later car) read the
//! packet, never the spec: the dispatcher reacts to an event with no
//! workflow row in hand, the same reason `authority_role` and
//! `claimable` ride there.
//!
//! **The effort must reach a control** (backlog e720dd00, 2026-09-19).
//! A declared effort that nothing applies reads as fact and is not one:
//! every builder dispatched before this recorded `effort = high` and
//! ran at the session default. The harness takes reasoning effort from
//! an agent DEFINITION, so [`definition_name`] is the mapping from a
//! declared effort to the file under [`DEFINITIONS_DIR`] that runs at
//! it, and `boss dispatch` names it on the Agent call. The definitions
//! are one file written [`Effort::ALL`] times with one word changed —
//! same [`DEFINITION_MODEL`] throughout, because a definition pairing a
//! cheaper model with a lower effort would confound the measurement
//! this exists to enable. The tests below pin the roster, the name, the
//! effort line and the model; that the harness HONOURS the key is
//! attested by its documentation, not measured here.
//!
//! **The model must be priced.** `agent_rate_card` is the registry of
//! models the system can cost (`agent_runs`); a step naming a model the
//! card does not hold would run unpriced, which is the failure that
//! table's own comment refuses. The card is only reachable through
//! Postgres and the viability lint is synchronous, so [`known_models`]
//! reads the card's SEEDING MIGRATION at compile time — one definition,
//! not a second list — and two tests pin it: every `INSERT INTO
//! agent_rate_card` across the schema directory yields exactly this
//! set, and the live table (`PgAgentRuns::rate_card`) agrees.

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

/// How hard the agent should think. A closed set: serde refuses any
/// other word at parse, naming the step in the seed loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    /// Every effort a block may declare, and so every effort that
    /// needs a definition to run at. The roster the definition tests
    /// iterate — one set, not a second list beside this enum.
    pub const ALL: [Effort; 3] = [Self::Low, Self::Medium, Self::High];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Where the agent definitions live, relative to the repo root — the
/// project-level directory Claude Code reads a `subagent_type` from.
pub const DEFINITIONS_DIR: &str = ".claude/agents";

/// The model EVERY definition declares, and the family every `agent`
/// block's model must belong to (backlog e720dd00). Fixed on purpose:
/// the series this enables varies EFFORT and nothing else, and a
/// definition pairing a cheaper model with a lower effort — the
/// documentation's own example does exactly that — would make no
/// observation attributable to either.
pub const DEFINITION_MODEL: &str = "opus";

/// The definition that runs a step at `effort` — the `subagent_type`
/// the dispatched prompt names and the hook sets on the Agent call.
/// THE mapping, in one place: the file is `<name>.md` under
/// [`DEFINITIONS_DIR`] and its `effort:` line is `effort`.
pub fn definition_name(effort: &str) -> String {
    format!("effort-{effort}")
}

/// The block. Every key is required within it — a block that names a
/// model and no budget is half a declaration — and an unknown key is
/// refused rather than ignored, so `buget_usd` cannot pass as "no
/// budget".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    /// The agent profile the run is launched under (`builder`,
    /// `analyst`, …). Free text here; the claim door interprets it.
    pub profile: String,
    /// A model `agent_rate_card` prices, spelled as the card spells it
    /// (`opus-5[1m]`, never `claude-…`). Validated at publish by the
    /// viability lint against [`known_models`].
    pub model: String,
    /// The spend cap for one run of this step, in USD. Positive.
    pub budget_usd: f64,
    pub effort: Effort,
}

/// The metadata keys [`projection`] writes, in the order it writes them.
pub const PROFILE_KEY: &str = "agent_profile";
pub const MODEL_KEY: &str = "agent_model";
pub const BUDGET_KEY: &str = "agent_budget_usd";
pub const EFFORT_KEY: &str = "agent_effort";
pub const KEYS: [&str; 4] = [PROFILE_KEY, MODEL_KEY, BUDGET_KEY, EFFORT_KEY];

/// THE projection: one block, four plain keys on the packet. Pure and
/// total, and the only place the mapping lives — the sibling of
/// [`crate::audience::selectors_for`].
pub fn projection(spec: &AgentSpec) -> [(&'static str, serde_json::Value); 4] {
    [
        (PROFILE_KEY, serde_json::Value::String(spec.profile.clone())),
        (MODEL_KEY, serde_json::Value::String(spec.model.clone())),
        (BUDGET_KEY, serde_json::json!(spec.budget_usd)),
        (
            EFFORT_KEY,
            serde_json::Value::String(spec.effort.as_str().to_string()),
        ),
    ]
}

/// The migration that seeds `agent_rate_card`. Read at compile time so
/// the lint can name the priced models without a database; the pin
/// tests below hold it equal to the whole schema directory and to the
/// live table.
const RATE_CARD_SEED: &str = include_str!(
    "../../../../infra/postgres/schema/20260910030644-an-agent-run-leaves-a-record.sql"
);

/// Every model an `INSERT INTO agent_rate_card … VALUES` block in `sql`
/// names, in file order. Line-based: a VALUES row is written one per
/// line as `('<model>', …)`, and the block ends at its `ON CONFLICT` or
/// terminating `;`.
pub fn models_in(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in sql.lines() {
        let t = line.trim();
        if t.starts_with("--") {
            continue;
        }
        if !in_block {
            in_block = t.starts_with("INSERT INTO agent_rate_card");
            if !in_block {
                continue;
            }
        }
        if let Some(rest) = t.strip_prefix("('")
            && let Some(end) = rest.find('\'')
        {
            out.push(rest[..end].to_string());
        }
        if t.starts_with("ON CONFLICT") || t.ends_with(';') {
            in_block = false;
        }
    }
    out
}

/// The models the rate card prices — the set a step's `agent.model`
/// must belong to.
pub fn known_models() -> &'static [String] {
    static MODELS: LazyLock<Vec<String>> = LazyLock::new(|| models_in(RATE_CARD_SEED));
    &MODELS
}

/// Why a block cannot be published, or `None` when it can. Shape-level
/// only (the lint adds the step and workflow names): a blank profile, a
/// model the card does not price — the refusal names every model it
/// does — or a budget that is not a positive finite number.
pub fn refusal(spec: &AgentSpec) -> Option<String> {
    if spec.profile.trim().is_empty() {
        return Some("agent.profile is blank — name the profile the run launches under".into());
    }
    if !known_models().iter().any(|m| m == &spec.model) {
        return Some(format!(
            "agent.model `{}` is not on the rate card, so a run of it could not be priced — \
             known models: {}",
            spec.model,
            known_models().join(", ")
        ));
    }
    if !(spec.budget_usd.is_finite() && spec.budget_usd > 0.0) {
        return Some(format!(
            "agent.budget_usd must be a positive number, got {}",
            spec.budget_usd
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> AgentSpec {
        AgentSpec {
            profile: "builder".into(),
            model: "opus-5[1m]".into(),
            budget_usd: 5.0,
            effort: Effort::High,
        }
    }

    #[test]
    fn the_wire_shape_is_the_designs_own_and_round_trips() {
        let json = serde_json::json!({
            "profile": "builder", "model": "opus-5[1m]", "budget_usd": 5, "effort": "high"
        });
        let parsed: AgentSpec = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, builder());
        let back = serde_json::to_value(&parsed).unwrap();
        assert_eq!(back["budget_usd"], 5.0);
        assert_eq!(back["effort"], "high");
        assert_eq!(serde_json::from_value::<AgentSpec>(back).unwrap(), parsed);
    }

    #[test]
    fn effort_is_a_closed_set_and_every_key_is_required() {
        let parse = |s: &str| serde_json::from_str::<AgentSpec>(s);
        let err = parse(r#"{"profile":"b","model":"opus-5","budget_usd":1,"effort":"max"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown variant `max`"), "{err}");
        assert!(err.contains("low"), "names the set: {err}");
        for missing in [
            r#"{"model":"opus-5","budget_usd":1,"effort":"high"}"#,
            r#"{"profile":"b","budget_usd":1,"effort":"high"}"#,
            r#"{"profile":"b","model":"opus-5","effort":"high"}"#,
            r#"{"profile":"b","model":"opus-5","budget_usd":1}"#,
        ] {
            assert!(parse(missing).is_err(), "{missing}");
        }
        // A misspelled key is refused, not read as absent.
        let err = parse(
            r#"{"profile":"b","model":"opus-5","buget_usd":1,"budget_usd":1,"effort":"high"}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unknown field `buget_usd`"), "{err}");
    }

    #[test]
    fn the_projection_is_four_plain_keys() {
        let keys = projection(&builder());
        assert_eq!(
            keys.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            KEYS.to_vec()
        );
        let map: serde_json::Map<String, serde_json::Value> =
            keys.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        assert_eq!(map["agent_profile"], "builder");
        assert_eq!(map["agent_model"], "opus-5[1m]");
        assert_eq!(map["agent_budget_usd"], 5.0);
        assert_eq!(map["agent_effort"], "high");
    }

    #[test]
    fn the_seeding_migration_names_the_pods_own_model() {
        let models = known_models();
        assert!(
            models.iter().any(|m| m == "opus-5[1m]"),
            "the dev pod's session model is priced: {models:?}"
        );
        assert!(models.len() >= 8, "{models:?}");
    }

    /// The one definition is the schema directory, whole: a later
    /// migration that prices a new model must reach [`known_models`],
    /// and this test names the model it did not (CLAUDE.md §9a).
    #[test]
    fn every_rate_card_insert_in_the_schema_directory_is_known() {
        let dir = boss_testing::repo_root().join("infra/postgres/schema");
        let mut from_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("schema directory")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "sql"))
            .flat_map(|p| models_in(&std::fs::read_to_string(p).expect("readable")))
            .collect();
        from_disk.sort();
        from_disk.dedup();
        let mut known = known_models().to_vec();
        known.sort();
        assert_eq!(
            from_disk, known,
            "a migration prices a model the compiled seed does not know — add its file to \
             agent_spec::known_models"
        );
    }

    #[test]
    fn models_in_reads_only_the_rate_card_block() {
        let sql = "INSERT INTO other (a) VALUES\n  ('nope');\n\
                   -- ('commented-out')\n\
                   INSERT INTO agent_rate_card (model, x) VALUES\n  ('m-1', 1),\n  ('m-2', 2)\n\
                   ON CONFLICT (model) DO NOTHING;\n\
                   INSERT INTO event_kinds (k) VALUES\n  ('agents.run.recorded');\n";
        assert_eq!(models_in(sql), vec!["m-1".to_string(), "m-2".to_string()]);
    }

    #[test]
    fn a_refusal_names_the_fix() {
        assert_eq!(refusal(&builder()), None);

        let bad_model = AgentSpec {
            model: "claude-opus-5".into(),
            ..builder()
        };
        let why = refusal(&bad_model).expect("refused");
        assert!(why.contains("`claude-opus-5`"), "{why}");
        assert!(why.contains("opus-5[1m]"), "names the known models: {why}");
        assert!(why.contains("haiku-4-5"), "{why}");

        for budget in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            let bad = AgentSpec {
                budget_usd: budget,
                ..builder()
            };
            let why = refusal(&bad).expect("refused");
            assert!(why.contains("positive"), "{why}");
        }

        let blank = AgentSpec {
            profile: "  ".into(),
            ..builder()
        };
        assert!(refusal(&blank).unwrap().contains("profile"));
    }

    // ---- The effort definitions (backlog e720dd00, 2026-09-19) ----

    /// The `key: value` frontmatter of a definition file: the lines
    /// between the opening and the closing `---`.
    fn frontmatter(text: &str) -> Vec<(String, String)> {
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("---"), "a definition opens with ---");
        lines
            .take_while(|l| l.trim() != "---")
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect()
    }

    /// Whole-word blanking, so two definitions can be compared for
    /// anything OTHER than the effort they declare — `low` is a
    /// substring of `follow`, so a plain replace would not do.
    fn blank(text: &str, word: &str) -> String {
        let (b, w) = (text.as_bytes(), word.as_bytes());
        let mut out: Vec<u8> = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            let ends = i + w.len();
            let whole = ends <= b.len()
                && &b[i..ends] == w
                && (i == 0 || !b[i - 1].is_ascii_alphanumeric())
                && (ends == b.len() || !b[ends].is_ascii_alphanumeric());
            if whole {
                out.extend_from_slice(b"EFFORT");
                i = ends;
            } else {
                // Byte-wise on purpose: the prose carries em dashes,
                // and a char-boundary slice would panic on them.
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).expect("the replacement is ASCII, the rest is copied verbatim")
    }

    fn definition_text(effort: Effort) -> String {
        let path = boss_testing::repo_root()
            .join(DEFINITIONS_DIR)
            .join(format!("{}.md", definition_name(effort.as_str())));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// The declaration must reach a control: every effort an `agent`
    /// block is ALLOWED to declare has a definition under
    /// `.claude/agents/`, and that definition declares that effort.
    /// The set is exactly [`Effort::ALL`] in both directions — a
    /// definition for a value [`Effort`] refuses would be a CPU no
    /// block can ask for.
    #[test]
    fn every_declarable_effort_has_a_definition_that_declares_it() {
        let dir = boss_testing::repo_root().join(DEFINITIONS_DIR);
        for effort in Effort::ALL {
            let name = definition_name(effort.as_str());
            let text = definition_text(effort);
            let fm = frontmatter(&text);
            let get = |k: &str| {
                fm.iter()
                    .find(|(a, _)| a == k)
                    .map(|(_, v)| v.trim_matches('"').to_string())
            };
            assert_eq!(
                get("name").as_deref(),
                Some(name.as_str()),
                "{name}: the frontmatter name must equal the filename stem"
            );
            assert_eq!(
                get("effort").as_deref(),
                Some(effort.as_str()),
                "{name}: the effort line must equal the effort in its name, or the block's \
                 declaration selects a CPU running at some other budget"
            );
            assert_eq!(
                get("model").as_deref(),
                Some(DEFINITION_MODEL),
                "{name}: hold the model FIXED across the definitions — a definition pairing a \
                 cheaper model with a lower effort confounds the measurement this exists for"
            );
            assert!(
                get("description").is_some_and(|d| !d.is_empty()),
                "{name}: description is required frontmatter"
            );
            // Scope fence (e720dd00): permission is not effort.
            assert!(
                get("permissionMode").is_none(),
                "{name}: permissionMode is unrelated to effort and must not ride this file"
            );
        }

        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("the definitions directory")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
            .filter(|s| s.starts_with("effort-"))
            .collect();
        on_disk.sort();
        let mut declarable: Vec<String> = Effort::ALL
            .iter()
            .map(|e| definition_name(e.as_str()))
            .collect();
        declarable.sort();
        assert_eq!(
            on_disk, declarable,
            "the definitions and the efforts a block may declare are one set — widening one \
             without the other leaves either a CPU nothing can ask for or a declaration with \
             no CPU"
        );
    }

    /// The experiment this enables is a series where EFFORT varies and
    /// nothing else does, so the definitions must be one file written
    /// three times with one word changed.
    #[test]
    fn the_definitions_differ_only_in_the_effort_they_declare() {
        let blanked: Vec<(Effort, String)> = Effort::ALL
            .iter()
            .map(|e| (*e, blank(&definition_text(*e), e.as_str())))
            .collect();
        let (first_effort, first) = &blanked[0];
        for (effort, text) in &blanked[1..] {
            assert_eq!(
                text,
                first,
                "{} and {} differ in more than their effort — the measurement would not be \
                 attributable to effort",
                definition_name(first_effort.as_str()),
                definition_name(effort.as_str())
            );
        }
    }

    /// The other half of "its model matches the block's": every agent
    /// block in the authored protocols names a model of the family the
    /// definitions fix. A block declaring some other family would run
    /// under a definition that silently changes the model too.
    #[test]
    fn every_authored_agent_block_names_a_model_the_definitions_model_covers() {
        let dir = boss_testing::repo_root().join("infra/platform/workflows");
        let mut seen = 0usize;
        for entry in std::fs::read_dir(&dir).expect("the workflows directory") {
            let path = entry.expect("a directory entry").path();
            if path.extension().is_none_or(|x| x != "toml") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("readable");
            for line in text.lines() {
                let t = line.trim();
                if !t.starts_with("agent = {") {
                    continue;
                }
                let model = t
                    .split_once("model = \"")
                    .and_then(|(_, rest)| rest.split_once('"'))
                    .map(|(m, _)| m)
                    .unwrap_or_else(|| {
                        panic!("{}: an agent block naming no model", path.display())
                    });
                seen += 1;
                assert!(
                    model.starts_with(DEFINITION_MODEL),
                    "{}: the block declares model `{model}`, which the definitions' fixed \
                     `model: {DEFINITION_MODEL}` does not cover — a run of it would change the \
                     model as well as the effort",
                    path.display()
                );
            }
        }
        assert!(
            seen >= 10,
            "the authored protocols carry agent blocks: {seen}"
        );
    }
}
