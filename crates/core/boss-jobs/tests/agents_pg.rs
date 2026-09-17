//! Postgres half of the agents registry (design 6fda05ae).
//!
//! The in-memory door test proves the RULE; this file proves the
//! SCHEMA, which holds three facts the rule leans on:
//!
//!   * the migration seeds the one live alias — the pod's own
//!     `claude@algedonic.dev` resolves to `agent-claude` on a fresh
//!     database, so the day this lands the address stops reaching
//!     steps without anyone editing a row;
//!   * an alias cannot point at an unregistered actor, and a registered
//!     actor's id cannot be anything but `agent-<slug>` — the SQL
//!     spelling of `boss_core::actor::REGISTERED_AGENT_PREFIX`, so an
//!     id this table admits is one `ActorId` parses as an agent;
//!   * an agent's default model must be a rate-card row: a default that
//!     cannot be priced cannot be registered.
//!
//! The raw INSERTs go around the Rust adapter on purpose — the adapter
//! only reads — because the question is whether the DATABASE refuses.

use boss_core::actor::ActorId;
use boss_jobs::agents::{AgentsRegistry, PgAgents};
use boss_testing::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn the_seeded_alias_resolves_and_an_unknown_login_does_not() {
    let db = TestDb::new().await;
    let registry = PgAgents::new(db.pool.clone());

    let resolved = registry
        .resolve_login("claude@algedonic.dev")
        .await
        .expect("registry answers");
    assert_eq!(resolved.as_deref(), Some("agent-claude"));
    // What the door signs with parses as the agent class, not staff.
    let actor: ActorId = resolved.unwrap().parse().unwrap();
    assert!(actor.is_agent() && !actor.is_human(), "{actor:?}");

    assert_eq!(
        registry
            .resolve_login("nobody@example.test")
            .await
            .expect("registry answers"),
        None,
        "an unknown login is an answer (None), not an error"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_schema_refuses_an_alias_to_nothing_and_a_mis_shaped_id() {
    let db = TestDb::new().await;

    let orphan = sqlx::query("INSERT INTO actor_aliases (alias, actor_id) VALUES ($1, $2)")
        .bind("ghost@example.test")
        .bind("agent-ghost")
        .execute(&db.pool)
        .await;
    assert!(
        orphan.is_err(),
        "an alias must name a registered agent (FK), got {orphan:?}"
    );

    for bad in [
        "claude@algedonic.dev",
        "claude:opus-5",
        "emp-claude",
        "agent-",
        "agent-Claude",
    ] {
        let r = sqlx::query(
            "INSERT INTO agents (id, display_name, default_model) VALUES ($1, 'x', 'opus-5')",
        )
        .bind(bad)
        .execute(&db.pool)
        .await;
        assert!(r.is_err(), "{bad:?} is not an agent id, got {r:?}");
    }

    let unpriced = sqlx::query(
        "INSERT INTO agents (id, display_name, default_model) VALUES ('agent-x', 'x', 'claude-opus-5')",
    )
    .execute(&db.pool)
    .await;
    assert!(
        unpriced.is_err(),
        "a default model must be a rate-card row (the card spells it opus-5, not claude-opus-5), got {unpriced:?}"
    );

    // And the well-formed row goes in, with its alias.
    sqlx::query(
        "INSERT INTO agents (id, display_name, default_model) VALUES ('agent-x', 'x', 'opus-5')",
    )
    .execute(&db.pool)
    .await
    .expect("a well-formed agent registers");
    sqlx::query("INSERT INTO actor_aliases (alias, actor_id) VALUES ('x@example.test', 'agent-x')")
        .execute(&db.pool)
        .await
        .expect("its alias registers");
    let registry = PgAgents::new(db.pool.clone());
    assert_eq!(
        registry
            .resolve_login("x@example.test")
            .await
            .unwrap()
            .as_deref(),
        Some("agent-x")
    );
}

/// The tenant's batch against the real tables (backlog f56155f0,
/// 2026-09-17): insert-if-absent by id, aliases insert-if-absent by
/// alias, a row the migration already registered is KEPT and the batch
/// names which declared fields differ, and an unpriced default model is
/// refused as `Unpriced` naming the model — not as a storage error.
#[tokio::test(flavor = "multi_thread")]
async fn a_batch_registers_new_agents_keeps_the_migrations_row_and_names_what_differs() {
    use boss_jobs::agents::{AgentInput, AgentsError};
    let db = TestDb::new().await;
    let registry = PgAgents::new(db.pool.clone());
    let declared = |id: &str, name: &str, alias: &str| AgentInput {
        id: id.into(),
        display_name: name.into(),
        default_model: "opus-5[1m]".into(),
        aliases: vec![alias.into()],
        hourly_budget_usd_micros: None,
        max_concurrent_runs: None,
    };

    let out = registry
        .publish(&[
            // The migration's row, declared with the tenant's own name.
            declared(
                "agent-claude",
                "Claude (engineering)",
                "claude@algedonic.dev",
            ),
            declared("agent-scout", "Scout", "scout@example.test"),
        ])
        .await
        .expect("the batch lands");
    assert_eq!((out.received, out.inserted), (2, 1));
    assert_eq!(out.kept.len(), 1);
    assert_eq!(out.kept[0].id, "agent-claude");
    assert_eq!(out.kept[0].differs, ["display_name"]);

    let rows = registry.list().await.expect("list");
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["agent-claude", "agent-scout"]);
    assert_eq!(
        rows[0].display_name, "Claude (Claude Code sessions on the dev pod)",
        "the migration's row is kept as registered, never overwritten"
    );
    assert_eq!(rows[0].aliases, ["claude@algedonic.dev"]);
    assert_eq!(rows[1].aliases, ["scout@example.test"]);
    assert_eq!(
        registry
            .resolve_login("scout@example.test")
            .await
            .unwrap()
            .as_deref(),
        Some("agent-scout"),
        "the declared alias resolves through the login door"
    );

    // A second, identical publish inserts nothing and differs nowhere.
    let again = registry
        .publish(&[declared("agent-scout", "Scout", "scout@example.test")])
        .await
        .unwrap();
    assert_eq!((again.received, again.inserted), (1, 0));
    assert_eq!(again.kept[0].differs, Vec::<String>::new());

    // A default model the rate card does not price is refused by name,
    // and the whole batch rolls back (agent-later never lands).
    let mut unpriced = declared("agent-unpriced", "U", "u@example.test");
    unpriced.default_model = "claude-opus-5".into();
    let err = registry
        .publish(&[unpriced, declared("agent-later", "L", "l@example.test")])
        .await
        .unwrap_err();
    match err {
        AgentsError::Unpriced(m) => assert_eq!(m, "claude-opus-5"),
        other => panic!("expected Unpriced, got {other:?}"),
    }
    let after = registry.list().await.unwrap();
    assert!(
        !after.iter().any(|r| r.id == "agent-later"),
        "one transaction: nothing after the refused row landed"
    );
}
