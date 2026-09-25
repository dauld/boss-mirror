<!--
  One node of the dispatcher cascade: its name, and the line under it
  (a rule's trigger and guard mark, a handler's emit count).

  A node type of the page's own, not the library's `default` (backlog
  db2ab40a). The default node drew one text label, so the two lines rode
  a newline under `white-space: pre-line` — which also rendered every
  newline of the library template's own whitespace wherever the compiler
  keeps it (the dev server the mocked suite serves does: a one-line
  event node painted 163px tall). And `.svelte-flow__node-default`'s
  150px width, 10px padding, border and background applied to it,
  winning or losing against the page by stylesheet order alone. Neither
  reaches this type. Its size is the box buildFlow lays out, set inline
  by the library from the node's own width and height.
-->
<script lang="ts">
  import { Handle, Position, type NodeProps } from '@xyflow/svelte';

  let { data }: NodeProps = $props();
  const name = $derived(String(data.label ?? ''));
  const sublabel = $derived(typeof data.sublabel === 'string' ? data.sublabel : '');
</script>

<Handle type="target" position={Position.Left} />
<span class="dx-name">{name}</span>
{#if sublabel}<span class="dx-sublabel"> {sublabel}</span>{/if}
<Handle type="source" position={Position.Right} />

<style>
  .dx-name,
  .dx-sublabel {
    display: block;
    overflow-wrap: anywhere;
  }
</style>
