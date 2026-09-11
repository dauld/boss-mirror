-- 20260910030644-an-agent-run-leaves-a-record.sql — an agent run is a
-- fact the system holds: which model ran, what it cost, how long it
-- took, and which car it produced.
--
-- Origin: backlog 83344e16. On 2026-09-10 five coding agents were
-- dispatched from the dev pod and four reported:
--
--   fix/the-mirrors-own-merge-is-not-foreign-work  142,982 tok  52 calls  10m31s
--   fix/a-parked-car-triages-its-item              218,218 tok  91 calls  14m15s
--   fix/ci-images-are-pruned-by-age                204,346 tok  68 calls  15m02s
--   feat/a-verb-answers-which-rules-are-live       196,036 tok  98 calls  18m12s
--
-- ~761,000 tokens and ~58 minutes of agent time. Every one of those
-- runs produced a car that lands on main, so the WORK is on the record
-- and its COST is not — it existed only in a terminal transcript that
-- dies with the session. CLAUDE.md's reading frame says the actors run
-- it and that actors are the part of the system that executes; an
-- executor with no instrumentation is the gap these two tables close.
--
-- WHY THERE IS NO `model` COLUMN. The model is read out of `actor_id`.
-- boss-core/src/actor.rs already made it a groupable dimension of the
-- actor id — `claude:opus-5` — and that module says in as many words
-- that no separate model key should be added. A `model` column beside
-- `actor_id` would be the same fact twice (CLAUDE.md §9a), so the
-- grouping happens in Rust off the one parse (`AgentRun::model`).
--
-- WHY `usd_micros` IS NULLABLE. NULL means no rate-card row covered
-- the model: the run is recorded, its cost is unknown, and the roll-up
-- counts unpriced runs out loud. Zero would read as FREE — a component
-- answering instead of erroring — and would silently understate a bill.

CREATE TABLE IF NOT EXISTS agent_rate_card (
    -- The model half of an agent ActorId: `opus-5`, not `claude:opus-5`
    -- and not the API's `claude-opus-5`. Matching is EXACT; a model no
    -- row names is unpriced, which is visible, rather than fuzzily
    -- matched to a neighbour, which is not.
    model                       TEXT PRIMARY KEY,
    -- Micro-USD per million tokens, integer for the same reason
    -- boss_core::agent::Cost is integer: $5.00/MTok = 5000000.
    input_usd_micros_per_mtok   BIGINT NOT NULL CHECK (input_usd_micros_per_mtok >= 0),
    output_usd_micros_per_mtok  BIGINT NOT NULL CHECK (output_usd_micros_per_mtok >= 0),
    -- Where the number came from, so a reader can check it rather than
    -- trust it.
    note                        TEXT NOT NULL DEFAULT '',
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE agent_rate_card IS
  'What a model''s tokens cost, in micro-USD per million tokens. '
  'READ-ONLY over the API by decision: a price arrives by migration, '
  'the way a dispatcher rule file does. The dispatcher registry accepts '
  'live authoring and on 2026-09-10 the running system enforced 64 rules '
  'while the tree held 60 files (backlog 8d471ec5) — four rows in no '
  'file, carrying no stated reason. A wrong price is harder to notice '
  'than a missing rule, so this registry does not take that risk yet.';

-- Anthropic first-party API list prices, per the bundled claude-api
-- reference cached 2026-06-24. Keys are the model strings as they
-- appear in an agent ActorId, which is why they carry no `claude-`
-- prefix. `opus-5[1m]` is the 1M-context Opus 5 the dev pod's own
-- sessions run as; the published table gives Opus 5 a 1M context at the
-- same per-token price, so it is the same rate, stated explicitly
-- rather than guessed at by a prefix match.
INSERT INTO agent_rate_card (model, input_usd_micros_per_mtok, output_usd_micros_per_mtok, note) VALUES
  ('opus-5',      5000000, 25000000, 'Claude Opus 5 — $5.00/$25.00 per MTok (Anthropic API list, cached 2026-06-24)'),
  ('opus-5[1m]',  5000000, 25000000, 'Claude Opus 5, 1M context — same rate as opus-5; the dev pod''s own session model'),
  ('opus-4-8',    5000000, 25000000, 'Claude Opus 4.8 — $5.00/$25.00 per MTok'),
  ('sonnet-5',    2000000, 10000000, 'Claude Sonnet 5 — $2.00/$10.00 per MTok'),
  ('sonnet-4-6',  3000000, 15000000, 'Claude Sonnet 4.6 — $3.00/$15.00 per MTok'),
  ('haiku-4-5',   1000000,  5000000, 'Claude Haiku 4.5 — $1.00/$5.00 per MTok'),
  ('fable-5',    10000000, 50000000, 'Claude Fable 5 — $10.00/$50.00 per MTok'),
  ('fable-5-1',  10000000, 50000000, 'Claude Fable 5.1 — $10.00/$50.00 per MTok')
ON CONFLICT (model) DO NOTHING;

CREATE TABLE IF NOT EXISTS agent_runs (
    -- Caller-minted, so a retried report records the run once. Same
    -- idempotence contract cadence_firings.firing_id rests on.
    run_id         TEXT PRIMARY KEY,
    -- The CPU that ran, in the `<mode>:<model>` agent form. Carries the
    -- model; see the header.
    actor_id       TEXT NOT NULL,
    -- Both bound by the caller from its own clock, never NOW(): the run
    -- happened when it happened, and a write-time reading would record
    -- when the REPORT arrived instead. Duration is derived from the
    -- pair and deliberately not stored.
    started_at     TIMESTAMPTZ NOT NULL,
    finished_at    TIMESTAMPTZ NOT NULL,
    outcome        TEXT NOT NULL CHECK (outcome IN ('success', 'failed', 'cancelled')),
    error          TEXT,
    input_tokens   BIGINT NOT NULL CHECK (input_tokens >= 0),
    output_tokens  BIGINT NOT NULL CHECK (output_tokens >= 0),
    -- The other half of "what did this cost", and the one a token count
    -- cannot recover.
    tool_calls     INTEGER NOT NULL DEFAULT 0 CHECK (tool_calls >= 0),
    -- NULL = unpriced, NOT free. See the header.
    usd_micros     BIGINT CHECK (usd_micros >= 0),
    -- Which rate-card row priced it, so the arithmetic stays checkable
    -- after the card changes.
    priced_by      TEXT,
    -- The packet the run was working, when one exists. No FK: a builder
    -- knows its branch before any packet is filed for it (backlog
    -- be025b44), and a constraint here would refuse the honest record.
    job_id         UUID,
    -- The car the run produced.
    branch         TEXT,
    detail         JSONB NOT NULL DEFAULT '{}',
    -- The recording event's timestamp, so the row and the log entry
    -- share one instant.
    recorded_at    TIMESTAMPTZ NOT NULL
);

COMMENT ON TABLE agent_runs IS
  'A PROJECTION of agents.run.recorded, not an independent record. '
  'The audit log is the system of record; boss_jobs::agent_runs::rebuild '
  'reproduces every row from it. A rebuild replays the recorded price '
  'rather than re-pricing against today''s card — re-pricing would make '
  'a rebuild change history.';

COMMENT ON COLUMN agent_runs.usd_micros IS
  'Micro-USD the run cost at the rate card in force when it was '
  'recorded. NULL means no card row covered the model: unpriced, not '
  'free. A roll-up over a set containing one of these reports no total '
  'at all rather than an understated one.';

-- The three questions asked of this table: what did this car cost,
-- what did this packet cost, and what has this actor spent lately.
CREATE INDEX IF NOT EXISTS idx_agent_runs_branch ON agent_runs (branch) WHERE branch IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_agent_runs_job ON agent_runs (job_id) WHERE job_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_agent_runs_actor_finished ON agent_runs (actor_id, finished_at DESC);

-- The emitted kind must be declared, or it rides inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('agents.run.recorded', 'jobs', 'An agent run was recorded: its actor (which carries the model), tokens, tool calls, duration, outcome and priced cost', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
