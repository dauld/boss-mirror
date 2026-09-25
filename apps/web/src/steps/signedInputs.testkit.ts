// Generated inputs for the signed-value pins (backlog 6093cf13, the
// adversarial review of car 30674304). The review's point was that a
// check comparing BYTES says nothing about what a person SEES, so the
// hostile cases here are the ones a reader cannot tell apart by eye:
// bidi overrides, zero-width and control characters, look-alike letters
// from other scripts, strings that read as numbers or JSON, and
// whitespace at an edge. Test-only: nothing in the app imports it.

/** Strings a reader could mistake for another string, or for a non-string. */
export const TRICKY: readonly string[] = [
  '',
  ' ',
  '\n',
  '42',
  '-0',
  '1e3',
  'true',
  'false',
  'null',
  '"quoted"',
  '"half',
  '{"verb":"wipe"}',
  '[1,2]',
  'forge',
  'forge ',
  ' forge',
  'for​ge',
  'forge‮xcod.exe',
  '⁦isolate⁩',
  'fоrge',
  'ｆorge',
  'a\tb',
  'a\r\nb',
  'line1\nline2\n',
  'a \nb',
  'a — b',
  'a – b',
  'café',
  ' nbsp',
  'soft­hyphen',
  'bom﻿',
  'emoji \u{1F600}',
  'lone \uD800 surrogate',
  'back\\slash',
  'esc \\u{202E} literal',
  'esc \\n literal',
  'nul\u0000',
  'del\u007F',
  'c1\u0085',
  'ls ps ',
  'tag\u{E0041}',
];

/** mulberry32: a seeded generator, so a failing case names its seed. */
export function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const pick = <T>(r: () => number, xs: readonly T[]): T => xs[Math.floor(r() * xs.length)]!;

// Code points drawn from the ranges a reader cannot see or can confuse.
const POOLS: readonly (readonly [number, number])[] = [
  [0x20, 0x7e], // printable ASCII
  [0x20, 0x7e],
  [0x00, 0x1f], // C0 controls, \n and \t among them
  [0x7f, 0x9f], // DEL and C1
  [0x200b, 0x200f], // zero-width, LRM/RLM
  [0x202a, 0x202e], // bidi embeddings and overrides
  [0x2066, 0x2069], // bidi isolates
  [0x0400, 0x04ff], // Cyrillic look-alikes
  [0xff01, 0xff5e], // fullwidth ASCII
  [0x2010, 0x2015], // hyphens and dashes, em dash among them
  [0x1f600, 0x1f64f], // astral
  [0xd800, 0xdfff], // lone surrogates
];

export function genString(r: () => number): string {
  if (r() < 0.3) return pick(r, TRICKY);
  const n = Math.floor(r() * 8);
  return Array.from({ length: n }, () => {
    if (r() < 0.2) return pick(r, TRICKY);
    const [lo, hi] = pick(r, POOLS);
    return String.fromCodePoint(lo + Math.floor(r() * (hi - lo + 1)));
  }).join('');
}

export function genValue(r: () => number, depth = 0): unknown {
  const roll = r();
  if (roll < 0.45 || depth > 2) return genString(r);
  if (roll < 0.55) return pick(r, [0, 42, -1, 1.5, 1e21]);
  if (roll < 0.62) return r() < 0.5;
  if (roll < 0.67) return null;
  if (roll < 0.82) return Array.from({ length: Math.floor(r() * 3) }, () => genValue(r, depth + 1));
  return genMetadata(r, depth + 1);
}

export function genMetadata(r: () => number, depth = 0): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  const n = Math.floor(r() * 5);
  for (let i = 0; i < n; i++) out[genString(r)] = genValue(r, depth);
  return out;
}
