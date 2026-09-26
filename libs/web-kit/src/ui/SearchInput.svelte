<script lang="ts">
  // `label` is the input's accessible name. The FilterGroup heading above
  // a search box is a <div>, not a <label>, so without it the input's only
  // name is its placeholder, which is example text rather than a name and
  // is gone once anything is typed (backlog 2361ac45, /ux/parts).
  // Required, not optional: with an optional label, nine of eleven uses
  // shipped without one, so svelte-check now refuses an unnamed search box
  // instead of a reviewer having to notice it (backlog d03c6b72).
  let {
    value = $bindable(''),
    placeholder = '',
    label,
  }: {
    value?: string;
    placeholder?: string;
    label: string;
  } = $props();
</script>

<input
  type="search"
  class="filter-input"
  {placeholder}
  aria-label={label}
  bind:value
/>
