// What happened to the feedback guests sent — as packets standing at
// stations, which is what they actually are.
//
// Origin (David, feedback cef0f06f): "We need a better Guest landing
// experience. It should welcome people to Algedonic Ales, introduce
// them to the guest experience, and I think it would be cool if we
// could show how Guest feedback has been flowing through the real IT
// department to add functionality as we go." Then, on the first cut:
// "I think the Guest landing looks too much like an employee view
// still ... I think we show that more as job cards moving through
// stations instead of just a static list."
//
// The first version was a table of rows in the instrument idiom — the
// yard's language, aimed at an operator who already knows what a
// station is. A visitor does not. So the same truth is rendered as
// motion: a short track of stops, and each piece of feedback standing
// at the one it has reached.
//
// The claim this makes is unusual and worth making carefully: the
// feedback control in the bar does not file a ticket into a void — it
// opens a Job that moves through the same stations, protocols and
// trains as every other piece of work here. That is only worth showing
// if it is TRUE, so nothing below invents a stop, rounds a count, or
// quietly drops the feedback that was turned down.

import { partitionOf, type Partition } from '@boss/web-kit/ui/packet-card';

/** A feedback packet as the jobs API serves it. Steps arrive with the
 *  job on the list endpoint, so the current stop needs no second
 *  call. */
export type FeedbackPacket = Readonly<{
  id: string;
  status: string;
  opened_on: string;
  subject?: Readonly<{ id?: string }> | null;
  partition?: Partition;
  simulated?: boolean;
  steps?: ReadonlyArray<
    Readonly<{ spec_slug?: string | null; title?: string | null; status: string }>
  >;
}>;

/** The track a guest sees, in order.
 *
 *  Five stops, not the protocol's nine steps. A visitor is being shown
 *  that their words moved, not taught `user-feedback`'s step graph —
 *  and a track wide enough to need scrolling stops reading as motion.
 *  The protocol's own vocabulary maps onto these; it is never renamed
 *  in the data, only on the platform sign. */
export const PACKET_STOPS = [
  { key: 'received', label: 'Received' },
  { key: 'reading', label: 'Being read' },
  { key: 'working', label: 'Being worked out' },
  { key: 'building', label: 'Being built' },
  { key: 'done', label: 'Done' },
] as const;

export type StopKey = (typeof PACKET_STOPS)[number]['key'];

/** Protocol step slug → the stop a guest sees it standing at.
 *  `needs-info` sits at "Being read" deliberately: from the visitor's
 *  side the honest statement is that someone is still reading it and
 *  wants more, not that it has advanced. */
const STOP_OF_STEP: Readonly<Record<string, StopKey>> = {
  submitted: 'received',
  triage: 'reading',
  'needs-info': 'reading',
  investigate: 'working',
  'design-review': 'working',
  build: 'building',
};

/** Terminals that mean the packet left the track rather than finished
 *  it. Kept visible in their own line — feedback that was turned down
 *  is still an answer, and hiding it would make the track a
 *  advertisement. */
const OFF_TRACK: Readonly<Record<string, string>> = {
  duplicate: 'already reported',
  declined: 'not taken up',
};

export type PacketCard = Readonly<{
  id: string;
  /** The surface it was about — the route path is the Subject id. */
  about: string;
  when: string;
}>;

export type TrackStop = Readonly<{
  key: StopKey;
  label: string;
  cards: readonly PacketCard[];
}>;

export type PacketTrack = Readonly<{
  stops: readonly TrackStop[];
  /** Everything real that has ever been sent. */
  received: number;
  /** Reached the `closed` terminal — feedback that changed something. */
  done: number;
  /** Left the track: duplicates and the ones turned down. */
  setAside: number;
  /** Anything at all to show. */
  any: boolean;
}>;

export const NO_TRACK: PacketTrack = {
  stops: PACKET_STOPS.map((s) => ({ ...s, cards: [] })),
  received: 0,
  done: 0,
  setAside: 0,
  any: false,
};

function slugOf(step: { spec_slug?: string | null; title?: string | null }): string {
  return (step.spec_slug ?? step.title ?? '').trim();
}

/** Which stop a packet is standing at, or `null` when it left the
 *  track.
 *
 *  An OPEN packet stands at the stop of the step someone is actually
 *  working — the first active, else the first ready. One with nothing
 *  actionable stands at `received`: it is waiting, and showing it
 *  further along would be inventing progress it has not made.
 *
 *  A CLOSED packet is `done` only if it reached the terminal that
 *  means something was done. */
export function stopOf(packet: FeedbackPacket): StopKey | null {
  const steps = packet.steps ?? [];
  if (packet.status !== 'open') {
    for (const s of steps) {
      if (s.status !== 'completed') continue;
      if (OFF_TRACK[slugOf(s)]) return null;
      if (slugOf(s) === 'closed') return 'done';
    }
    return 'done';
  }
  const working =
    steps.find((s) => s.status === 'active') ?? steps.find((s) => s.status === 'ready');
  if (!working) return 'received';
  return STOP_OF_STEP[slugOf(working)] ?? 'received';
}

/** Place the feedback corpus on the track.
 *
 *  `perStop` caps how many cards stand at any one stop so a busy stop
 *  cannot push the track off the page; the counts are over everything.
 *  Simulated packets are dropped entirely — the demo tenant generates
 *  synthetic work, and counting it here would turn a true claim into a
 *  marketing number. */
export function placeOnTrack(
  packets: readonly FeedbackPacket[],
  perStop = 4,
): PacketTrack {
  const real = packets.filter((p) => partitionOf(p) === 'real');
  const byStop = new Map<StopKey, PacketCard[]>(
    PACKET_STOPS.map((s) => [s.key, [] as PacketCard[]]),
  );
  let setAside = 0;

  // Newest first, so a stop that overflows keeps the freshest cards.
  const ordered = [...real].sort((a, b) =>
    (b.opened_on ?? '').localeCompare(a.opened_on ?? ''),
  );
  for (const p of ordered) {
    const stop = stopOf(p);
    if (stop === null) {
      setAside += 1;
      continue;
    }
    byStop.get(stop)!.push({
      id: p.id,
      about: (p.subject?.id ?? '').trim() || 'the app',
      when: p.opened_on,
    });
  }

  const stops = PACKET_STOPS.map((s) => ({
    key: s.key,
    label: s.label,
    cards: byStop.get(s.key)!.slice(0, perStop),
  }));
  return {
    stops,
    received: real.length,
    done: byStop.get('done')!.length,
    setAside,
    any: real.length > 0,
  };
}
