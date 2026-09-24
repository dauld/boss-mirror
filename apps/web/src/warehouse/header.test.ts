import { describe, expect, test } from 'bun:test';
import { failedRead, loadingRead, okRead } from '../data/readState';
import { warehouseHeader } from './header';
import type { WarehouseStatus } from './types';

// Backlog 8b1deea2 (the /ux/warehouse audit, gap 5). The header fell
// back to the items list whenever the status body was missing, and the
// items list is empty both before it answers and after it fails — so the
// loading window and an outage of both reads painted "0 tracked SKUs" /
// "0 below reorder point", an empty warehouse the data never claimed.

const STATUS: WarehouseStatus = {
  parts_stock: {
    total_skus: 5, total_on_hand: 482, total_allocated: 107, total_available: 375,
    below_reorder_count: 3, below_reorder_items: [],
  },
  inbound_pos: {
    total_open: 3, draft_count: 1, submitted_count: 1, acknowledged_count: 0,
    in_transit_count: 1, late_count: 1, arriving_this_week_count: 2, recent: [],
  },
  outbound_shipments: { label_created: 0, picked_up: 0, in_transit: 0, exception: 0, delivered_7d: 0, recent: [] },
  refurb_wip: { total_in_flight: 7, by_stage: [] },
  ready_for_sale_count: 2,
  as_of: '2026-09-23T22:00:00Z',
};
const BREWERY: WarehouseStatus = { ...STATUS, refurb_wip: { total_in_flight: 0, by_stage: [] }, ready_for_sale_count: 0 };

const items = (read = okRead, skus = 5, belowReorder = 3) => ({ read, skus, belowReorder });

describe('the warehouse header counts only what a read answered', () => {
  test('the status projection answers the header when it has answered', () => {
    expect(warehouseHeader({ read: okRead, body: STATUS }, items(loadingRead, 0, 0))).toEqual({
      title: '5 tracked SKUs',
      subtitle: '3 below reorder · 3 open POs · 7 refurb WIP · 2 ready for sale',
    });
  });

  test('zero refurb and zero ready-for-sale drop out — the brewery shape', () => {
    expect(warehouseHeader({ read: okRead, body: BREWERY }, items()).subtitle).toBe('3 below reorder · 3 open POs');
  });

  test('with no status body the items read answers, whether status is pending or failed', () => {
    const fallback = { title: '5 tracked SKUs', subtitle: '3 below reorder point' };
    expect(warehouseHeader({ read: loadingRead, body: null }, items())).toEqual(fallback);
    expect(warehouseHeader({ read: failedRead('HTTP 503'), body: null }, items())).toEqual(fallback);
  });

  test('an empty warehouse that answered still says zero', () => {
    expect(warehouseHeader({ read: failedRead('x'), body: null }, items(okRead, 0, 0))).toEqual({
      title: '0 tracked SKUs',
      subtitle: '0 below reorder point',
    });
  });

  test('neither read answered yet: loading, and no number', () => {
    for (const statusRead of [loadingRead, failedRead('HTTP 503')]) {
      for (const itemsRead of [loadingRead, failedRead('HTTP 503')]) {
        if (statusRead.kind === 'failed' && itemsRead.kind === 'failed') continue;
        expect(warehouseHeader({ read: statusRead, body: null }, items(itemsRead, 0, 0))).toEqual({
          title: 'Loading warehouse…',
          subtitle: '',
        });
      }
    }
  });

  test('both reads failed: the header says the count is unavailable, never zero', () => {
    const h = warehouseHeader({ read: failedRead('HTTP 503'), body: null }, items(failedRead('HTTP 503'), 0, 0));
    expect(h).toEqual({
      title: 'Tracked SKUs unavailable',
      subtitle: "Couldn't load warehouse status or inventory",
    });
    expect(h.title).not.toContain('0');
  });
});
