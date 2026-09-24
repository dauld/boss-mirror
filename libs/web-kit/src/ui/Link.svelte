<script lang="ts">
  // Client-side navigation link. Mirrors apps/web/src/ui/index.tsx Link.
  import { navigate } from '../nav';

  let { to, className = '', children } = $props<{
    to: string;
    className?: string;
    children: () => any;
  }>();

  function onClick(e: MouseEvent) {
    // A link's click is the link's alone. Inside a clickable row (the
    // `data-table-row-link` tables) the click used to bubble on to the
    // row's own navigate, so ONE click on an account name pushed two
    // history entries and the first Back landed on the same page
    // (backlog 18890a16, /ux/accounts) — and a cmd-click opened the new
    // tab AND moved this one. Stopping it here answers every such row at
    // once; the tables that hand-rolled their anchors already did this.
    e.stopPropagation();
    // Allow modified clicks (cmd/ctrl/shift) to fall through to the
    // browser's default open-in-new-tab behavior.
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(to);
  }
</script>

<a href={to} class={className} onclick={onClick}>{@render children()}</a>
