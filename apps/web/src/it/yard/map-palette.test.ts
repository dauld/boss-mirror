import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

/** The map's palette lives in ONE place (backlog 42f66fb3).
 *
 *  Measured 2026-09-23 before this pin: the three map files carried 81
 *  colour literals, every one of them a `var(--token, #hex)` fallback.
 *  The tokens are defined at :root in styles.css, which index.html
 *  loads on every page, so no fallback was ever painted — they were a
 *  second copy of the palette, and one had already drifted (MapPage's
 *  `--static, #78716c` against the root's #7a838c). A copy like that is
 *  harmless until the redesign RENAMES a token (design dea94998, board
 *  B · Transit): then every fallback silently repaints the old dark
 *  palette, and the result looks deliberate rather than broken.
 *
 *  So the map names its colours in the Transit grammar (`--map-*`,
 *  styles.css), states no fallback, and reaches no palette token but
 *  those. A retired token then fails loudly here, and round 2's palette
 *  is an edit to the `--map-*` block rather than to these files. */
const MAP_FILES = [
  'MapPage.svelte',
  'WorldMap.svelte',
  // 'RegionMap.svelte' and 'RegionFloor.svelte' retired with the floor
  // pages (design e765b3fc, car N3).
  // The HUD frame above the map (design 00774ca8): the same palette.
  'HudFrame.svelte',
  // The motion layer over the world (design 31bade8f, car M2): its canvas
  // reads the same tokens rather than naming a colour of its own.
  'MotionLayer.svelte',
  // The transit monitor (design 16091dfb, flight it-map-transit): its
  // lines are --map-line-* tokens, its rings the map's own states.
  'TransitMap.svelte',
  // The real moves on the transit map (design e765b3fc car M2, flight
  // it-map-live): its dots and pings wear the lines' own tokens.
  'LiveMotion.svelte',
] as const;

/** The grammar round 2 swaps: grounds, text, rules, the accent, and
 *  each state as a line colour with its ink / bg / edge. */
const GRAMMAR = [
  'bg', 'surface', 'ink', 'muted', 'rule', 'rule-strong', 'accent', 'link',
  ...(['ok', 'warn', 'bad'] as const).flatMap((s) => [`${s}-ink`, `${s}-bg`, `${s}-edge`]),
  // the transit monitor's routes (design 16091dfb Q1): Design's tokens
  ...(['delivery', 'publish', 'siding', 'tenant'] as const).map((l) => `line-${l}`),
  // the full station's solid disk (design e765b3fc Q2, David 2026-09-25)
  'full',
].map((n) => `--map-${n}`);

/** Custom properties that are not colours, which a map file may still
 *  read from the global system: spacing, type, tracking, corners. */
const NOT_COLOUR = /^--(s\d|font-|ls-|radius$)/;

const here = (f: string): string => readFileSync(join(import.meta.dir, f), 'utf8');
const styles = readFileSync(join(import.meta.dir, '..', '..', 'styles.css'), 'utf8');

/** Comments say WHY in this codebase and cite `#372`-style numbers; a
 *  comment is not a colour. `//` counts only at a line start or after
 *  whitespace, so a URL's `://` survives. */
const uncommented = (src: string): string =>
  src
    .replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, ' '))
    .replace(/<!--[\s\S]*?-->/g, (m) => m.replace(/[^\n]/g, ' '))
    .replace(/(^|\s)\/\/.*$/gm, '$1');

const LITERAL = [
  /#[0-9a-fA-F]{3,8}\b/,
  /\b(rgba?|hsla?|hwb|lab|lch|oklab|oklch|color)\(/,
  /(:\s*|(fill|stroke|color)=["'])(white|black|red|green|blue|gray|grey|orange|yellow|purple|silver)\b/,
];

const offending = (src: string, test: (line: string) => boolean): string[] =>
  uncommented(src)
    .split('\n')
    .flatMap((line, i) => (test(line) ? [`${i + 1}: ${line.trim()}`] : []));

describe('the map names every colour through its own tokens', () => {
  for (const file of MAP_FILES) {
    const src = here(file);

    it(`${file} states no colour literal, not even as a var() fallback`, () => {
      expect(offending(src, (l) => LITERAL.some((re) => re.test(l)))).toEqual([]);
    });

    it(`${file} reads no palette token but --map-*`, () => {
      const local = new Set([...src.matchAll(/(--[a-z][a-z0-9-]*)\s*:/g)].map((m) => m[1]));
      const foreign = offending(src, (l) =>
        [...l.matchAll(/var\(\s*(--[a-z][a-z0-9-]*)/g)]
          .map((m) => m[1] ?? '')
          .some((t) => !t.startsWith('--map-') && !local.has(t) && !NOT_COLOUR.test(t)),
      );
      expect(foreign).toEqual([]);
    });
  }

  it('every --map-* token a map file reads is defined in styles.css', () => {
    const defined = new Set([...styles.matchAll(/(--map-[a-z0-9-]+)\s*:/g)].map((m) => m[1]));
    const read = MAP_FILES.flatMap((f) => [...here(f).matchAll(/var\(\s*(--map-[a-z0-9-]+)/g)].map((m) => m[1]));
    expect(read.length).toBeGreaterThan(0);
    expect(read.filter((t) => !defined.has(t))).toEqual([]);
  });

  // Design e765b3fc Q2, decided by David 2026-09-25: full is a SOLID disk
  // in the state green with a heavier ring, clear stays a hollow ring —
  // readable by fill alone, with no text and no hue to tell apart.
  it('TransitMap fills a full station solid with --map-full, rings it heavier than clear, and leaves clear hollow', () => {
    const rule = (sel: string): string =>
      here('TransitMap.svelte').match(new RegExp(`^\\s*${sel.replaceAll('.', '\\.')}\\s*\\{([^}]*)\\}`, 'm'))?.[1] ?? '';
    const width = (css: string): number => Number(css.match(/stroke-width:\s*([\d.]+)/)?.[1] ?? NaN);
    const full = rule('.ring.full');
    expect(full).toMatch(/fill:\s*var\(--map-full\)/);
    expect(full).toMatch(/stroke:\s*var\(--map-full\)/);
    expect(width(full)).toBeGreaterThan(width(rule('.ring.clear')));
    expect(rule('.ring.clear')).not.toMatch(/fill:/);
    expect(rule('.ring')).toMatch(/fill:\s*var\(--map-surface\)/);
  });

  it('styles.css declares the whole Transit grammar, so round 2 is one block', () => {
    const defined = new Set([...styles.matchAll(/(--map-[a-z0-9-]+)\s*:/g)].map((m) => m[1]));
    expect(GRAMMAR.filter((t) => !defined.has(t))).toEqual([]);
  });
});
