// Asset types — shape matches /api/assets + /api/assets/summary.

export type AssetLifecyclePhase =
  | 'registered' | 'received' | 'shipped' | 'installed' | 'out-for-service'
  | 'decommissioned';

export type Asset = {
  asset_id: string;
  // Null until the unit is identified — identity-first: an asset is
  // `registered` (it exists) before its catalog model is known.
  sku: string | null;
  phase: AssetLifecyclePhase;
  holder_kind: string | null;
  holder_id: string | null;
  warranty_through: string | null;
  open_ticket_count: number;
  first_seen: string;
  last_event_at: string;
  oem_serial: string | null;
};

export type AssetsSummary = {
  phase_counts: Array<{ phase: string; count: number }>;
  total_systems: number;
  in_field_count: number;
  open_tickets_total: number;
  warranty_expiring_30d: number;
  sku_counts?: Array<{ sku: string; count: number }>;
};

export type AssetEvent = {
  id: string;
  asset_id: string;
  ts: string;
  actor_id: string | null;
  kind: string;
  [k: string]: unknown;
};
