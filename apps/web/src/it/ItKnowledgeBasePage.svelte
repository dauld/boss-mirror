<script lang="ts">
  // /it/kb — IT Knowledge Base.
  //
  // The IT department's reference: the four architecture diagrams,
  // source-derived (Mermaid SVGs from docs/architecture/) so the
  // page doesn't drift the way a hardcoded hosts / stack /
  // providers table would. A prior iteration of this page carried
  // inline tables for those — they went out of alignment with
  // reality the moment any of the underlying state changed, so we
  // deleted them rather than maintain them by hand. The decision
  // record itself lives in the repo as
  // docs/architecture-decisions.md (one consolidated current-truth
  // document), not as an in-app catalog.
  // See `crates/core/boss-core/src/hosts.rs` for the operator host
  // registry (empty by design in OSS — operators name their own
  // hosts via `~/.config/boss/hosts.toml`).

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { href } from '../router';

  // Diagrams under it/kb-assets/ — the rendered Mermaid output
  // colocated with the page that consumes them. Regenerate via
  // `infra/architecture/regenerate.sh` (or follow
  // docs/architecture-diagram.md) and copy the SVGs back into
  // kb-assets/.
  import stateSurfacesWorkSvg from './kb-assets/00-state-surfaces-work.svg';
  import primitivesSvg from './kb-assets/01-primitives.svg';
  import serviceMapSvg from './kb-assets/02-service-map.svg';
  import deploymentSvg from './kb-assets/03-deployment.svg';
</script>

{#snippet diagram(src: string, alt: string)}
  <div class="arch-diagram">
    <img
      src={src}
      {alt}
      style="display:block; margin:0 auto; width:max(100%, 1600px); height:auto"
    />
  </div>
  <div style="font-size:12px; color:#78716c; margin-top:6px; text-align:right">
    <a href={src} target="_blank" rel="noopener noreferrer">Open at full size ↗</a>
  </div>
{/snippet}

<style>
  .layers {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-bottom: 16px;
  }
  /* Stacked bands, substrate at the bottom — the diagram IS the
     ordering, so it is CSS rather than a generated asset that could
     drift from the prose beside it. */
  .layer {
    display: flex;
    gap: 14px;
    align-items: flex-start;
    border: 1px solid #e7e5e4;
    border-radius: 8px;
    padding: 14px 16px;
    background: #fff;
  }
  .layer p {
    margin: 4px 0 0;
    font-size: 14px;
    line-height: 1.55;
    color: #44403c;
  }
  .layer strong { font-size: 15px; color: #1c1917; }
  .layer-n {
    flex: 0 0 28px;
    height: 28px;
    border-radius: 6px;
    display: grid;
    place-items: center;
    font-size: 13px;
    font-weight: 600;
    color: #1c1917;
  }
  .layer-actors   { border-left: 4px solid #a8a29e; }
  .layer-actors   .layer-n { background: #f5f5f4; }
  .layer-protocols { border-left: 4px solid #78716c; }
  .layer-protocols .layer-n { background: #e7e5e4; }
  .layer-network  { border-left: 4px solid #44403c; }
  .layer-network  .layer-n { background: #d6d3d1; }

  .arch-diagram {
    background: #fff;
    border: 1px solid #e7e5e4;
    border-radius: 8px;
    padding: 16px;
    overflow: auto;
    max-height: 75vh;
  }
</style>

<div class="catalog theme-it">
  <Breadcrumb to={href('/')}>← Home</Breadcrumb>

  <PageHeader
    eyebrow="System Model · Knowledge Base"
    title="Knowledge Base"
    subtitle="The reading frame, then the four architecture diagrams — the reference for how BOSS is put together"
  />

  <div style="background:#dbeafe; border:1px solid #bfdbfe; border-radius:8px; padding:14px 16px; margin-bottom:16px; font-size:14px; line-height:1.55; color:#1c1917">
    <strong style="display:block; margin-bottom:4px">What lives here</strong>
    The reading frame (§0–1) followed by the four architecture diagrams, rendered from
    <code>docs/architecture/*.mmd</code> on every diagram-regen — a
    source-derived reference that doesn't drift. The decision record
    lives in the repo as <code>docs/architecture-decisions.md</code>,
    one consolidated current-truth document. Hosts, software-stack
    tables, and provider lists used to live here too as inline
    literals — they consistently drifted and were removed.
    Tenant-specific operator infrastructure (host registry, SaaS
    integrations) is per-deployment data, not core BOSS reference.
  </div>

  <nav
    aria-label="Knowledge Base jump nav"
    style="display:flex; flex-wrap:wrap; gap:8px; padding:12px 16px; margin-bottom:8px; background:#fafaf9; border:1px solid #e7e5e4; border-radius:8px; font-size:13px"
  >
    <span style="color:#78716c; margin-right:4px">Jump to:</span>
    <a href="#it-layers"      style="color:#1c1917">0 · The three layers</a>
    <span style="color:#d6d3d1">·</span>
    <a href="#it-framing"     style="color:#1c1917">1 · Execution lens</a>
    <span style="color:#d6d3d1">·</span>
    <a href="#it-primitives"  style="color:#1c1917">2 · Primitives</a>
    <span style="color:#d6d3d1">·</span>
    <a href="#it-service-map" style="color:#1c1917">3 · Service map</a>
    <span style="color:#d6d3d1">·</span>
    <a href="#it-deployment"  style="color:#1c1917">4 · Deployment</a>
    <span style="color:#d6d3d1">·</span>
    <a href={href('/it/registry')} style="color:#1c1917">Workflows ↗</a>
  </nav>

  <div class="tab-content" style="display:flex; flex-direction:column; gap:24px; padding:16px 0">

    <section id="it-layers" class="tab-section tab-section-wide" style="scroll-margin-top:16px">
      <h3 style="margin-top:0">0. The reading frame — three layers</h3>
      <p class="prose" style="margin-bottom:16px">
        <strong>The network is the substrate. The fat protocols dictate the current
        operating model. The actors run it.</strong> Three layers, each replaceable
        without disturbing the others — which is what lets a company change how it
        works without rebuilding what it works <em>on</em>. Read every new workflow,
        page, or abstraction against this frame first. Canonical statement:
        <code>docs/design/the-three-layers.md</code>.
      </p>

      <div class="layers">
        <div class="layer layer-actors">
          <span class="layer-n">3</span>
          <div>
            <strong>The actors run it</strong>
            <p>
              Humans and registered agents are the CPUs — nothing moves without an
              actor claiming a step and doing it. Capability is enforced at the claim;
              bandwidth is finite, which is why stations hold rather than drop. Actors
              are not users of the system; they are the part of it that executes.
            </p>
          </div>
        </div>
        <div class="layer layer-protocols">
          <span class="layer-n">2</span>
          <div>
            <strong>The fat protocols dictate the current operating model</strong>
            <p>
              Workflows are protocols, and the meaning lives in the protocol row, not
              in the endpoints: the steps, the predicates that order them, the evidence
              each requires, the terminals, the obligations. That is why protocols are
              <em>registry data</em> — versioned, append-only, in-flight packets pinned
              to the version they were admitted under. <em>Current</em> is
              load-bearing: a protocol that cannot be replaced without a deploy has
              leaked into the substrate, and that leak is the defect to hunt.
            </p>
          </div>
        </div>
        <div class="layer layer-network">
          <span class="layer-n">1</span>
          <div>
            <strong>The network is the substrate</strong>
            <p>
              Packets (a Job is an immutable envelope plus a protocol set fixed at
              admission), stations (data-defined priority queues that route or hold),
              routes, the log, and the one admission edge. This layer is physics — it
              has no opinion about what the work means.
            </p>
          </div>
        </div>
      </div>
    </section>

    <section id="it-framing" class="tab-section tab-section-wide" style="scroll-margin-top:16px">
      <h3 style="margin-top:0">1. The execution lens — State · Surfaces · Work</h3>
      <p class="prose" style="margin-bottom:16px">
        This is the lens <em>over</em> the three layers, not the foundation: it answers
        "how does one packet get executed". <strong>State</strong> is the machine's
        memory (event log + projections). <strong>Surfaces</strong> are how CPUs —
        humans and agents — observe memory well enough to pick their next action.
        <strong>Work</strong> is the typed transitions those CPUs fire: Steps flipping
        to <code>done</code>, governed by policy, recorded as immutable events.
      </p>
      <p class="prose" style="margin-bottom:16px">
        Its invariants still hold — human and agent executors are CPUs in the same
        machine, and new work types are registry rows rather than bespoke core code
        paths. But when this reading and the network framing above disagree,
        <strong>the network framing wins</strong>, because it is the one that survives
        changing the operating model.
      </p>
      {@render diagram(stateSurfacesWorkSvg, 'State / Surfaces / Work framing')}
    </section>

    <section id="it-primitives" class="tab-section tab-section-wide" style="scroll-margin-top:16px">
      <h3 style="margin-top:0">2. Primitives &amp; cross-cutting abstractions</h3>
      <p class="prose" style="margin-bottom:16px">
        Four foundational primitives — <strong>Subject</strong> (the identity-bearing
        thing work is about), <strong>Job</strong> (a bounded unit of coordinated
        work), <strong>Step</strong> (a typed transition inside a Job), and
        <strong>Event</strong> (the immutable record of a state change, and the system
        of record) — cover every business entity worth modeling.
      </p>
      <p class="prose" style="margin-bottom:16px">
        <strong>Class</strong>, <strong>Part</strong>, <strong>Composite</strong>,
        Location and Reservation are <em>supporting</em> concepts that hang off those
        four — load-bearing infrastructure, not foundational vocabulary. Class is the
        data-driven taxonomy layer (roles, types, categories) so tenants extend without
        forking core. Cross-cutting rails (policy, ledger, messages) are called through
        typed client ports.
      </p>
      {@render diagram(primitivesSvg, 'Primitives and abstractions')}
    </section>

    <section id="it-service-map" class="tab-section tab-section-wide" style="scroll-margin-top:16px">
      <h3 style="margin-top:0">3. Service map (domains)</h3>
      <p class="prose" style="margin-bottom:16px">
        Every shipped service grouped by tier — core state-machine OS,
        company-modeling modules, cross-tier orchestrators, sim bridges,
        and the two example tenants. Services talk only through typed
        cross-service client crates; no direct DB access between services.
      </p>
      {@render diagram(serviceMapSvg, 'Service map')}
    </section>

    <section id="it-deployment" class="tab-section tab-section-wide" style="scroll-margin-top:16px">
      <h3 style="margin-top:0">4. Deployment topology</h3>
      <p class="prose" style="margin-bottom:16px">
        The reference single-VM topology BOSS ships with: gateway +
        primitive services + operational modules + cross-cutting rails
        + tenant engines + periodic timers, all systemd-managed against
        a single Postgres + NATS. Multi-VM splits, cloud-provider
        provisioning, and edge-CDN choice are per-tenant deployment
        questions, not core BOSS reference.
      </p>
      {@render diagram(deploymentSvg, 'Deployment topology')}
    </section>

  </div>
</div>
