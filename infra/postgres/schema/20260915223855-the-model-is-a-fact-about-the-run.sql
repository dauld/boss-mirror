-- 20260915223855-the-model-is-a-fact-about-the-run.sql — an agent run
-- names the model it ran on in a column of its own.
--
-- Origin: backlog 7dd9f28c, decided as design 6fda05ae (2026-09-15),
-- resolution model-on-run: "On the RUN (agent_runs.model), because one
-- registered agent may run different models over time and cost is
-- priced per run; the agent row carries the default model and the
-- caps." This is car 2 of that design. Car 1 (20260915212644) gave an
-- agent ONE id — `agent-claude`, an `agents` row with a default_model
-- — and left agent_runs alone; until this file, a run could name its
-- CPU only in the colon form `claude:opus-5[1m]`, because the model
-- had nowhere else to live.
--
-- WHY THE HEADER OF 20260910030644 IS NOW WRONG, AND WAS RIGHT THEN.
-- "A `model` column beside `actor_id` would be the same fact twice"
-- held while the actor id CARRIED the model. It no longer does: a
-- registered agent's id is model-free by decision, so for every run
-- from here on the column is the only place the fact lives. There is
-- no second copy to drift from. For the rows written before this file
-- the colon form still spells the model, and the backfill below reads
-- it out ONCE, by the same rule boss_jobs::agent_runs::rebuild applies
-- to an event written before the column existed — so a migrated table
-- and a rebuilt one agree (determinism).
--
-- WHY NO FOREIGN KEY TO agent_rate_card. agents.default_model has one:
-- a default that cannot be priced must not be registrable. A run is a
-- different kind of row — it is the record of what HAPPENED, and a run
-- on a model the card does not name is still a fact. It is recorded
-- and reads as unpriced (`usd_micros` NULL, never 0; the roll-up counts
-- it out loud), which is the rule the table's own comment states. An
-- FK here would refuse the honest record to keep the card tidy.
--
-- NULLABLE, NOT DEFAULTED. NULL means "this run named no model", which
-- nothing writes any more (the recorder resolves one or refuses), and
-- an old row whose actor id was not the colon form. A default here
-- would be a guess wearing a fact's clothes.

ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS model TEXT;

COMMENT ON COLUMN agent_runs.model IS
  'The model this run ran on, spelled as agent_rate_card.model spells it '
  '(opus-5[1m], no claude- prefix). A fact about the RUN, not the actor '
  '(design 6fda05ae): a registered agent runs different models over '
  'time and cost is priced per run. Resolved once at record time — the '
  'report''s own word, else the agent row''s default_model, else the '
  'model half of a legacy colon-form actor id — and written down, so no '
  'reader parses an actor id for it. No FK: a run on a model the card '
  'does not name is still recorded, and reads as unpriced.';

-- The backfill: every row so far carries `<mode>:<model>` (measured
-- 2026-09-11: all of them `claude:opus-5[1m]`). The model is everything
-- after the FIRST colon — `ActorId::from_str` splits there, and a model
-- string may itself carry colons — and `automation:` is excluded the
-- way the parse excludes it first, though no such row can exist (the
-- recorder refuses a non-agent actor).
UPDATE agent_runs
   SET model = substr(actor_id, position(':' in actor_id) + 1)
 WHERE model IS NULL
   AND position(':' in actor_id) > 0
   AND actor_id NOT LIKE 'automation:%';

-- The registry's description of the event said the actor carries the
-- model. It carries the model as its own key now.
UPDATE event_kinds
   SET description = 'An agent run was recorded: its actor, the model it ran on (a column of its own since design 6fda05ae), tokens, tool calls, duration, outcome and priced cost'
 WHERE kind_pattern = 'agents.run.recorded';
