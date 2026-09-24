<script lang="ts">
  // A department's jobs — in / working / out over the packets whose
  // workflow declares the department (cc76f755, 2026-09-18).
  //
  // David, 2026-09-12: every department has the three thirds. IT's
  // are three pages; this is the one page every OTHER department gets
  // until it earns a surface of its own — the tab used to land on All
  // jobs with Home highlighted. Content and structure only: it renders
  // with the jobs list's own table and no styling of its own, because
  // the redesign is a separate conversation and inherits this.
  //
  // The thirds themselves are DepartmentThirds, shared with the
  // department surfaces that carry them too (/ux/parts, 044dffa1).
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { departmentLabel } from '@boss/web-kit/nav';
  import { departments } from '@boss/web-kit/session/departments.svelte';
  import DepartmentThirds from './DepartmentThirds.svelte';

  let { code } = $props<{ code: string }>();

  const label = $derived(departmentLabel(code, departments()));
</script>

<div class="catalog theme-exec">
  <PageHeader eyebrow={label} title="Jobs" subtitle="What is coming in, being worked, and going out." />
  <DepartmentThirds {code} />
</div>
