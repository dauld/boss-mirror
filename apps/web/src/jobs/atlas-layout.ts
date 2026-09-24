// Shared Workflow "atlas" layout engine.
//
// The System Atlas (IT monitoring) and the landing page's operating-model
// map both read /api/workflows and lay the active Workflows out the same
// way: one row per category (a few pinned at the top, the rest
// alphabetical), nodes wrapping to the next line at the canvas width, each
// node showing its step count. The two pages used to carry parallel copies
// of this engine that drifted only in their canvas width; this module is
// the single copy, with `canvasW` as the one parameter that differs.

/** The minimal Workflow shape the layout reads. Any richer spec — the
 *  landing page's `WorkflowSpec` or the canonical `workflowTypes` one — is
 *  structurally assignable, so callers pass their own spec arrays. */
export type AtlasSpec = Readonly<{
  kind: string;
  label: string;
  category: string;
  steps: ReadonlyArray<unknown>;
}>;

export type AtlasNode = Readonly<{
  kind: string;
  label: string;
  category: string;
  step_count: number;
  x: number;
  y: number;
}>;

export type AtlasRow = Readonly<{
  category: string;
  y: number;
  nodes: ReadonlyArray<AtlasNode>;
}>;

// Node dimensions — shared so both canvases render identical node shapes.
// NODE_W/NODE_H are exported because the callers' SVG markup draws the
// `<rect>` and centres labels with them.
export const NODE_W = 180;
export const NODE_H = 64;
const COL_GAP = 18;
const ROW_GAP = 28;
const ROW_LABEL_W = 130;

// Categories pinned to the top — the most-active flows lead; everything
// else sorts alphabetically.
const PINNED = ['production', 'sales', 'procurement', 'operations', 'finance'];

/** Lay Workflow specs out by category into positioned SVG rows + nodes.
 *  `canvasW` is the only per-page difference (the System Atlas canvas is
 *  wider than the landing card), so it is a parameter. */
export function atlasLayout(
  specs: ReadonlyArray<AtlasSpec>,
  canvasW: number,
): { rows: ReadonlyArray<AtlasRow>; height: number } {
  const byCat = new Map<string, AtlasSpec[]>();
  for (const s of specs) {
    const cat = s.category || 'other';
    const list = byCat.get(cat) ?? [];
    list.push(s);
    byCat.set(cat, list);
  }
  const cats = Array.from(byCat.keys()).sort((a, b) => {
    const ai = PINNED.indexOf(a);
    const bi = PINNED.indexOf(b);
    if (ai !== -1 && bi !== -1) return ai - bi;
    if (ai !== -1) return -1;
    if (bi !== -1) return 1;
    return a.localeCompare(b);
  });

  const rows: AtlasRow[] = [];
  let cursorY = 60;
  const usableW = canvasW - ROW_LABEL_W - 20;
  const perRow = Math.max(1, Math.floor((usableW + COL_GAP) / (NODE_W + COL_GAP)));

  for (const category of cats) {
    const list = (byCat.get(category) ?? [])
      .slice()
      .sort((a, b) => a.kind.localeCompare(b.kind));
    const nodes: AtlasNode[] = [];
    list.forEach((spec, i) => {
      const wrapRow = Math.floor(i / perRow);
      const col = i % perRow;
      nodes.push({
        kind: spec.kind,
        label: spec.label,
        category,
        step_count: spec.steps.length,
        x: ROW_LABEL_W + col * (NODE_W + COL_GAP),
        y: cursorY + wrapRow * (NODE_H + ROW_GAP / 2),
      });
    });
    const wraps = Math.ceil(list.length / perRow);
    rows.push({ category, y: cursorY, nodes });
    cursorY += wraps * (NODE_H + ROW_GAP / 2) + ROW_GAP;
  }
  return { rows, height: cursorY + 20 };
}

// colour-literal-ok: the department palette is data, kept apart from Enamel's
// state plates until the Finance/clear collision is decided (a4df741a).
// Labels sit on these fills in --fog (15.8-16.7:1) and --static (6.1-6.5:1).
const CATEGORY_COLORS: Record<string, { stroke: string; fill: string }> = {
  production: { stroke: '#3b82f6', fill: '#eff6ff' },
  sales: { stroke: '#10b981', fill: '#ecfdf5' },
  procurement: { stroke: '#f59e0b', fill: '#fffbeb' },
  operations: { stroke: '#8b5cf6', fill: '#f5f3ff' },
  finance: { stroke: '#dc2626', fill: '#fef2f2' },
  marketing: { stroke: '#ec4899', fill: '#fdf2f8' },
  hr: { stroke: '#0891b2', fill: '#ecfeff' },
  platform: { stroke: '#64748b', fill: '#f8fafc' },
  other: { stroke: '#64748b', fill: '#f8fafc' },
};

/** Stroke + fill for a category's node, falling back to the neutral
 *  `other` palette for unknown categories. */
export function atlasColorFor(cat: string): { stroke: string; fill: string } {
  return CATEGORY_COLORS[cat] ?? CATEGORY_COLORS['other']!;
}
