<script lang="ts">
  // Domain wrapper: maps InvoiceStatus onto the closed five-tone set
  // and renders the canonical web-kit StatusChip. The mapping is the
  // wrapper's whole job — call sites stay ignorant of tones.
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';
  import type { InvoiceStatus } from './types';

  type Props = { status: InvoiceStatus };
  let { status }: Props = $props();
  let tone: 'ok' | 'warn' | 'muted' = $derived(
    status === 'paid' ? 'ok' : status === 'past-due' ? 'warn' : 'muted',
  );
</script>

<StatusChip value={status} {tone} />
