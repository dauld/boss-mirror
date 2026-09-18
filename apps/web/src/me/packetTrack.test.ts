import { describe, expect, test } from 'bun:test';
import {
  PACKET_STOPS,
  NO_TRACK,
  placeOnTrack,
  stopOf,
  type FeedbackPacket,
} from './packetTrack';

function packet(over: Partial<FeedbackPacket>): FeedbackPacket {
  return {
    id: 'f1',
    status: 'open',
    opened_on: '2026-08-15',
    subject: { id: '/shop' },
    steps: [],
    ...over,
  } as FeedbackPacket;
}

const step = (slug: string, status: string) => ({ spec_slug: slug, status });

function cardsAt(track: ReturnType<typeof placeOnTrack>, key: string): readonly string[] {
  return track.stops.find((s) => s.key === key)!.cards.map((c) => c.id);
}

describe('stopOf', () => {
  test('stands at the stop of the step someone is actually working', () => {
    expect(
      stopOf(packet({ steps: [step('triage', 'completed'), step('build', 'ready')] })),
    ).toBe('building');
  });

  test('active beats ready — that is the one in hand', () => {
    expect(
      stopOf(packet({ steps: [step('investigate', 'active'), step('build', 'ready')] })),
    ).toBe('working');
  });

  test('drafting the design a feedback asked for is work in hand', () => {
    // f90ca046: the design route opens the executor's draft before the
    // founder's review; both are "working" from the visitor's side.
    expect(
      stopOf(packet({ steps: [step('draft-design', 'ready'), step('design-review', 'pending')] })),
    ).toBe('working');
  });

  test('waiting on the reporter is still "being read", not progress', () => {
    // From the visitor's side the honest statement is that someone is
    // reading it and wants more — not that it advanced.
    expect(stopOf(packet({ steps: [step('needs-info', 'ready')] }))).toBe('reading');
  });

  test('nothing actionable stands at the first stop rather than inventing progress', () => {
    expect(stopOf(packet({ steps: [step('closed', 'pending')] }))).toBe('received');
  });

  test('a step this build does not know stands at the first stop', () => {
    expect(stopOf(packet({ steps: [step('re-triage', 'ready')] }))).toBe('received');
  });

  test('done means it reached the terminal that means something was done', () => {
    expect(
      stopOf(packet({ status: 'closed', steps: [step('closed', 'completed')] })),
    ).toBe('done');
  });

  test('turned down or duplicate leaves the track — it is not "done"', () => {
    expect(
      stopOf(packet({ status: 'closed', steps: [step('declined', 'completed')] })),
    ).toBeNull();
    expect(
      stopOf(packet({ status: 'closed', steps: [step('duplicate', 'completed')] })),
    ).toBeNull();
  });

  test('a skipped terminal is not the one it reached', () => {
    expect(
      stopOf(
        packet({
          status: 'closed',
          steps: [step('duplicate', 'skipped'), step('closed', 'completed')],
        }),
      ),
    ).toBe('done');
  });
});

describe('placeOnTrack', () => {
  test('puts each packet at its stop and keeps the track in order', () => {
    const t = placeOnTrack([
      packet({ id: 'a', steps: [step('triage', 'ready')] }),
      packet({ id: 'b', steps: [step('build', 'active')] }),
      packet({ id: 'c', status: 'closed', steps: [step('closed', 'completed')] }),
    ]);
    expect(t.stops.map((s) => s.key)).toEqual(PACKET_STOPS.map((s) => s.key));
    expect(cardsAt(t, 'reading')).toEqual(['a']);
    expect(cardsAt(t, 'building')).toEqual(['b']);
    expect(cardsAt(t, 'done')).toEqual(['c']);
    expect(t.received).toBe(3);
    expect(t.done).toBe(1);
  });

  test('the ones turned down are counted, not hidden', () => {
    const t = placeOnTrack([
      packet({ id: 'no', status: 'closed', steps: [step('declined', 'completed')] }),
      packet({ id: 'yes', status: 'closed', steps: [step('closed', 'completed')] }),
    ]);
    expect(t.setAside).toBe(1);
    expect(t.done).toBe(1);
    expect(cardsAt(t, 'done')).toEqual(['yes']);
  });

  test('simulated packets never reach the track — the claim has to be true', () => {
    const t = placeOnTrack([
      packet({ id: 'real' }),
      packet({ id: 'sim', simulated: true }),
    ]);
    expect(t.received).toBe(1);
    expect(cardsAt(t, 'received')).toEqual(['real']);
  });

  test('a crowded stop keeps the freshest cards and still counts them all', () => {
    const many = Array.from({ length: 7 }, (_, i) =>
      packet({ id: `f${i}`, opened_on: `2026-08-0${i + 1}` }),
    );
    const t = placeOnTrack(many, 2);
    expect(cardsAt(t, 'received')).toEqual(['f6', 'f5']);
    expect(t.received).toBe(7);
  });

  test('a packet with no subject still reads as being about something', () => {
    const t = placeOnTrack([packet({ subject: null })]);
    expect(t.stops.find((s) => s.key === 'received')!.cards[0]?.about).toBe('the app');
  });

  test('nothing in, nothing claimed — but the track still stands', () => {
    const t = placeOnTrack([]);
    expect(t.any).toBe(false);
    expect(t).toEqual(NO_TRACK);
    expect(t.stops).toHaveLength(PACKET_STOPS.length);
  });
});
