import { describe, expect, test } from 'bun:test';
import { isSim, partitionOf, PROTOCOL_PALETTE, protocolHue, redTrainsPhrase } from './packet-card';

// The packet-card vocabulary moved here from apps/web/src/it/yard/yard.ts
// when the card was promoted to web-kit (feedback d69033dd: one card
// grammar for every queue surface). yard.ts re-exports these, so the
// definition still lives exactly once (CLAUDE.md §9a).
describe('protocolHue', () => {
  test('is stable, palette-bound, and distinguishes the pipeline kinds', () => {
    expect(protocolHue('ship-a-change')).toBe(protocolHue('ship-a-change'));
    expect(PROTOCOL_PALETTE).toContain(protocolHue('ship-a-change'));
    expect(PROTOCOL_PALETTE).toContain(protocolHue('some-future-kind'));
    expect(protocolHue('ship-a-change')).not.toBe(protocolHue('pr-train'));
    expect(new Set(PROTOCOL_PALETTE).size).toBe(PROTOCOL_PALETTE.length);
  });
});

// The strike sentence lives with the card so the yard floor's dock wagon
// and the My Day card say the same words for the same count (d6e53a35).
// Zero and absent are the clean car, and read as no sentence at all.
describe('redTrainsPhrase', () => {
  test('counts and pluralises; zero or absent is silence', () => {
    expect(redTrainsPhrase(1)).toBe('1 red train behind it');
    expect(redTrainsPhrase(2)).toBe('2 red trains behind it');
    expect(redTrainsPhrase(0)).toBe('');
    expect(redTrainsPhrase(undefined)).toBe('');
  });
});

// The partition is one fact with two wire spellings (packet 508cc38c):
// the word, and the legacy `simulated` bool derived as not-real. One
// reader for both, so no lens keeps its own bool test — and `isSim`
// answers "not real", so a shadow packet closes every door a simulated
// one closes until car 4 draws it as its own.
describe('partitionOf', () => {
  test('reads the word first, then the legacy bool, then real', () => {
    expect(partitionOf({ partition: 'shadow', simulated: true })).toBe('shadow');
    expect(partitionOf({ partition: 'real', simulated: true })).toBe('real');
    expect(partitionOf({ simulated: true })).toBe('simulated');
    expect(partitionOf({ simulated: false })).toBe('real');
    expect(partitionOf({})).toBe('real');
    // An unknown word is not a claim: the derived bool decides.
    expect(partitionOf({ partition: 'nonsense', simulated: true })).toBe('simulated');
    expect(partitionOf({ partition: 7, simulated: false })).toBe('real');
  });
});

describe('isSim', () => {
  test('is "not real": shadow reads as not-real exactly as simulated does', () => {
    expect(isSim({ partition: 'shadow' })).toBe(true);
    expect(isSim({ partition: 'simulated' })).toBe(true);
    expect(isSim({ partition: 'real' })).toBe(false);
    // A `real` word does not silence the tag fallback: a packet from
    // before the column reads real there and carries the tag instead.
    expect(isSim({ partition: 'real', tags: ['sim'] })).toBe(true);
    expect(isSim({ partition: 'real', tags: ['hotfix'] })).toBe(false);
    expect(isSim({ simulated: true })).toBe(true);
    expect(isSim({ tags: ['synthetic'] })).toBe(true);
    expect(isSim({})).toBe(false);
  });
});
