//! The viability lint validates a step's `agent.model` against
//! [`boss_jobs::agent_spec::known_models`], which is read at compile
//! time from the migration that seeds `agent_rate_card` — because the
//! lint is synchronous and the card lives in Postgres (design c87fb59b
//! car 1, backlog 028891cf). Two readers of one fact, so this is the
//! equality test CLAUDE.md §9a asks for: the LIVE table, applied from
//! the whole schema directory, prices exactly the models the lint
//! admits. A migration that adds a priced model the compiled reader
//! does not see fails here by name, as does one that retires a model
//! the lint would still publish.

use boss_jobs::agent_runs::{AgentRunLog, PgAgentRuns};
use boss_jobs::agent_spec::known_models;
use boss_testing::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn the_models_a_step_may_name_are_the_rate_cards_rows() {
    let db = TestDb::new().await;
    let log = PgAgentRuns::new(db.pool.clone());
    let mut live: Vec<String> = log
        .rate_card()
        .await
        .expect("the rate card reads")
        .into_iter()
        .map(|r| r.model)
        .collect();
    live.sort();
    let mut compiled = known_models().to_vec();
    compiled.sort();
    assert_eq!(
        live, compiled,
        "the rate card the database prices and the models the publish lint admits disagree"
    );
    assert!(
        live.iter().any(|m| m == "opus-5[1m]"),
        "the bundle's declared model is priced: {live:?}"
    );
}
