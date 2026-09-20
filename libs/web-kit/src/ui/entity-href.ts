// Pure helpers for EntityLink — extracted so non-Svelte callers can
// import without pulling a component. Same contract as
// apps/web/src/ui/EntityLink.tsx.

import { href } from '../nav';

// The kinds, as DATA — the union is derived from this array so the
// list a test can iterate and the type the compiler checks cannot
// disagree (CLAUDE.md 9a). Every kind here must resolve to a route:
// apps/web/src/entity-href-routes.test.ts is the pin.
//
// 'ticket', 'agreement' and 'opportunity' were RETIRED here on
// 2026-09-20 (backlog 38d4e458, from the /ux/support page audit
// 9876ef0d): each produced a path the router never matched, so every
// link they made was a dead end, and two of them were eaten by a
// greedy wildcard and landed on the wrong detail page instead of
// nowhere. None had a page to route to — a support case IS a Job
// (/ux/support lists kind=field-service and links its rows as 'job'),
// and contracts/opportunities have no detail surface at all. A page is
// a page car; producing a link to a page that does not exist is not.
export const ENTITY_KINDS = [
  'account',
  'employee',
  'job',
  'invoice',
  'asset',
  'part',
  'product',
  'vendor',
  'po',
  'vendor-invoice',
  'shipment',
  'fact',
  'ledger-entry',
  'marketing-asset',
] as const;

export type EntityKind = (typeof ENTITY_KINDS)[number];

export function entityHref(kind: EntityKind, id: string): string {
  const encoded = encodeURIComponent(id);
  switch (kind) {
    case 'account': return href(`/ux/accounts/${encoded}`);
    case 'employee': return href(`/ux/people/${encoded}`);
    case 'job': return href(`/ux/jobs/${encoded}`);
    case 'invoice': return href(`/ux/finance/${encoded}`);
    case 'asset': return href(`/ux/assets/${encoded}`);
    case 'part': return href(`/ux/parts/${encoded}`);
    case 'product': return href(`/ux/products/${encoded}`);
    case 'vendor': return href(`/ux/vendors/${encoded}`);
    case 'po': return href(`/ux/purchase-orders/${encoded}`);
    case 'vendor-invoice': return href(`/ux/vendor-invoices/${encoded}`);
    case 'shipment': return href(`/ux/shipments/${encoded}`);
    case 'fact': return href(`/ux/finance?fact=${encoded}`);
    case 'ledger-entry': return href(`/ux/finance?entry=${encoded}`);
    case 'marketing-asset': return href(`/ux/marketing-assets/${encoded}`);
  }
}

export const ID_IS_LABEL: ReadonlySet<EntityKind> = new Set<EntityKind>([
  'asset', 'product', 'invoice', 'po', 'vendor-invoice', 'shipment', 'fact',
  'ledger-entry', 'marketing-asset',
]);
