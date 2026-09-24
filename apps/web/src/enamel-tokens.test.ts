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
    ['--halo', 'rgba(217, 164, 0, 0.35)'], // a focused field's amber halo
    ['--troubled-ink', '#9B1C2C'], // troubled as words: a field's error line
    ['--band', '#0E1B2E'], // enamel bands: headers, table heads
    ['--on-band', '#FFFFFF'],
    ['--clear', '#0B6B4F'],
    ['--busy', '#F2C230'],
    ['--troubled', '#C8283D'],
    ['--on-clear', '#FFFFFF'],
    ['--on-busy', '#0E1B2E'], // busy's plate is read under ink
    ['--on-troubled', '#FFFFFF'],
    // the step lifecycle's plates: ready solid blue, completed solid ink
    // (pending is a dashed frame, so it has no ground of its own)
    ['--ready', '#0F6E9F'],
    ['--on-ready', '#FFFFFF'],
    ['--completed', '#0E1B2E'],
    ['--on-completed', '#FFFFFF'],
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
    ['--troubled-ink', '--void'],
    // a primary button, a nav badge: white on the action blue
    ['--void', '--signal'],
    ['--on-band', '--band'],
    // the chrome bar's quieter words — tabs, the time label — on the band,
    // and on a tab's hover ground
    ['--on-band-dim', '--band'],
    ['--on-band-dim', '--band-raised', '--band'],
    ['--on-band', '--band-raised', '--band'],
    // the plates always carry their word
    ['--on-clear', '--clear'],
    ['--on-busy', '--busy'],
    ['--on-troubled', '--troubled'],
    ['--on-ready', '--ready'],
    ['--on-completed', '--completed'],
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

/** Every rule outside :root, as selector -> declarations. A selector list
 *  is split, so `.plate-ready, .step-status-ready { ... }` answers for both.
 *  Rules nested in @media are read too; nothing below cares which. */
const rules: ReadonlyMap<string, ReadonlyMap<string, string>> = (() => {
  const outside = uncommented.replace(ROOT_BLOCK, (m) => m.replace(/[^\n]/g, ' '));
  const map = new Map<string, Map<string, string>>();
  for (const m of outside.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const decls = new Map(
      [...(m[2] ?? '').matchAll(/([a-z-]+)\s*:\s*([^;]+);?/g)].map(
        (d) => [d[1] ?? '', (d[2] ?? '').trim()] as const,
      ),
    );
    for (const sel of (m[1] ?? '').split(',').map((s) => s.trim())) {
      map.set(sel, new Map([...(map.get(sel) ?? []), ...decls]));
    }
  }
  return map;
})();

const decl = (selector: string, property: string): string | undefined =>
  rules.get(selector)?.get(property);

/** States are solid PLATES that always carry their word (the fold's
 *  "Its parts are Enamel", backlog 7eb59678 car 2). Before this car every
 *  state pill in the shared chrome was a word tinted on a 4% wash — the
 *  "tinted pills" the round-2 board drew as Transit and David did not
 *  choose. */
describe('states are solid plates that carry their word', () => {
  // [the rule, its ground token, the token its word is set in]. Completed
  // is a step's state only, so only .step-status-completed wears it.
  const SOLID: ReadonlyArray<readonly [string, string, string]> = [
    ['.plate-clear', '--clear', '--on-clear'],
    ['.plate-busy', '--busy', '--on-busy'],
    ['.plate-troubled', '--troubled', '--on-troubled'],
    ['.plate-ready', '--ready', '--on-ready'],
    ['.step-status-completed', '--completed', '--on-completed'],
  ];

  for (const [rule, ground, word] of SOLID) {
    it(`${rule} is ${ground} under ${word}`, () => {
      expect(decl(rule, 'background')).toBe(`var(${ground})`);
      expect(decl(rule, 'color')).toBe(`var(${word})`);
    });
  }

  it('pending is a dashed frame with no ground — the plate not yet filled', () => {
    expect(decl('.step-status-pending', 'background')).toBe('transparent');
    expect(decl('.step-status-pending', 'border-style')).toBe('dashed');
  });

  it('skipped is that frame gone quiet and struck — a plate never to be filled', () => {
    expect(decl('.step-status-skipped', 'background')).toBe('transparent');
    expect(decl('.step-status-skipped', 'border-style')).toBe('dashed');
    expect(decl('.step-status-skipped', 'text-decoration')).toBe('line-through');
  });

  it('every plate names its word in caps, the way a sign does', () => {
    expect(decl('.plate', 'text-transform')).toBe('uppercase');
    expect(decl('.plate', 'white-space')).toBe('nowrap');
  });

  // A step's status IS a state, so its span is a plate: one rule each, not
  // a second palette. [the StepStatus word, the rule it shares]
  const STEP: ReadonlyArray<readonly [string, string]> = [
    ['ready', '.plate-ready'],
    ['active', '.plate-busy'],
    // the legacy three-class set still renders on unmigrated rows
    ['done', '.step-status-completed'],
    ['waived', '.step-status-skipped'],
  ];

  it('.step-status is a plate', () => {
    expect(rules.get('.step-status')).toEqual(rules.get('.plate'));
  });
  for (const [status, rule] of STEP) {
    it(`a ${status} step wears ${rule}`, () => {
      expect(rules.get(rule)).toBeDefined();
      expect(rules.get(`.step-status-${status}`)).toEqual(rules.get(rule));
    });
  }

  it('every plate a component names is defined here, and no tinted tone survives', () => {
    const glob = new Bun.Glob('**/*.svelte');
    const roots = [import.meta.dir, join(import.meta.dir, '../../../libs/web-kit/src')];
    const used = new Set<string>();
    const tinted: string[] = [];
    for (const root of roots) {
      for (const file of glob.scanSync(root)) {
        const src = readFileSync(join(root, file), 'utf8');
        // `plate plate-x` in a class list, or a `class:plate-x` directive —
        // not the map's own `plate-tag`, a platform's name on its sign.
        for (const m of src.matchAll(/(?:\bplate |class:)plate-([a-z]+)\b/g)) used.add(m[1] ?? '');
        if (/\bchip-tone-/.test(src)) tinted.push(file);
      }
    }
    expect(used.size).toBeGreaterThan(0);
    expect([...used].filter((p) => !rules.has(`.plate-${p}`))).toEqual([]);
    expect(tinted).toEqual([]);
  });
});

describe('the chrome bar is an enamel band', () => {
  const bar = readFileSync(
    join(import.meta.dir, '../../../libs/web-kit/src/PerspectiveTabs.svelte'),
    'utf8',
  );
  const block = (selector: string): string =>
    bar.match(new RegExp(`\\n\\s*${selector.replace(/\./g, '\\.')}\\s*\\{([^}]*)\\}`))?.[1] ?? '';

  it('is night ink under white words', () => {
    expect(block('.perspective-tabs')).toMatch(/background:\s*var\(--band\);/);
    expect(block('.perspective-tabs')).toMatch(/color:\s*var\(--on-band\);/);
  });
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

/** The page header is Enamel's title (backlog 6f471ff6, car 1). Until this
 *  car PageHeader — on 76 pages — still wore Design Language v1.0: a 32px
 *  mixed-case title in malt, a diamond eyebrow and an amber rule, with hop,
 *  glass, tap and barrel glyphs on four hubs. The round-2 board sets the
 *  page title in Overpass 900, uppercase, tracked .01em, 26px, and amber is
 *  focus and nothing else (the fold: "the one amber outside a state"). */
describe('the page header is Enamel’s title, not the brewery’s', () => {
  it('sets the title in Overpass 900, uppercase, 26px, tracked .01em, in night ink', () => {
    expect(decl('.exec-title', 'font-family')).toBe('var(--font-body)');
    expect(decl('.exec-title', 'font-weight')).toBe('900');
    expect(decl('.exec-title', 'text-transform')).toBe('uppercase');
    expect(decl('.exec-title', 'font-size')).toBe('26px');
    expect(decl('.exec-title', 'letter-spacing')).toBe('0.01em');
    expect(decl('.exec-title', 'color')).toBe('var(--fog)');
  });

  it('labels the page with the board’s small label, not a diamond ribbon', () => {
    expect(decl('.exec-eyebrow', 'font')).toBe('400 11px var(--font-mono)');
    expect(decl('.exec-eyebrow', 'letter-spacing')).toBe('var(--ls-label)');
    expect(decl('.exec-eyebrow', 'color')).toBe('var(--static)');
    expect(rules.has('.exec-eyebrow::before')).toBe(false);
  });

  it('draws no amber rule under the header', () => {
    expect(decl('.exec-header', 'border-bottom')).toBeUndefined();
  });

  it('carries no brewery motif and reads no brewery token', () => {
    const header = readFileSync(
      join(import.meta.dir, '../../../libs/web-kit/src/ui/PageHeader.svelte'),
      'utf8',
    );
    expect(header).not.toMatch(/motif|--brew-/);
  });
});

/** Enamel's button (backlog 6f471ff6, car 1): the board's `.btn` is 13px,
 *  weight 800, uppercase, tracked .06em, padding 11px 18px, line-height 1,
 *  in a 2px frame — primary filled with the action blue, secondary white in
 *  an ink frame, danger white with troubled words in a troubled frame,
 *  disabled at .45, small at 12.5px with 7px 12px padding. Before this car
 *  `.btn` was 12px/700 at 6px 12px, and only 14 class lists wore it while
 *  `.step-btn` (34, a 1px hairline in mixed case) and `.wb-btn` (31,
 *  frameless) drew the rest. */
describe('the button is Enamel’s', () => {
  it('is 13px, 800, uppercase, tracked, in a 2px ink frame', () => {
    expect(decl('.btn', 'font-size')).toBe('13px');
    expect(decl('.btn', 'font-weight')).toBe('800');
    expect(decl('.btn', 'text-transform')).toBe('uppercase');
    expect(decl('.btn', 'letter-spacing')).toBe('var(--ls-button)');
    expect(decl('.btn', 'padding')).toBe('11px 18px');
    expect(decl('.btn', 'line-height')).toBe('1');
    expect(decl('.btn', 'border')).toBe('var(--frame) solid var(--border-strong)');
    expect(decl('.btn', 'border-radius')).toBe('var(--radius)');
    expect(decl('.btn', 'color')).toBe('var(--text)');
  });

  it('fills the primary with the action blue', () => {
    expect(decl('.btn-primary', 'background')).toBe('var(--accent)');
    expect(decl('.btn-primary', 'border-color')).toBe('var(--accent)');
  });

  it('draws danger as troubled words in a troubled frame', () => {
    expect(resolve('--err')).toBe('#C8283D'); // the troubled plate's red
    expect(decl('.btn-danger-outline', 'color')).toBe('var(--err)');
    expect(decl('.btn-danger-outline', 'border-color')).toBe('var(--err)');
  });

  it('dims disabled to .45 and sets small at 12.5px', () => {
    expect(decl('.btn:disabled', 'opacity')).toBe('0.45');
    expect(decl('.btn-sm', 'font-size')).toBe('12.5px');
    expect(decl('.btn-sm', 'padding')).toBe('7px 12px');
  });

  // The step plugins (infra/step-plugins/*.js) are bundles served beside
  // the app and still name the old classes, so each old name is the SAME
  // rule as the Enamel one it became — never a second look.
  const ALIAS: ReadonlyArray<readonly [string, string]> = [
    ['.step-btn', '.btn'],
    ['.step-btn-primary', '.btn-primary'],
    ['.step-btn-approve', '.btn-primary'],
    ['.step-btn-reject', '.btn-danger-outline'],
    ['.wb-btn', '.btn'],
  ];
  for (const [old, now] of ALIAS) {
    it(`${old} is ${now}`, () => {
      expect(rules.get(now)).toBeDefined();
      expect(rules.get(old)).toEqual(rules.get(now));
    });
  }

  it('no component outside the IT department names a legacy button class', () => {
    // The IT surfaces are left to the IT map cars (c3105b2a) that are
    // rebuilding them; every other page names the Enamel class itself.
    const LEFT = [
      'it/design/DesignReviewPage.svelte',
      'it/step-plugins/StepPluginDetailPage.svelte',
      'it/yard/FloorDeck.svelte',
    ];
    const glob = new Bun.Glob('**/*.svelte');
    const roots = [import.meta.dir, join(import.meta.dir, '../../../libs/web-kit/src')];
    const naming = roots.flatMap((root) =>
      [...glob.scanSync(root)].filter((file) =>
        /\b(?:step|wb)-btn\b/.test(readFileSync(join(root, file), 'utf8')),
      ),
    );
    expect(naming.sort()).toEqual(LEFT);
  });
});

/** Every .svelte file under apps/web/src and web-kit, as [path, source]. */
const svelteFiles: ReadonlyArray<readonly [string, string]> = (() => {
  const glob = new Bun.Glob('**/*.svelte');
  const roots: ReadonlyArray<readonly [string, string]> = [
    ['', import.meta.dir],
    ['web-kit/', join(import.meta.dir, '../../../libs/web-kit/src')],
  ];
  return roots.flatMap(([prefix, root]) =>
    [...glob.scanSync(root)].map(
      (file) => [`${prefix}${file}`, readFileSync(join(root, file), 'utf8')] as const,
    ),
  );
})();

/** A text-entry control, with zero specificity: every input a person types
 *  into, and no box that is ticked, slid, picked or pressed. */
const TEXT_ENTRY =
  ':where(input:not([type="checkbox"]):not([type="radio"]):not([type="range"]):not([type="color"]):not([type="file"]):not([type="hidden"]):not([type="button"]):not([type="submit"]):not([type="reset"]):not([type="image"]))';

/** A selector that styles a field: an input, a select or a textarea, by
 *  element or by a class named for one. Ticks and radios are not fields. */
const FIELD_SELECTOR = /(?:^|[\s>+~(])(?:input|select|textarea)\b|-(?:input|select|textarea|search)\b/;
const isFieldSelector = (sel: string): boolean =>
  FIELD_SELECTOR.test(sel) && !/checkbox|radio/.test(sel);

/** Enamel's field (backlog 6f471ff6, car 2). The round-2 board's `.field`
 *  under `.t-enamel`: 14px words, padding 9px 12px, in a 2px night-ink frame
 *  squared to 3px; focus turns the frame the action blue inside a 3px amber
 *  halo; an invalid field's frame turns troubled, and its error line is
 *  12.5px, 600, in troubled ink. Before this car there was no shared field
 *  rule: 42 rules drew fields in 1px hairlines, five of them with a blue
 *  focus outline, and aria-invalid appeared nowhere among 157 fields. */
describe('the field is Enamel’s', () => {
  it('frames every text-entry control the page does not style itself', () => {
    expect(decl(TEXT_ENTRY, 'border')).toBe('var(--frame) solid var(--border-strong)');
    expect(decl(TEXT_ENTRY, 'border-radius')).toBe('var(--radius-field)');
    expect(decl(TEXT_ENTRY, 'padding')).toBe('9px 12px');
    expect(decl(TEXT_ENTRY, 'font-size')).toBe('14px');
    expect(decl(TEXT_ENTRY, 'font-family')).toBe('inherit');
    expect(decl(TEXT_ENTRY, 'color')).toBe('var(--text)');
    expect(decl(TEXT_ENTRY, 'background')).toBe('var(--card)');
  });

  it('is the same rule for a select and a textarea', () => {
    expect(rules.get(':where(select)')).toEqual(rules.get(TEXT_ENTRY));
    expect(rules.get(':where(textarea)')).toEqual(rules.get(TEXT_ENTRY));
  });

  it('focuses in the action blue inside an amber halo, not a blue outline', () => {
    for (const sel of [TEXT_ENTRY, ':where(select)', ':where(textarea)']) {
      expect(decl(`${sel}:focus`, 'outline')).toBe('none');
      expect(decl(`${sel}:focus`, 'border-color')).toBe('var(--accent)');
      expect(decl(`${sel}:focus`, 'box-shadow')).toBe('0 0 0 3px var(--halo)');
    }
  });

  it('reads invalid off aria-invalid, so the look and the announcement cannot disagree', () => {
    for (const sel of [TEXT_ENTRY, ':where(select)', ':where(textarea)']) {
      expect(decl(`${sel}[aria-invalid="true"]`, 'border-color')).toBe('var(--troubled)');
    }
    expect([...rules.keys()].filter((s) => /\.invalid\b/.test(s))).toEqual([]);
  });

  it('sets the error line in troubled ink, 12.5px, 600', () => {
    expect(decl('.field-error', 'color')).toBe('var(--troubled-ink)');
    expect(decl('.field-error', 'font-size')).toBe('12.5px');
    expect(decl('.field-error', 'font-weight')).toBe('600');
  });

  it('labels a step’s field in 13.5px, 700, night ink', () => {
    expect(decl('.step-field label', 'font-size')).toBe('13.5px');
    expect(decl('.step-field label', 'font-weight')).toBe('700');
    expect(decl('.step-field label', 'color')).toBe('var(--text)');
  });

  it('no shared rule in styles.css draws a field in its own frame or outline', () => {
    // Printing strips a date field to its words; that is not a look.
    const LEFT = ['body.boss-printing .finance-print-area input[type="date"]'];
    const drawing = [...rules]
      .filter(([sel]) => isFieldSelector(sel) && !sel.startsWith(':where('))
      .filter(([, d]) => ['border', 'border-radius', 'outline'].some((p) => d.has(p)))
      .map(([sel]) => sel);
    expect(drawing).toEqual(LEFT);
  });

  it('the components still drawing their own field frame are only these', () => {
    // A ratchet, not a destination: each is a page's scoped rule, left for
    // the next car so this one migrates the shared field and stays small.
    // Two web-kit controls keep their own: GlobalSearch is a white field let
    // into the band (7eb59678 car 2), and FeedbackControl also renders in
    // the simulator, which does not load this stylesheet until it wears
    // Enamel itself (c6db2cfb) — without its own rule it would fall to the
    // browser's field there.
    const LEFT = [
      'auth/AuthAdminPage.svelte',
      'auth/LoginPage.svelte',
      'dispatcher/DispatcherCascadePage.svelte',
      'finance/MonthlyClosePackageButton.svelte',
      'it/monitoring/EventsPage.svelte',
      'jobs/JobsListPage.svelte',
      'jobs/TriageBoard.svelte',
      'kb/WorkflowsPage.svelte',
      'landing/SystemModelLiveView.svelte',
      'steps/ProductionConsumeSurface.svelte',
      'steps/ReceivingSurface.svelte',
      'web-kit/FeedbackControl.svelte',
      'web-kit/GlobalSearch.svelte',
      'workflows/StepInspector.svelte',
    ];
    const drawing = svelteFiles
      .filter(([, src]) => {
        const css = (src.match(/<style[^>]*>([\s\S]*?)<\/style>/)?.[1] ?? '').replace(
          /\/\*[\s\S]*?\*\//g,
          '',
        );
        return [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].some(
          (m) =>
            (m[1] ?? '').split(',').some((s) => isFieldSelector(s.trim())) &&
            /(?:^|[;\s])border(?:-width)?\s*:\s*(?!none|0\b)/.test(m[2] ?? ''),
        );
      })
      .map(([file]) => file);
    expect(drawing.sort()).toEqual(LEFT);
  });

  it('the one form that validates a field as it is typed wires aria-invalid to its error', () => {
    const editor = svelteFiles.find(([f]) => f === 'workflows/StepDagEditor.svelte')?.[1] ?? '';
    expect(editor).toMatch(/aria-invalid=\{metaError\[idx\] \? 'true' : undefined\}/);
    expect(editor).toMatch(/aria-describedby=\{metaError\[idx\] \? `sde-meta-err-\$\{idx\}` : undefined\}/);
    expect(editor).toMatch(/<span class="field-error" id=\{`sde-meta-err-\$\{idx\}`\}>/);
  });
});

/** Enamel's filter chip (backlog 6f471ff6, car 2): the round-2 board's
 *  `.chip` under `.t-enamel` — 12.5px, padding 5px 12px, squared to 3px, in
 *  a 2px ink frame; the pressed chip is solid ink under white words, and it
 *  is pressed by `aria-pressed`, so a chip cannot look chosen without
 *  saying so. Before this car FilterButton — on 16 pages — was a frameless
 *  word on a 4% wash when active, with no aria-pressed anywhere outside IT. */
describe('the filter chip is Enamel’s', () => {
  it('is 12.5px in a 2px ink frame squared to 3px', () => {
    expect(decl('.filter-btn', 'font-size')).toBe('12.5px');
    expect(decl('.filter-btn', 'padding')).toBe('5px 12px');
    expect(decl('.filter-btn', 'border')).toBe('var(--frame) solid var(--border-strong)');
    expect(decl('.filter-btn', 'border-radius')).toBe('var(--radius-field)');
    expect(decl('.filter-btn', 'background')).toBe('var(--card)');
    expect(decl('.filter-btn', 'color')).toBe('var(--text)');
  });

  it('is pressed in solid ink under white words, read off aria-pressed', () => {
    const pressed = '.filter-btn[aria-pressed="true"]';
    expect(decl(pressed, 'background')).toBe('var(--fog)');
    expect(decl(pressed, 'color')).toBe('var(--void)');
    expect(decl(pressed, 'border-color')).toBe('var(--fog)');
    expect(decl(pressed, 'font-weight')).toBe('700');
    // the -active class is a test hook now, never a second look
    expect(rules.has('.filter-btn-active')).toBe(false);
  });

  it('.filter-button is the same chip', () => {
    expect(rules.get('.filter-button')).toEqual(rules.get('.filter-btn'));
    expect(rules.get('.filter-button[aria-pressed="true"]')).toEqual(
      rules.get('.filter-btn[aria-pressed="true"]'),
    );
  });

  it('every filter chip a component draws says whether it is pressed', () => {
    const silent = svelteFiles.flatMap(([file, src]) =>
      [...src.matchAll(/<button\b[^>]*>/g)]
        .map((m) => m[0])
        .filter((tag) => /\bfilter-(?:btn|button)\b/.test(tag) && !/aria-pressed=/.test(tag))
        .map(() => file),
    );
    expect(silent).toEqual([]);
  });
});
