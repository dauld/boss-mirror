// What the warehouse header is allowed to say, given which reads answered.
//
// WHY A FUNCTION AND NOT TWO $derived IN THE PAGE (backlog 8b1deea2, the
// /ux/warehouse audit, gap 5). The header fell back to the items list
// whenever the status body was missing, and that list is empty both
// before it answers and after it fails — so the loading window and an
// outage of both reads painted "0 tracked SKUs" / "0 below reorder
// point", an empty warehouse the data never claimed. Which read may
// speak is the whole question, so it is one function with one test.

import type { ReadState } from '../data/readState';
import type { WarehouseStatus } from './types';

export type WarehouseHeader = Readonly<{ title: string; subtitle: string }>;

export function warehouseHeader(
  status: Readonly<{ read: ReadState; body: WarehouseStatus | null }>,
  items: Readonly<{ read: ReadState; skus: number; belowReorder: number }>,
): WarehouseHeader {
  if (status.body) {
    const s = status.body;
    return {
      title: `${s.parts_stock.total_skus} tracked SKUs`,
      subtitle: `${s.parts_stock.below_reorder_count} below reorder · ${s.inbound_pos.total_open} open POs`,
    };
  }
  if (items.read.kind === 'ok') {
    return { title: `${items.skus} tracked SKUs`, subtitle: `${items.belowReorder} below reorder point` };
  }
  if (status.read.kind === 'loading' || items.read.kind === 'loading') {
    return { title: 'Loading warehouse…', subtitle: '' };
  }
  return { title: 'Tracked SKUs unavailable', subtitle: "Couldn't load warehouse status or inventory" };
}
