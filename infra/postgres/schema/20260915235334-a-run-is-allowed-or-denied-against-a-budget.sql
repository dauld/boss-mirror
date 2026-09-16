-- 20260915235334-a-run-is-allowed-or-denied-against-a-budget.sql — a
-- run is admitted against its actor's budget, and the decision is a
-- recorded fact either way.
--
-- Origin: backlog 7dd9f28c ("Actors and their budgets are types with
-- no registry: AgentSpec and BudgetDecision are unwired"), car 3 of
-- design 6fda05ae. Car 1 (20260915212644) gave the agent one id and a
-- row carrying its caps, NULL, "the honest value while nothing reads
-- them"; car 2 (20260915223855) put the model on the run so a run is
-- priced against what it actually ran. This car is the reader: the
-- jobs-API recorder now measures an actor's priced spend in the hour
-- before a run STARTED, plus its runs in flight at that instant,
-- against the caps on its row, and writes down what it decided.
--
-- WHY THE DECISION IS A VALUE, WRITTEN DOWN. On 2026-09-08 a session
-- ran out of credit and the only signal was the work stopping. A
-- refusal that is a row nobody can read is that failure again in a
-- different place. So an ALLOW rides the run (this column: what was
-- left at admission), and a DENY is its own event (the kind below:
-- which actor, against which cap, in which window, and what the
-- refused run itself cost) with no row written — the refusal is as
-- visible as the spend, and the money stays on the log (conservation)
-- even though the projection does not hold it.
--
-- NULLABLE, NOT DEFAULTED. NULL on this column is "no decision was
-- made" — every row recorded before this file — and it is kept
-- distinct from an allow with no cap, which is written as
-- {"kind":"allow","remaining_usd_micros":null}. A default of "allow"
-- would credit the old rows with a judgement nobody made.
--
-- NULL CAPS STAY "UNBUDGETED". The one live row (agent-claude) keeps
-- both caps NULL and is admitted with nothing to count down; setting a
-- cap is a data change on that row, not a deploy, which is what the
-- packet asked for. A missing number must not stop the stack.

ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS budget JSONB;

COMMENT ON COLUMN agent_runs.budget IS
  'The admission decision (backlog 7dd9f28c): the actor''s registry caps '
  'judged against its priced spend in the hour before this run started '
  'and its runs in flight at that instant. Always {"kind":"allow", '
  '"remaining_usd_micros":N|null} on a row — a refused run is not a row, '
  'it is an agents.run.denied event. NULL only on a row recorded before '
  'budgets were consulted: no decision was made, which is not "allowed".';

-- The refusal. Declared here so it does not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('agents.run.denied', 'jobs', 'An agent run was refused against its actor''s budget: the run as reported and priced, the actor, the window and its cutoff, the spend and runs in flight measured at the run''s start, the caps on the agents row, and the reason. No agent_runs row is written for a refused run; this event is the record that it was asked for', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;

UPDATE event_kinds
   SET description = 'An agent run was recorded: its actor, the model it ran on (a column of its own since design 6fda05ae), tokens, tool calls, duration, outcome, priced cost, and the budget decision it was admitted under (backlog 7dd9f28c)'
 WHERE kind_pattern = 'agents.run.recorded';
