// THE LINES THE LIVE MOTION RIDES — the one place the motion (car M2 of
// design e765b3fc, live-motion.ts) is wired to the map's geometry: the
// routes GET /api/yard/routes serves, laid out by route-layout.ts (car
// R3) exactly as RouteLayer.svelte draws them. So a dot travels the
// stroke on the screen — section, exit or entry — lanes and arcs
// included, and walks it by the layout's own `walked`. A move on a route
// the read does not serve answers null here, and live-motion.ts draws it
// dashed red. With the routes unread, nothing is served: every move is
// drawn that way, which is true of what the page knows.

import type { Geometry } from './live-motion';
import { sectionKey, sectionsOf } from './route-layout';
import type { Routes } from './routes';
import { stationOf } from './transit';

/** The motion's geometry over one routes read — laid out once per read,
 *  not once per move. */
export function geometryOf(routes: Routes | null): Geometry {
  const byKey = new Map((routes === null ? [] : sectionsOf(routes).sections).map((s) => [s.key, s] as const));
  return {
    path: (from, to) => {
      const s = byKey.get(sectionKey(from, to));
      return s === undefined ? null : { d: s.d, walked: s.walked };
    },
    stationAt: (name) => {
      const s = stationOf(name);
      return s === undefined ? null : { x: s.x, y: s.y };
    },
    lineOf: (from, to) => byKey.get(sectionKey(from, to))?.line ?? null,
  };
}
