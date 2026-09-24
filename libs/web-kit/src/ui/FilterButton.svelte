<script lang="ts">
  let { active, onclick, children } = $props<{
    active: boolean;
    onclick: () => void;
    children: () => any;
  }>();
</script>

<!-- A filter chip is a toggle, so it says whether it is pressed: Enamel
     draws the pressed chip in solid ink off aria-pressed (backlog 6f471ff6,
     car 2), and the -active class stays as the hook the specs read. -->
<button
  type="button"
  class="filter-btn {active ? 'filter-btn-active' : ''}"
  aria-pressed={active}
  {onclick}
>
  {@render children()}
</button>
