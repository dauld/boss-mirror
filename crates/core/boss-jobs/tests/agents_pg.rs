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
use boss_core::publisher::EventStamp;
use boss_jobs::agents::{AgentsRegistry, PgAgents};
use boss_testing::TestDb;

fn stamp() -> EventStamp {
    EventStamp::new("jobs", ActorId::Automation("tenant-seed".into()))
}

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
/// 2026-09-17; the rule since 09887242): a row the registry lacks is
/// inserted, a row the migration already registered is UPDATED to the
/// declaration and the batch names each change from → to, a re-run of
/// the same declaration changes nothing, an alias the tenant does not
/// declare is kept, and an unpriced default model is refused as
/// `Unpriced` naming the model — not as a storage error.
#[tokio::test(flavor = "multi_thread")]
async fn a_batch_registers_new_agents_and_updates_the_migrations_row_to_the_declaration() {
    use boss_jobs::agents::{AgentInput, AgentsError};
    let db = TestDb::new().await;
    let registry = PgAgents::new(db.pool.clone());
    let declared = |id: &str, name: &str, alias: &str| AgentInput {
        id: id.into(),
        display_name: name.into(),
        default_model: "opus-5[1m]".into(),
        aliases: vec![alias.into()],
        role: None,
        department: None,
        hourly_budget_usd_micros: None,
        max_concurrent_runs: None,
    };

    // A login an operator added by hand, which the tenant's file does
    // not list — the rule keeps it.
    sqlx::query("INSERT INTO actor_aliases (alias, actor_id) VALUES ($1, 'agent-claude')")
        .bind("ops-added@algedonic.dev")
        .execute(&db.pool)
        .await
        .unwrap();
    let out = registry
        .publish(
            &[
                // The migration's row, declared with the tenant's own name.
                declared(
                    "agent-claude",
                    "Claude (engineering)",
                    "claude@algedonic.dev",
                ),
                declared("agent-scout", "Scout", "scout@example.test"),
            ],
            &stamp(),
        )
        .await
        .expect("the batch lands");
    assert_eq!((out.received, out.inserted, out.unchanged), (2, 1, 0));
    assert_eq!(out.updated.len(), 1);
    assert_eq!(out.updated[0].id, "agent-claude");
    assert_eq!(
        out.updated[0].render(),
        "agent-claude (display_name Claude (Claude Code sessions on the dev pod) → Claude (engineering))"
    );

    let rows = registry.list().await.expect("list");
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["agent-claude", "agent-scout"]);
    assert_eq!(
        rows[0].display_name, "Claude (engineering)",
        "the tenant's declaration wins on the declared field"
    );
    assert_eq!(
        rows[0].aliases,
        ["claude@algedonic.dev", "ops-added@algedonic.dev"],
        "the undeclared alias is kept"
    );
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

    // A second, identical publish inserts nothing and changes nothing.
    let again = registry
        .publish(
            &[
                declared(
                    "agent-claude",
                    "Claude (engineering)",
                    "claude@algedonic.dev",
                ),
                declared("agent-scout", "Scout", "scout@example.test"),
            ],
            &stamp(),
        )
        .await
        .unwrap();
    assert_eq!((again.received, again.inserted, again.unchanged), (2, 0, 2));
    assert!(again.updated.is_empty(), "{:?}", again.updated);

    // A default model the rate card does not price is refused by name,
    // and the whole batch rolls back (agent-later never lands).
    let mut unpriced = declared("agent-unpriced", "U", "u@example.test");
    unpriced.default_model = "claude-opus-5".into();
    let err = registry
        .publish(
            &[declared("agent-later", "L", "l@example.test"), unpriced],
            &stamp(),
        )
        .await
        .unwrap_err();
    match err {
        AgentsError::Unpriced(m) => assert_eq!(m, "claude-opus-5"),
        other => panic!("expected Unpriced, got {other:?}"),
    }
    let after = registry.list().await.unwrap();
    assert!(
        !after.iter().any(|r| r.id == "agent-later"),
        "one transaction: nothing before the refused row stays landed"
    );

    // The facts ride the batch's transaction (backlog d9409039): of
    // everything above, exactly one row was inserted and committed —
    // agent-scout — so exactly one `agent.declared` is staged, and
    // exactly one row was changed — agent-claude, once — so exactly
    // one `agent.updated`. The re-run staged nothing, and agent-later's
    // fact rolled back with its row.
    let staged: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT source, payload FROM event_outbox WHERE kind = $1")
            .bind(boss_jobs::agents::AGENT_DECLARED)
            .fetch_all(&db.pool)
            .await
            .expect("outbox reads");
    assert_eq!(staged.len(), 1, "{staged:?}");
    assert_eq!(staged[0].0, "jobs");
    assert_eq!(staged[0].1["id"], "agent-scout");
    assert_eq!(
        staged[0].1["aliases"],
        serde_json::json!(["scout@example.test"])
    );
    assert_eq!(staged[0].1["declared_by"], "automation:tenant-seed");
    let changed: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT source, payload FROM event_outbox WHERE kind = $1")
            .bind(boss_jobs::agents::AGENT_UPDATED)
            .fetch_all(&db.pool)
            .await
            .expect("outbox reads");
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(changed[0].1["id"], "agent-claude");
    assert_eq!(changed[0].1["display_name"], "Claude (engineering)");
    assert_eq!(changed[0].1["changes"][0]["field"], "display_name");
    assert_eq!(
        changed[0].1["changes"][0]["from"],
        "Claude (Claude Code sessions on the dev pod)"
    );
    assert_eq!(changed[0].1["updated_by"], "automation:tenant-seed");
}

/// The two columns an agent shares with an employee (backlog ab192a9f):
/// the migration's row holds neither (NULL, not ""), a declaration
/// that names them UPDATES the row and names both changes from null, a
/// re-run changes nothing, the roster reads them back, and the
/// `agent.updated` fact carries them. The Class check is the door's
/// (http.rs), not the schema's — the registry is data, so a CHECK that
/// copied it would drift.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_holds_a_role_and_sits_in_a_department_like_an_employee() {
    use boss_jobs::agents::AgentInput;
    let db = TestDb::new().await;
    let registry = PgAgents::new(db.pool.clone());

    let seeded = registry.list().await.expect("list");
    let claude = seeded
        .iter()
        .find(|r| r.id == "agent-claude")
        .expect("the migration's row");
    assert_eq!(
        (claude.role.as_deref(), claude.department.as_deref()),
        (None, None)
    );

    let placed = AgentInput {
        id: "agent-claude".into(),
        display_name: "Claude (Claude Code sessions on the dev pod)".into(),
        default_model: "opus-5[1m]".into(),
        aliases: vec![],
        role: Some("engineering-agent".into()),
        department: Some("engineering".into()),
        hourly_budget_usd_micros: None,
        max_concurrent_runs: None,
    };
    let out = registry
        .publish(std::slice::from_ref(&placed), &stamp())
        .await
        .expect("lands");
    assert_eq!((out.inserted, out.unchanged), (0, 0));
    assert_eq!(
        out.updated[0].render(),
        "agent-claude (role null → engineering-agent, department null → engineering)"
    );
    let again = registry.publish(&[placed], &stamp()).await.expect("lands");
    assert_eq!((again.inserted, again.unchanged), (0, 1));
    assert!(again.updated.is_empty(), "{:?}", again.updated);

    let rows = registry.list().await.expect("list");
    let claude = rows.iter().find(|r| r.id == "agent-claude").unwrap();
    assert_eq!(claude.role.as_deref(), Some("engineering-agent"));
    assert_eq!(claude.department.as_deref(), Some("engineering"));
    let json = serde_json::to_value(claude).unwrap();
    assert_eq!(json["role"], "engineering-agent");

    let changed: Vec<(serde_json::Value,)> =
        sqlx::query_as("SELECT payload FROM event_outbox WHERE kind = $1")
            .bind(boss_jobs::agents::AGENT_UPDATED)
            .fetch_all(&db.pool)
            .await
            .expect("outbox reads");
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(changed[0].0["role"], "engineering-agent");
    assert_eq!(changed[0].0["department"], "engineering");
    assert_eq!(changed[0].0["changes"][0]["field"], "role");
    assert_eq!(changed[0].0["changes"][1]["field"], "department");
}
