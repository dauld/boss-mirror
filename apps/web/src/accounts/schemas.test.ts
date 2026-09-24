import { describe, expect, test } from 'bun:test';
import { AccountSchema, RiskScoreListSchema } from './schemas';

// The crash this schema exists to stop: WatchlistPage cast the payload
// and read `.length` off the result, so a response without `accounts`
// made `scores` undefined and the page died with
// "Cannot read properties of undefined (reading 'length')"
// under the route-smoke suite's adversarial mock (feedback 2fe1c8c1).

const ok = {
  accounts: [
    {
      account_id: 'acct-1',
      account_name: 'Algedonic Ales',
      score: 72,
      top_factor: 'days_since_last_invoice',
      factors: { days_since_last_invoice: 40, open_ticket_count: 2 },
    },
  ],
};

describe('RiskScoreListSchema', () => {
  test('accepts a well-formed payload', () => {
    const r = RiskScoreListSchema.safeParse(ok);
    expect(r.success).toBe(true);
  });

  test('REFUSES a payload with no accounts key — the crash', () => {
    expect(RiskScoreListSchema.safeParse({}).success).toBe(false);
    expect(RiskScoreListSchema.safeParse({ data: [] }).success).toBe(false);
  });

  test('refuses accounts that is not an array', () => {
    expect(RiskScoreListSchema.safeParse({ accounts: null }).success).toBe(false);
    expect(RiskScoreListSchema.safeParse({ accounts: 'nope' }).success).toBe(false);
  });

  /** A score the table sorts on must be a NUMBER; a string sorts wrong
   *  and silently, which is worse than refusing. */
  test('refuses a non-numeric score', () => {
    const bad = { accounts: [{ ...ok.accounts[0], score: 'high' }] };
    expect(RiskScoreListSchema.safeParse(bad).success).toBe(false);
  });

  /** `factors` is deliberately permissive: the page reads a few keys and
   *  tolerates absent ones, so a backend adding a field must not become
   *  a hard parse failure. */
  test('tolerates an absent or extended factors bag', () => {
    const noFactors = { accounts: [{ ...ok.accounts[0], factors: undefined }] };
    expect(RiskScoreListSchema.safeParse(noFactors).success).toBe(true);
    const extra = {
      accounts: [{ ...ok.accounts[0], factors: { brand_new_signal: 1 } }],
    };
    expect(RiskScoreListSchema.safeParse(extra).success).toBe(true);
  });

  test('an empty account list is valid — zero at-risk accounts is a real answer', () => {
    expect(RiskScoreListSchema.safeParse({ accounts: [] }).success).toBe(true);
  });
});

// Backlog d2c9e79f: the schema pinned `tier` to platinum/gold/silver,
// citing a DB CHECK that 22-accounts.sql does not have — `tier` is
// plain TEXT, and a tier is an (account, tier) Class row a tenant adds
// without a deploy. The account detail validates with this schema, so
// an account on a tenant-added tier refused to load.
describe('AccountSchema tier', () => {
  const account = (tier: unknown) => ({
    id: 'acct-1', name: 'One', director: null, city: null, state: null,
    tier, customer_since: null, territory_rep_id: null,
  });

  test('accepts a tier the seeded trio does not name — the registry decides, not the schema', () => {
    expect(AccountSchema.safeParse(account('bronze')).success).toBe(true);
  });

  test('accepts an untiered account and the seeded tiers', () => {
    for (const t of [null, 'platinum', 'gold', 'silver']) {
      expect(AccountSchema.safeParse(account(t)).success).toBe(true);
    }
  });

  test('still refuses a tier that is not a string', () => {
    expect(AccountSchema.safeParse(account(3)).success).toBe(false);
  });
});
