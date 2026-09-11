import { describe, it, expect } from 'bun:test';

import { formatActor, isHumanActor } from './actor';

describe('isHumanActor', () => {
  it('is true only for an employee id', () => {
    expect(isHumanActor('emp-032')).toBe(true);
    expect(isHumanActor('emp-bootstrap-admin')).toBe(true);
    expect(isHumanActor('emp-aa-001')).toBe(true);
  });

  it('is false for every machine CPU — automation and agent alike', () => {
    expect(isHumanActor('automation:dispatcher')).toBe(false);
    expect(isHumanActor('automation:rule:bill-approve')).toBe(false);
    expect(isHumanActor('rule:bill-approve')).toBe(false);
    // The whole point: an agent is a CPU, never staff.
    expect(isHumanActor('claude:opus-5')).toBe(false);
    expect(isHumanActor('claude:fable')).toBe(false);
  });

  // The WHOLE contract, as a table, because this predicate is the one
  // definition of the human/machine split on the client (§9a) and the
  // next person to change it should see every case at once rather than
  // one example. Every id below was READ off the system of record or
  // off `ActorId::from_str` — none is invented.
  //
  //  id                                       human?  why
  //  emp-david                                yes     employees.id, the bare human wire form
  //  emp-aa-185                               yes     a simulated brewery employee — an ordinary employee id
  //  automation:train-conductor               no      named automation
  //  automation:rule:auto-park-on-gate-green  no      a dispatch rule (slug carries further colons)
  //  automation:sim                           no      the simulator's own authority
  //  rule:bill-approve                        no      the legacy bare rule spelling
  //  claude:opus-5[1m]                        no      agent session, <mode>:<model> — agent_runs.actor_id
  //  claude@algedonic.dev                     no      agent session, address form — steps.assignee_id
  //  operator:unidentified                    no      a READ by an unnamed caller; attributes nothing
  //  system                                   no      parses as the `platform` automation
  //  '' / null / undefined                    no      Platform, never a fake human
  const CLASSIFICATION: ReadonlyArray<readonly [string | null | undefined, boolean]> = [
    ['emp-david', true],
    ['emp-032', true],
    ['emp-bootstrap-admin', true],
    ['emp-aa-185', true],
    ['automation:train-conductor', false],
    ['automation:rule:auto-park-on-gate-green', false],
    ['automation:sim', false],
    ['rule:bill-approve', false],
    ['claude:opus-5', false],
    ['claude:opus-5[1m]', false],
    ['claude@algedonic.dev', false],
    ['operator:unidentified', false],
    ['system', false],
    ['', false],
    [null, false],
    [undefined, false],
  ];

  it('classifies every spelling the system actually uses', () => {
    for (const [id, human] of CLASSIFICATION) {
      expect(isHumanActor(id)).toBe(human);
    }
  });

  it('is false for an agent addressed by its login — the address form', () => {
    // MEASURED on the system of record, 2026-09-11: `claude@algedonic.dev`
    // held 142 step assignments across the 69 open packets and signed 12
    // completions. It is the id the pod's `BOSS_ACTOR` carries, so every
    // `boss` verb run by the agent session signs with it.
    //
    // It carries no colon, so the colon test called it a PERSON — and the
    // department's busiest CPU rendered as staff on every surface that
    // asks this question. An address is the shape of a LOGIN, and BOSS
    // resolves a human login to an employee id before any write is signed
    // (`boss-gateway/src/oidc.rs`: the IdP's email → `bootstrap_email` →
    // `session.employee_id = emp-*`). So an actor id that is STILL an
    // address at the point a step records it was never resolved to a
    // person — it is a session identity, which here is an agent's.
    expect(isHumanActor('claude@algedonic.dev')).toBe(false);
  });

  it('is false for any address-shaped session id, not just this one', () => {
    expect(isHumanActor('fable@algedonic.dev')).toBe(false);
    expect(isHumanActor('someone@example.com')).toBe(false);
  });

  it('is false for an absent actor — Platform is not a person', () => {
    expect(isHumanActor(null)).toBe(false);
    expect(isHumanActor(undefined)).toBe(false);
    expect(isHumanActor('')).toBe(false);
  });
});

describe('formatActor', () => {
  it('renders a dispatch rule readably', () => {
    expect(formatActor('automation:rule:bill-approve')).toBe('Rule · bill-approve');
  });

  it('maps known automations to friendly names', () => {
    expect(formatActor('automation:dispatcher')).toBe('Dispatcher');
    expect(formatActor('automation:sim')).toBe('Simulator');
    expect(formatActor('automation:platform')).toBe('Platform');
  });

  it('title-cases a service automation slug', () => {
    expect(formatActor('automation:account-provisioning')).toBe('Account Provisioning');
  });

  it('resolves a human via empNames, else shows the bare id', () => {
    expect(formatActor('emp-032', new Map([['emp-032', 'Dana Ng']]))).toBe('Dana Ng');
    expect(formatActor('emp-099')).toBe('emp-099');
  });

  it('renders an agent as its mode and model, never as a missing employee', () => {
    expect(formatActor('claude:opus-5')).toBe('Claude · opus-5');
    expect(formatActor('claude:fable')).toBe('Claude · fable');
    // The model half stays whole — dots and further colons are the
    // vendor's model string, not structure we get to reinterpret.
    expect(formatActor('claude:claude-opus-5.1')).toBe('Claude · claude-opus-5.1');
    expect(formatActor('claude:opus-5:1m')).toBe('Claude · opus-5:1m');
  });

  it('never mistakes an agent id for an employee lookup', () => {
    // An empNames map that happens to be present must not turn an
    // agent into "unknown employee claude:fable".
    const names = new Map([['emp-032', 'Dana Ng']]);
    expect(formatActor('claude:fable', names)).toBe('Claude · fable');
  });

  it('keeps `automation:` winning over the agent split', () => {
    // Same branch order as ActorId::from_str: a dispatch rule is not
    // an `automation`-mode agent.
    expect(formatActor('automation:rule:bill-approve')).toBe('Rule · bill-approve');
  });

  it('leaves an address-form agent id verbatim', () => {
    // The label side deliberately does NOT change with the predicate.
    // An address is already self-describing and is what operators read
    // on the incidents strip (`tests/mocked/incidents.mocked.spec.ts`
    // asserts it visible verbatim). `empNames` never holds an address
    // key, so the lookup below is a miss and the id falls through whole.
    expect(formatActor('claude@algedonic.dev')).toBe('claude@algedonic.dev');
    expect(formatActor('claude@algedonic.dev', new Map([['emp-032', 'Dana Ng']]))).toBe(
      'claude@algedonic.dev',
    );
  });

  it('reads a legacy null/empty actor as the platform automation', () => {
    expect(formatActor(null)).toBe('Platform');
    expect(formatActor(undefined)).toBe('Platform');
    expect(formatActor('')).toBe('Platform');
  });
});
