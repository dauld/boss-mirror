// THE LINES THE LIVE MOTION RIDES — the one place the motion (car M2 of
// design e765b3fc, live-motion.ts) is wired to the map's geometry.
//
// Today that is the transit map's own sections and stations
// (transit.ts). Car R3 replaces those hand-drawn paths with the routes
// GET /api/yard/routes serves, octilinearly routed, off-ramps included;
// when it lands, `path` here reads R3's route function instead and the
// motion follows the served lines with no other change. Until then a
// route the transit map does not draw — an off-ramp, a pair no section
// joins — falls back to live-motion.ts's straight line or its off-map
// stub, and an undeclared one is drawn dashed red either way.

import type { Geometry } from './live-motion';
import { SECTIONS, stationOf } from './transit';

export const transitGeometry: Geometry = {
  path: (from, to) => SECTIONS.find((s) => s.from === from && s.to === to)?.d ?? null,
  stationAt: (name) => {
    const s = stationOf(name);
    return s === undefined ? null : { x: s.x, y: s.y };
  },
  lineOf: (from, to) => SECTIONS.find((s) => s.from === from && s.to === to)?.line ?? null,
};
