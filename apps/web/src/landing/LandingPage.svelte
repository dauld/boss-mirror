<script lang="ts">
  // Public unauth landing surface at `/`. A marketing hero over the
  // live System-Model view — Jobs in flight right now plus the
  // per-Workflow step-graph each Job is walking. The live view itself
  // is the SystemModelLiveView component (this page's own since the
  // perspective root); this page wraps it with the landing chrome
  // (hero + sign-in CTA).

  // EVERY CLAIM THIS PAGE MAKES IS MARKED (design 59a776c5). A
  // `data-claim` is a promise that src/marketing/claims.ts names the row
  // that answers it, and the web suite refuses when the two disagree
  // (src/marketing/the-landing-page-claims.test.ts, backlog e1524f57). Adding an assertion to this page means adding a mark and a
  // row; an UNMARKED assertion is invisible to the check, which is why
  // the checker also fingerprints the prose and says so when the words
  // move but the claim set does not.
  //
  // The live view below is exempt by construction: it is read from the
  // live registry, so it cannot drift from it.
  import SystemModelLiveView from './SystemModelLiveView.svelte';
</script>

<div class="landing">
  <header class="hero">
    <h1>BOSS</h1>
    <p class="tagline">Beer Open Source Software — for modeling systems as state machines.</p>
    <p class="lede subtagline">
      Helping the people who run a business operate it better —
      and making the basic software a business needs reachable
      for organizations that can't afford a bespoke enterprise
      stack.
    </p>
    <p class="lede">
      Below is a live window into
      <strong data-claim="tenant.name">Algedonic Ales</strong>,
      the brewery tenant that ships with BOSS — Jobs in flight
      right now, plus the step-graph workflow each Job is walking.
      The data is the live registry; pick a different workflow to
      see how BOSS models another part of the company.
    </p>
  </header>

  <SystemModelLiveView />

  <footer class="cta">
    <a class="cta-link" data-claim="cta.signin" href="/login">Sign in to operate the brewery →</a>
    <span class="docs">
      <a data-claim="source.repo" href="https://github.com/algedonic-dev/boss" target="_blank" rel="noopener">Source on GitHub</a>
    </span>
  </footer>
</div>

<style>
  .landing {
    max-width: 1100px;
    margin: 0 auto;
    padding: 1.5rem 1.5rem 3rem;
    color: #2a1d10;
    box-sizing: border-box;
    font-family: var(--font-display, 'Iowan Old Style', 'Palatino Linotype', Georgia, serif);
  }
  .hero {
    margin-bottom: 1.5rem;
  }
  .hero h1 {
    font-size: 2.6rem;
    margin: 0 0 0.25rem;
    font-weight: 800;
    letter-spacing: -0.01em;
  }
  .tagline {
    font-size: 1.25rem;
    margin: 0 0 1rem;
    color: #7a3f1f;
  }
  .lede {
    font-size: 1rem;
    line-height: 1.55;
    max-width: 60ch;
    color: #4a392b;
  }
  .cta {
    margin-top: 2rem;
    padding-top: 1rem;
    border-top: 1px solid #c5a880;
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    font-family: -apple-system, system-ui, sans-serif;
    font-size: 0.95rem;
  }
  .cta-link {
    color: #7a3f1f;
    text-decoration: none;
    font-weight: 600;
  }
  .cta-link:hover {
    text-decoration: underline;
  }
  .docs a {
    color: #7a6855;
    font-size: 0.85rem;
    text-decoration: none;
  }
  .docs a:hover {
    text-decoration: underline;
  }
</style>
