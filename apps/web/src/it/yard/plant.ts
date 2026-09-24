// THE PLANT STRIP (design 62de32ae, decision 11: "Host runners move to
// a plant strip along the map's edge, because they serve every region";
// approved by David 2026-09-24, car E on backlog c3105b2a).
//
// The review of 2026-09-24 found the boss-gcp and forge host runners
// drawn as 10px squares in the corner of RECEIVING — filed there
// because an ops-request is an inbound kind — while neither moves a
// packet into receiving: they answer the converge for arrivals and the
// probe for the shed alike. Machinery that serves every region belongs
// to none of them, so the server now sends it apart (`plant` on
// `/api/yard/regions`) and the world draws it here, on a strip of its
// own under the territories.
//
// The strip is the machinery idiom (world-machines.ts) with its words
// written out: each machine is its glyph AND its name and state, since
// a strip along the edge has the room a territory's corner did not.
// Nothing here judges a machine — the server does — and what does not
// fit is COUNTED, never dropped.

import type { Machine } from './regions';
import { machineOrder } from './world-machines';

/** The strip's height, in the world's own units. */
export const PLANT_H = 30;

const GLYPH = 12;
const EDGE = 8;
/** The lead label's room: "PLANT · serves every region". */
const LEAD_W = 236;
const TEXT_GAP = 6;
const ITEM_GAP = 22;
/** What a character of the strip's 10px mono text needs, in the world's units
 *  (measured at 1440px on 2026-09-24: 6.9). */
const CHAR_W = 7.2;

export type PlantGlyph = Readonly<{
  machine: Machine;
  /** The glyph's box. */
  x: number;
  y: number;
  w: number;
  h: number;
  /** Where the words start, and what they say: `forge runner · idle`. */
  textX: number;
  label: string;
}>;

/** Lay the plant along a strip `width` wide: the lead label, then each
 *  machine — failed first, unknown next — as a glyph with its words. */
export function plantLayout(
  machines: ReadonlyArray<Machine>,
  width: number,
): Readonly<{ placed: ReadonlyArray<PlantGlyph>; hidden: number; leadX: number }> {
  const y = (PLANT_H - GLYPH) / 2;
  const ordered = machineOrder(machines);
  const placed: PlantGlyph[] = [];
  let x = EDGE + LEAD_W;
  for (const machine of ordered) {
    const label = `${machine.name} · ${machine.state}`;
    const textX = x + GLYPH + TEXT_GAP;
    const end = textX + label.length * CHAR_W;
    // Room is kept at the end for the "+N more" note, so a count of what
    // did not fit never runs under the last machine's words.
    if (end > width - EDGE - (placed.length + 1 < ordered.length ? 60 : 0)) break;
    placed.push({ machine, x, y, w: GLYPH, h: GLYPH, textX, label });
    x = end + ITEM_GAP;
  }
  return { placed, hidden: ordered.length - placed.length, leadX: EDGE };
}
