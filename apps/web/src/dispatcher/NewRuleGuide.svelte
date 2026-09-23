<script lang="ts">
  // /it/registry/rules/new — where "+ New rule" lands. It explains how
  // to author a rule that lasts, and it creates nothing (backlog
  // 7d9df2fe, design ff1c3615: David chose option b, 2026-09-23).
  //
  // WHY THERE IS NO FORM. This route used to be the editor in create
  // mode, and its Save draft sent no `source`, which makes a PRODUCT
  // rule. The dispatcher's boot seed (boss-dispatcher rules::seed)
  // retires every active product rule no file under
  // infra/dispatcher/rules/ names, so the rule it made fired until the
  // next restart and its retirement showed only in a boot log. The draft
  // door now refuses that body server-side as well; this page is the
  // honest half: it names the two paths a rule survives a restart on.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { href } from '../router';

  // The references below are repository paths in plain text, not links:
  // the public mirror's address lives once, in infra/estate/estate.toml
  // (the_public_mirror_url_lives_once), and no in-app route serves docs.
</script>

<div class="catalog theme-exec">
  <Breadcrumb to={href('/it/registry/rules')}>← All dispatcher rules</Breadcrumb>
  <PageHeader
    eyebrow="Platform · Dispatcher rule"
    title="New dispatcher rule"
    subtitle="A rule is added by writing it down where it lasts, not on this page"
  />

  <div class="tab-grid">
    <Section title="Why this page has no form" wide>
      <p style="max-width:760px; line-height:1.5">
        Every time the dispatcher starts, it reads the product's rule files
        and retires any product rule that no file names. A rule created here
        would fire only until the next restart, then stop without notice. So
        a new rule starts in one of the two places below, and this registry
        takes it from there.
      </p>
    </Section>

    <Section title="A product rule — a file carried by a car" wide>
      <ol style="max-width:760px; line-height:1.5; padding-left:20px">
        <li>
          Add <code class="mono">infra/dispatcher/rules/&lt;name&gt;.toml</code>
          holding one <code class="mono">[[rule]]</code> whose
          <code class="mono">name</code> is the file's name. It needs a
          <code class="mono">why</code>, a trigger (<code class="mono">on_event</code>
          or <code class="mono">schedule</code>) and at least one
          <code class="mono">[[rule.do]]</code> handler.
        </li>
        <li>Gate it and park it like any change; it rides the next train.</li>
        <li>
          When the dispatcher next starts after the train lands, it publishes the
          rule at the version the file declares. It then appears in the list,
          where its page can validate, save and publish later versions, or retire it.
        </li>
      </ol>
      <p style="max-width:760px">
        Reference, in the repository: <code class="mono">infra/dispatcher/rules/README.md</code>
      </p>
    </Section>

    <Section title="A tenant's rule — its seeds/rules.toml" wide>
      <ol style="max-width:760px; line-height:1.5; padding-left:20px">
        <li>
          Add a <code class="mono">[[rule]]</code> to the tenant's
          <code class="mono">seeds/rules.toml</code>, in the same shape as a
          product rule file.
        </li>
        <li>
          Run <code class="mono">boss tenant publish</code>. The rule is recorded as
          the tenant's own (<code class="mono">source = tenant:&lt;id&gt;</code>), and
          a dispatcher restart never retires it.
        </li>
      </ol>
      <p style="max-width:760px">
        Reference, in the repository: <code class="mono">docs/tenant-contract.md</code>
        (the section on a tenant's own reactors)
      </p>
    </Section>
  </div>
</div>
