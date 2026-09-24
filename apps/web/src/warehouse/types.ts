// Port of apps/web/src/warehouse/types.ts.

export type LowStockItem = {
  part_sku: string;
  bin: string;
  on_hand: number;
  allocated: number;
  available: number;
  reorder_point: number;
};

export type PartsStockSummary = {
  total_skus: number;
  total_on_hand: number;
  total_allocated: number;
  total_available: number;
  below_reorder_count: number;
  below_reorder_items: ReadonlyArray<LowStockItem>;
};

export type InboundPoRow = {
  id: string;
  vendor: string;
  status: string;
  expected_on: string;
  days_until_expected: number;
  line_count: number;
};

export type InboundPoSummary = {
  total_open: number;
  draft_count: number;
  submitted_count: number;
  acknowledged_count: number;
  in_transit_count: number;
  late_count: number;
  arriving_this_week_count: number;
  recent: ReadonlyArray<InboundPoRow>;
};

export type OutboundShipmentRow = {
  id: string;
  status: string;
  carrier: string;
  destination: string;
  account_id: string | null;
  shipped_on: string | null;
  estimated_delivery: string | null;
  asset_id_count: number;
};

export type OutboundShipmentSummary = {
  label_created: number;
  picked_up: number;
  in_transit: number;
  exception: number;
  delivered_7d: number;
  recent: ReadonlyArray<OutboundShipmentRow>;
};

export type WarehouseStatus = {
  parts_stock: PartsStockSummary;
  inbound_pos: InboundPoSummary;
  outbound_shipments: OutboundShipmentSummary;
  as_of: string;
};
