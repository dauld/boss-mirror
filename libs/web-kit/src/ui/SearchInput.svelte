<script lang="ts">
  // `label` is the input's accessible name. The FilterGroup heading above
  // a search box is a <div>, not a <label>, so without it the input's only
  // name is its placeholder, which is example text rather than a name and
  // is gone once anything is typed (backlog 2361ac45, /ux/parts).
  let { value = $bindable(''), placeholder = '', label } = $props<{
    value?: string;
    placeholder?: string;
    label?: string;
  }>();
</script>

<input
  type="search"
  class="filter-input"
  {placeholder}
  aria-label={label}
  bind:value
/>
