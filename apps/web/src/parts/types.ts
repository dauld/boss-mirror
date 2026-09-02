// Inventory & parts domain. Ported from apps/web/src/parts/types.ts.

export type InventoryItem = {
  part_sku: string;
  bin: string;
  on_hand: number;
  allocated: number;
  reorder_point: number;
  reorder_qty: number;
  trailing_90d_usage: number;
};

export type PoStatus =
  | 'draft'
  | 'submitted'
  | 'acknowledged'
  | 'in-transit'
  | 'received'
  | 'closed';

export type PurchaseOrderLine = {
  part_sku: string;
  qty: number;
  unit_cost_cents: number;
  currency: string;
};

export type PurchaseOrder = {
  id: string;
  vendor: string;
  status: PoStatus;
  placed_on: string;
  expected_on: string;
  received_on: string | null;
  lines: ReadonlyArray<PurchaseOrderLine>;
};

export type StockStatus = 'healthy' | 'low' | 'critical' | 'out';

export function stockStatus(item: InventoryItem): StockStatus {
  const available = item.on_hand - item.allocated;
  if (available <= 0) return 'out';
  if (available < item.reorder_point / 2) return 'critical';
  if (available <= item.reorder_point) return 'low';
  return 'healthy';
}

/// Tone for the web-kit StatusChip; mirrors the retired chip-stock-*
/// CSS colors (out was --err, critical/low --warn, healthy --ok).
export function stockTone(status: StockStatus): 'ok' | 'warn' | 'err' {
  if (status === 'out') return 'err';
  if (status === 'healthy') return 'ok';
  return 'warn';
}

/// Flat row from `GET /api/catalog/parts`. The brewery's
/// PartsList reads this directly — no need to walk system_models
/// when the parts aren't satellites of tracked device assets.
export type CatalogPart = {
  part_sku: string;
  name: string;
  description: string;
  unit_price_cents: number;
  currency: string;
  lead_time_days: number;
};

/// Heuristic kind from sku prefix. Brewery uses `ING-` for
/// ingredients and `PKG-` for packaging; everything else is a
/// "spare" (the legacy device-asset shape).
export function kindFromSku(sku: string): 'ingredient' | 'packaging' | 'spare' {
  if (sku.startsWith('ING-')) return 'ingredient';
  if (sku.startsWith('PKG-')) return 'packaging';
  return 'spare';
}

// Minimal catalog part shape — we don't need the full DeviceModel,
// just what collectParts inspects.

export type SparePart = {
  part_sku: string;
  name: string;
  description: string;
  unit_price_cents: number;
  currency: string;
  lead_time_days: number;
  high_usage: boolean;
};

export type Consumable = {
  part_sku: string;
  name: string;
  description: string;
  unit_price_cents: number;
  currency: string;
  treatments_per_unit: number | null;
};

export type DeviceModel = {
  sku: string;
  name: string;
  category: string;
  spare_parts: ReadonlyArray<SparePart>;
  consumables: ReadonlyArray<Consumable>;
};

export type PartUsedBy = {
  sku: string;
  part: SparePart | Consumable;
  kind: 'spare' | 'consumable';
  used_by: ReadonlyArray<string>;
};

export function collectParts(
  catalog: ReadonlyArray<DeviceModel>,
): ReadonlyArray<PartUsedBy> {
  const map = new Map<
    string,
    { part: SparePart | Consumable; kind: 'spare' | 'consumable'; used_by: Set<string> }
  >();
  for (const model of catalog) {
    for (const p of model.spare_parts ?? []) {
      const entry = map.get(p.part_sku) ?? {
        part: p,
        kind: 'spare' as const,
        used_by: new Set<string>(),
      };
      entry.used_by.add(model.sku);
      map.set(p.part_sku, entry);
    }
    for (const c of model.consumables ?? []) {
      const entry = map.get(c.part_sku) ?? {
        part: c,
        kind: 'consumable' as const,
        used_by: new Set<string>(),
      };
      entry.used_by.add(model.sku);
      map.set(c.part_sku, entry);
    }
  }
  return [...map.entries()].map(([sku, e]) => ({
    sku,
    part: e.part,
    kind: e.kind,
    used_by: [...e.used_by],
  }));
}
