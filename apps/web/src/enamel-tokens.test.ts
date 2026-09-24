import { describe, expect, it } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

/** The look is Enamel, and its values live in ONE place (design a4df741a,
 *  David 2026-09-24: "I like Enamel. Let's start there." and "Let's do
 *  comfortable rows"; backlog e4b58c50).
 *
 *  The values below are the fold's (docs/architecture-decisions.md,
 *  "Its parts are Enamel") and the round-2 board's `.t-enamel` block,
 *  which the fold wrote down so they would not live only behind a link.
 *  This pins three things a re-skin can quietly get wrong:
 *
 *  1. the tokens ARE the decided values, under the names components
 *     already read (--void, --ink, --fog, --signal ...), so the change is
 *     a re-skin and not a rename every call site has to follow;
 *  2. every word the palette is meant to carry is legible — measured
 *     here from the tokens, because a light theme inverts every pair the
 *     dark one had tuned, and busy's plate (#F2C230) is 1.6:1 as text on
 *     white, so it cannot also be the warn TEXT colour;
 *  3. styles.css states a colour only inside a :root token block, so the
 *     dark palette cannot survive as a literal somewhere below and paint
 *     a night-black scrim or a mint hover onto the new ground. */

const styles = readFileSync(join(import.meta.dir, 'styles.css'), 'utf8');

const uncommented = styles.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, ' '));

const ROOT_BLOCK = /:root\s*\{[^}]*\}/g;

/** Every custom property declared at :root, last declaration wins — the
 *  cascade's own rule for one selector repeated. */
const tokens: ReadonlyMap<string, string> = new Map(
  [...uncommented.matchAll(ROOT_BLOCK)].flatMap((block) =>
    [...block[0].matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)].map(
      (m) => [m[1] ?? '', (m[2] ?? '').trim()] as const,
    ),
  ),
);

/** A token's value with every var() followed to its definition. */
const resolve = (name: string, seen: readonly string[] = []): string => {
  const raw = tokens.get(name);
  if (raw === undefined) throw new Error(`${name} is not defined at :root`);
  if (seen.includes(name)) throw new Error(`var() cycle: ${[...seen, name].join(' -> ')}`);
  return raw.replace(/var\(\s*(--[a-z0-9-]+)\s*(?:,[^)]*)?\)/g, (_, ref: string) =>
    resolve(ref, [...seen, name]),
  );
};

type Rgba = readonly [number, number, number, number];

const parse = (colour: string): Rgba => {
  const hex = colour.match(/^#([0-9a-f]{6})$/i)?.[1];
  if (hex) {
    const n = Number.parseInt(hex, 16);
    return [(n >> 16) & 255, (n >> 8) & 255, n & 255, 1];
  }
  const fn = colour.match(/^rgba?\(([^)]*)\)$/)?.[1];
  if (fn) {
    const [r = 0, g = 0, b = 0, a = 1] = fn.split(',').map((s) => Number(s.trim()));
    return [r, g, b, a];
  }
  if (colour === 'transparent') return [0, 0, 0, 0];
  throw new Error(`not a colour this test reads: ${colour}`);
};

/** Paint `top` over an opaque `under`. */
const over = (top: Rgba, under: Rgba): Rgba => {
  const a = top[3];
  return [0, 1, 2].map((i) => (top[i] ?? 0) * a + (under[i] ?? 0) * (1 - a)).concat(1) as unknown as Rgba;
};

const luminance = ([r, g, b]: Rgba): number => {
  const lin = (v: number) => {
    const s = v / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
};

const contrast = (a: Rgba, b: Rgba): number => {
  const [lo, hi] = [luminance(a), luminance(b)].sort((x, y) => x - y);
  return ((hi ?? 0) + 0.05) / ((lo ?? 0) + 0.05);
};

/** WCAG AA for body text — the floor the mocked suite's _contrast.ts uses. */
const AA = 4.5;

describe('the Enamel tokens are the decided values', () => {
  // [token, the fold's / the board's value]
  const DECIDED: ReadonlyArray<readonly [string, string]> = [
    ['--void', '#FFFFFF'], // white ground
    ['--ink', '#FFFFFF'], // surfaces sit on it, set apart by rules, not shade
    ['--ink-raised', '#F1F3F6'], // the board's surface-2: nested rows, hover
    ['--fog', '#0E1B2E'], // night ink
    ['--static', '#4F5D72'], // the board's muted
    ['--hairline', '#DCE2EA'], // the board's rule
    ['--border-strong', '#0E1B2E'], // rule-strong: frames are ink
    ['--signal', '#0F6E9F'], // action and progress
    ['--focus', '#D9A400'], // the one amber outside a state
    ['--band', '#0E1B2E'], // enamel bands: headers, table heads
    ['--on-band', '#FFFFFF'],
    ['--clear', '#0B6B4F'],
    ['--busy', '#F2C230'],
    ['--troubled', '#C8283D'],
    ['--on-clear', '#FFFFFF'],
    ['--on-busy', '#0E1B2E'], // busy's plate is read under ink
    ['--on-troubled', '#FFFFFF'],
    ['--radius', '4px'],
    ['--radius-field', '3px'],
    ['--frame', '2px'],
    ['--row-pad-y', '12px'], // comfortable, everywhere (no compact exception)
  ];

  for (const [name, value] of DECIDED) {
    it(`${name} is ${value}`, () => {
      expect(resolve(name).toUpperCase()).toBe(value.toUpperCase());
    });
  }

  it('keeps the spacing scale as it was: --s1..--s7, and 72 for gutters', () => {
    const scale = ['--s1', '--s2', '--s3', '--s4', '--s5', '--s6', '--s7', '--s8'].map((n) =>
      resolve(n),
    );
    expect(scale).toEqual(['4px', '8px', '12px', '16px', '24px', '32px', '48px', '72px']);
  });
});

describe('the faces are Overpass and Overpass Mono, self-hosted', () => {
  it('names Overpass for voice and Overpass Mono for instrument', () => {
    expect(resolve('--font-body').split(',')[0]?.trim()).toBe("'Overpass'");
    expect(resolve('--font-mono').split(',')[0]?.trim()).toBe("'Overpass Mono'");
  });

  const faces = [...uncommented.matchAll(/@font-face\s*\{([^}]*)\}/g)].map((m) => m[1] ?? '');

  it('declares no face but those two', () => {
    const families = new Set(
      faces.map((f) => f.match(/font-family:\s*'([^']+)'/)?.[1] ?? '(unnamed)'),
    );
    expect([...families].sort()).toEqual(['Overpass', 'Overpass Mono']);
  });

  it('serves every face from our own origin — Google subsets, no CDN on page load', () => {
    const srcs = faces.map((f) => f.match(/url\('([^']+)'\)/)?.[1] ?? '(no url)');
    expect(srcs.length).toBeGreaterThan(0);
    for (const src of srcs) {
      expect(src).toStartWith('./assets/fonts/');
      expect(existsSync(join(import.meta.dir, src))).toBe(true);
    }
    expect(uncommented).not.toMatch(/@import|https?:\/\//);
  });
});

describe('every word the palette carries is legible (AA, 4.5:1)', () => {
  const colour = (name: string): Rgba => parse(resolve(name));
  const on = (fg: string, bg: string, under?: string): number => {
    const ground = under ? over(colour(bg), colour(under)) : colour(bg);
    return contrast(over(colour(fg), ground), ground);
  };

  // [text, surface, the opaque ground a translucent surface sits on]
  const PAIRS: ReadonlyArray<readonly [string, string, string?]> = [
    ['--fog', '--void'],
    ['--fog', '--ink-raised'],
    ['--static', '--void'],
    ['--static', '--ink-raised'],
    ['--text-faint', '--void'],
    ['--signal', '--void'],
    ['--ok', '--void'],
    ['--warn', '--void'],
    ['--err', '--void'],
    ['--err', '--ink-raised'],
    // a primary button, a nav badge: white on the action blue
    ['--void', '--signal'],
    ['--on-band', '--band'],
    // the plates always carry their word
    ['--on-clear', '--clear'],
    ['--on-busy', '--busy'],
    ['--on-troubled', '--troubled'],
    // a chip: its text on its own wash, on a card
    ['--text', '--wash', '--ink'],
    ['--warn', '--wash', '--ink'],
    // a state's words on its own tint
    ['--ok', '--ok-wash', '--void'],
    ['--warn', '--warn-wash', '--void'],
    ['--err', '--err-wash', '--void'],
    ['--fog', '--signal-wash', '--void'],
    // the map reads the same system through its own names
    ['--map-ink', '--map-bg'],
    ['--map-muted', '--map-bg'],
    ['--map-ok-ink', '--map-bg'],
    ['--map-warn-ink', '--map-bg'],
    ['--map-bad-ink', '--map-bg'],
  ];

  for (const [fg, bg, under] of PAIRS) {
    it(`${fg} on ${bg}${under ? ` over ${under}` : ''}`, () => {
      expect(on(fg, bg, under)).toBeGreaterThanOrEqual(AA);
    });
  }
});

describe('styles.css states a colour only in its token blocks', () => {
  it('no hex or rgb() literal outside :root', () => {
    const LITERAL = /#[0-9a-fA-F]{3,8}\b|\brgba?\(|\bhsla?\(|\bcolor-mix\(/;
    const outside = uncommented.replace(ROOT_BLOCK, (m) => m.replace(/[^\n]/g, ' '));
    const hits = outside
      .split('\n')
      .flatMap((line, i) => (LITERAL.test(line) ? [`${i + 1}: ${line.trim()}`] : []));
    expect(hits).toEqual([]);
  });
});
