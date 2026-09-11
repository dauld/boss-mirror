use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod brief;
mod built_from;
mod cadence;
mod car;
mod census;
mod channels;
mod credential;
mod delivery_policy;
mod deploy;
mod design;
mod dock_preview;
mod doctor;
mod envelope;
mod freshness;
mod gate;
mod git_auth;
mod host_readiness;
mod identity;
mod inspect;
mod job;
mod merged;
mod ops;
mod orient;
mod park;
mod prove;
mod publish;
mod publish_requests;
mod queue;
mod receipt;
mod rerail;
mod running;
mod script;
mod train;
mod upgrade;
mod workflow;

#[derive(Parser)]
#[command(name = "boss", about = "Boss operator + developer CLI", version = built_from::VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// `large_enum_variant` is allowed deliberately. This enum is the
/// PARSE of one command line: exactly one value of it exists per
/// process, built by clap at startup and matched once. `Gate` is the
/// widest variant because gating carries the full park intent a car
/// needs, and clippy's remedy — boxing its fields — would add an
/// indirection to every field for a one-off allocation no one measures.
/// The lint is about data structures held in quantity; a subcommand
/// parse is not one.
#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Post-install health check — verifies Postgres, NATS, gateway,
    /// tenant manifest, SPA bundle, and registered systemd services.
    Doctor,
    /// Emit an event to the local bus
    Emit {
        /// Event kind (e.g., "test.hello")
        kind: String,
        /// JSON payload
        #[arg(default_value = "{}")]
        payload: String,
    },
    /// Upgrade boss to the latest release
    Upgrade,
    /// CTO toolbox — list and inspect registered scripts
    Script {
        #[command(subcommand)]
        action: ScriptAction,
    },
    /// Build, install, and restart services
    Deploy {
        #[command(subcommand)]
        action: DeployAction,
    },
    /// Check health of all services, Postgres, NATS, and backups
    Status {
        /// Output as JSON (for Claude Code / machine parsing)
        #[arg(long)]
        json: bool,
    },
    /// Restart a service without rebuilding
    Restart {
        /// Service name (assets, catalog, people, commerce, etc.)
        service: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// View service logs via journalctl
    Logs {
        /// Service name
        service: String,
        /// Number of log lines to show
        #[arg(short = 'n', long, default_value = "50")]
        lines: u32,
        /// Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Trigger a manual backup (pg_dump + configs)
    Backup {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Asset maintenance subcommands
    Assets {
        #[command(subcommand)]
        action: AssetsAction,
    },
    /// Run the Boss simulator (thin wrapper around `boss-sim`)
    Sim {
        #[command(subcommand)]
        action: SimAction,
    },
    /// Ledger operations — rebuild the GL projection from financial_facts
    Ledger {
        #[command(subcommand)]
        action: LedgerAction,
    },
    /// Read-only diagnostic queries against the gateway's HTTP
    /// APIs. Replaces the `sudo -u postgres psql` muscle memory
    /// for diagnostic reads — raw SQL hides API gaps and the same
    /// muscle memory leads to raw SQL writes that bypass the
    /// audit_log + policy gate.
    Inspect {
        #[command(subcommand)]
        action: InspectAction,
    },
    /// PR-train conductor — drive the pr-train Workflow: reconcile
    /// open trains against reality, board this window's train. The
    /// systemd timers enter through `boss train run` (via
    /// infra/train/conductor.sh).
    Train {
        #[command(subcommand)]
        action: TrainAction,
    },
    /// Launch a gate for a branch — files or reuses the gate-run
    /// packet, renders the runner Job, and creates it.
    ///
    /// Replaces the seven-step by-hand sequence recorded in 51ca3405.
    /// Gates run in PARALLEL: every workspace is a per-run emptyDir
    /// seeded from a warm target, so verdicts are independent by
    /// construction. At the concurrency bound (BOSS_GATE_MAX_CONCURRENT,
    /// default 3) `--wait` QUEUES — the gate-run takes a place in line
    /// and launches when a slot frees, oldest first, so one call
    /// replaces a hand-rolled retry loop. Without `--wait` the bound
    /// refuses, naming the running gates and filing nothing.
    Gate {
        /// Branch to gate.
        branch: String,
        /// Gate mode: "auto" (or "--auto"), or "-p <crate>". Empty = full.
        ///
        /// Checked before the cluster is touched — an unknown mode is a
        /// refusal here, not a red gate forty minutes from now.
        #[arg(long)]
        mode: Option<String>,
        /// Runner manifest. Defaults to infra/gate-runner/gate-runner.yaml.
        #[arg(long)]
        manifest: Option<std::path::PathBuf>,
        /// Kubernetes namespace holding the gate Jobs.
        #[arg(long, default_value = "boss-dev")]
        namespace: String,
        /// Poll the gate-run packet until it reports a verdict.
        #[arg(long)]
        wait: bool,
        /// Show what would happen without filing or creating anything.
        #[arg(long)]
        dry_run: bool,
        /// Auto-park on green: what the change does (first sentence =
        /// title). Stamped onto the gate-run so the dispatcher files the
        /// car when the gate goes green — no hand-park. Requires the
        /// other three --park-* below (a car needs a full receipt) and
        /// ONE of --park-backlog-item / --park-partial-item /
        /// --park-no-item (a car says which item it fixes).
        #[arg(long)]
        park_summary: Option<String>,
        /// Auto-park: what the change deliberately leaves out.
        #[arg(long)]
        park_excludes: Option<String>,
        /// Auto-park: what was run, and what it proves.
        #[arg(long)]
        park_test: Option<String>,
        /// Auto-park: what was observed working beyond the gate.
        #[arg(long)]
        park_verified: Option<String>,
        /// Auto-park: the backlog item this change answers. This car IS
        /// that item's build, so the arrival rule routes its triage and
        /// COMPLETES its build when the car lands — which closes it.
        ///
        /// One of this, --park-partial-item or --park-no-item is
        /// REQUIRED with any --park-* flag. Measured 2026-09-10
        /// (e1325456): 13 of 19 open cars named no item, so their items
        /// stayed open after the fix was live and an operator closed
        /// them by hand with a worse record than the car's own arrival.
        #[arg(long)]
        park_backlog_item: Option<String>,
        /// Auto-park: the item this change is ONE PIECE of — recorded as
        /// provenance, NOT as the closing edge.
        ///
        /// For an item that is several separable pieces. `backlog_item`
        /// is one-to-one and the arrival rule closes what it names, so
        /// linking a multi-piece item to a car that is one piece closes
        /// it with work outstanding (cf0f5e2d was three pieces; held car
        /// f73828a9 was piece (1), piece (3) awaiting David's yes). This
        /// flag records which item the car belongs to and leaves the
        /// item open for its remaining pieces.
        #[arg(long)]
        park_partial_item: Option<String>,
        /// Auto-park: this car answers NO item, and why.
        ///
        /// The escape for the item-less cars that legitimately exist — a
        /// fix David asks for in conversation, a defect found while
        /// building something else. The reason is the point: it records
        /// which kind, so a later reader can tell a deliberate one from
        /// a forgotten one.
        #[arg(long, value_name = "REASON")]
        park_no_item: Option<String>,
        /// Auto-park: the CAR this one must land BEHIND. The dock will not
        /// board this car until that one has landed, and says so by name
        /// on every board attempt until it does.
        ///
        /// Recorded as the car's `boards_after` job edge — ref-checked at
        /// the write, so a mistyped id is refused here rather than
        /// becoming a hold nobody can clear. Use it for an ordering
        /// constraint the change itself creates: a car that must not share
        /// a consist with another, a two-part sequence whose halves must
        /// land in order. Measured 2026-09-10: four cars stated exactly
        /// this by a human holding the dock and re-reading it
        /// (d3320278) — `fix/the-reclaim-follows-the-build` was gated
        /// `--hold` and parked by hand purely so it would board solo.
        ///
        /// NOT the same tool as `--hold`, which still exists: a hold says
        /// "not yet" and needs a person to release it; an edge says "after
        /// THAT" and releases itself. A gate-blind change that must board
        /// SOLO is still the solo rule's business, not this flag's.
        #[arg(long, value_name = "CAR")]
        park_after: Option<String>,
        /// Auto-park: the probe that proves this change in production,
        /// written now by the builder who knows what it does. Recorded
        /// on the car as `proof_probe` and RUN — by `boss prove <car>
        /// --from-car`, or by the arrival rule once the car lands — in
        /// exactly the shape `boss prove` records. Needs --park-expect.
        ///
        /// WHERE IT RUNS: on the FORGE HOST, as david, in the converged
        /// checkout (/home/david/boss), with the forge's tools — NOT on
        /// the machine you are typing on. The forge is outside the
        /// cluster and holds no kubeconfig, so a `kubectl` probe is
        /// correct from the dev pod and unrunnable there (f9304366).
        /// Write it against something the forge can reach: the system
        /// of record over HTTP, its own journal, the checkout. A probe
        /// naming a tool in infra/forge/host-absent-tools.txt is
        /// refused here rather than at arrival.
        #[arg(long)]
        park_probe: Option<String>,
        /// Auto-park: the string the probe must print for the claim to
        /// hold (`proof_expect` on the car). Required with --park-probe.
        /// Judged on the forge host, where the probe runs. Make it a
        /// unique token, not a count: a number is compared as a whole
        /// token, but `test $(...) -eq 1 && echo claim:ok` asserts what
        /// you mean and cannot pass on the wrong number.
        #[arg(long)]
        park_expect: Option<String>,
        /// Auto-park: for a change only an EVENT can prove (a stalled
        /// train, a red gate, an operator's cancel) — say which event
        /// and how to prove it when it fires. Recorded as `proof_event`;
        /// the car stays hand-proven and the yard counts it as waiting.
        /// Never with --park-probe.
        #[arg(long)]
        park_proof_event: Option<String>,
        /// Gate a branch whose content ALREADY landed on main, stating
        /// why (e.g. to close a dead gate-run packet). Refused by
        /// default: a landed branch's next step is deletion, and a
        /// re-gate that inherits stale park intent files a twin car
        /// that boards an empty diff (610537b2). Never combines with
        /// --park-*.
        #[arg(long, value_name = "REASON")]
        force_regate: Option<String>,
        /// Gate a branch whose base is BEHIND origin/main, stating why
        /// (e.g. reproducing something that only happens on the old
        /// tree). Refused by default: a gate judges the branch's tree
        /// alone, so a green on a stale base vouches for a tree that
        /// will never exist — train #244 went red that way with two
        /// innocent cars aboard (2026-09-07). The reason is recorded on
        /// the gate-run as `stale_base_anyway`. The ordinary fix is
        /// `git rebase origin/main`, which the refusal prints.
        #[arg(long, value_name = "REASON")]
        stale_base_anyway: Option<String>,
        /// Gate and deliberately do NOT park: stamp `hold: <reason>` on
        /// the gate-run so its green reads HELD (in the yard, `boss
        /// orient` and the stranded-green alarm) rather than stranded.
        /// For a car that must land at a timed restart, or behind
        /// another car. Never combines with --park-*.
        #[arg(long, value_name = "REASON")]
        hold: Option<String>,
    },
    /// Park a gated branch as a car, carrying its receipt.
    ///
    /// Refuses unless the branch has a GREEN gate-run packet, and
    /// copies that packet's receipt verbatim into the car's gate step
    /// — the transcription step where a wrong head used to get in.
    Park {
        /// Branch to park. Must already have a green gate.
        branch: String,
        /// What the change does. Its first sentence becomes the title.
        #[arg(long)]
        summary: String,
        /// What it deliberately leaves out.
        #[arg(long)]
        excludes: String,
        /// What was run, and what it proves.
        #[arg(long)]
        test: String,
        /// What was observed working, beyond the gate being green.
        #[arg(long)]
        verified: String,
        /// Backlog item this change answers (a ref-checked job edge).
        #[arg(long)]
        backlog_item: Option<String>,
        /// Check the receipt and report, without filing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// A car's own lifecycle — the half that happens BEFORE the gate.
    ///
    /// Grouped like `Workflow` and `Job`: a new car verb lands inside
    /// `CarAction`, not as another variant here (84f9fbc0).
    Car {
        #[command(subcommand)]
        action: CarAction,
    },
    /// Protocol registry operations.
    ///
    /// Grouped deliberately: a new workflow verb lands inside
    /// `WorkflowAction` rather than adding another variant to this
    /// enum, which is the line every subcommand car contends on
    /// (84f9fbc0).
    Workflow {
        #[command(subcommand)]
        action: WorkflowAction,
    },
    /// Read, file, and patch packets — the curl-with-a-header this
    /// replaces was the most repeated manual act in the pipeline
    /// (~23/session, 514b39d8).
    ///
    /// Grouped like `Workflow`: a new job verb lands inside
    /// `JobAction`, not as another variant here (84f9fbc0).
    Job {
        #[command(subcommand)]
        action: JobAction,
    },
    /// A session's first verb: trains in transit, gates running,
    /// stranded greens, the dock, and the task queue — the approach in
    /// one read, with the startup checklist at the end (CLAUDE.md
    /// §Engineering Session Startup; automation of acedf981's L1).
    Orient {
        /// List every entry of a bounded section instead of the first
        /// twelve and "…and N more". The default is a bound, not a
        /// filter — the counts are always whole — but 34 of 46 orphans
        /// were unreadable from any surface until this flag (ce673e64).
        #[arg(long)]
        all: bool,
    },
    /// The handover a builder is dispatched with: the packet verbatim
    /// from the system of record, and the invariants derived from the
    /// files that decide them.
    ///
    /// Ten briefs in one session restated the same invariants from
    /// memory and the uid one was wrong in all ten (cc9ddc5d). A brief
    /// references this instead of retyping it; nothing printed here is
    /// a sentence somebody typed about the gate.
    Brief {
        /// The packet: its full uuid, 8+ characters of its id, or its
        /// branch. Omit it to print the invariants alone.
        packet: Option<String>,
    },
    /// Where the IT department's work comes from — the input-channel
    /// mix (user-feedback vs monitoring/error-discovery), the algedonic
    /// reading over recent work (docs/design/it-delivery-channels.md).
    Channels,
    /// A conflict-skipped car back aboard, with the traps encoded:
    /// new branch from current main (never a force-push), rebase with
    /// ONE human stop on a real conflict, gate, receipt machine-copied
    /// to regate_receipt, packet repointed and skip cleared (e7c86455).
    Rerail {
        /// The car: its branch, or 8+ characters of its id.
        car: String,
        /// Skip straight to transcription + repoint — the branch is
        /// already pushed and gated (the post-conflict path). With no
        /// `<branch>-rerail` on the forge there is nothing to rerail
        /// to, so this REFRESHES the car where it stands: the current
        /// green receipt is copied on and the stale skip cleared. That
        /// is the way to fix "gated, then changed" on an already
        /// re-railed or rebased car, and it refuses unless a green
        /// gate-run vouches for the branch's head right now.
        #[arg(long)]
        finish: bool,
        /// Report what would happen without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// File a design-doc packet in the shape the protocol can process.
    ///
    /// The `design-doc` workflow declares no metadata schema and its
    /// review step never mentions `questions` — the one field the
    /// protocol exists to process — so a malformed draft is admitted
    /// silently and reaches the reviewer with nothing to answer. This
    /// verb writes the working shape so nobody has to know it.
    Design {
        /// The doc's title.
        title: String,
        /// The doc body, markdown.
        #[arg(long, default_value = "")]
        markdown: String,
        /// An open question as `anchor|title|proposal`. Repeatable.
        #[arg(long = "question")]
        questions: Vec<String>,
        /// Record it without queuing a review — for a doc that states
        /// a decision already made.
        #[arg(long)]
        no_questions: bool,
        /// The docs/design path this packet mirrors, when there is one.
        #[arg(long)]
        doc_path: Option<String>,
    },
    /// Prove a merged car in production by RUNNING a probe.
    ///
    /// The verb executes the command itself and records exit status and
    /// output verbatim, refusing unless the probe exits zero and prints
    /// what was claimed. `proven` used to require only prose, which is
    /// how a change got reported done on an HTTP 204.
    Prove {
        /// Car to prove: its branch, or 8+ characters of its id.
        car: String,
        /// Command to run against production. Its exit and output are
        /// the evidence; it is recorded so it can be re-run later.
        #[arg(long)]
        probe: Option<String>,
        /// String the probe must print for the claim to hold. A bare
        /// number is compared as a WHOLE token (so `1` is not matched
        /// by `10`) and warns: have the probe assert internally and
        /// print a unique token instead.
        #[arg(long)]
        expect: Option<String>,
        /// Assert on the exit code alone, when the command IS the test
        /// (`grep -q`, `test -f`). Recorded, so a reader sees it.
        #[arg(long)]
        exit_only: bool,
        /// What the probe means, in prose, for a human reader.
        #[arg(long)]
        verified: Option<String>,
        /// How it was checked, if that needs saying.
        #[arg(long)]
        method: Option<String>,
        /// Re-run the proof already recorded and report whether it
        /// still holds. Read-only: records nothing.
        #[arg(long)]
        recheck: bool,
        /// Put a BETTER probe under a claim that is already proven,
        /// for a proof that was transient rather than wrong. The
        /// original stays on the step; the replacement is recorded
        /// beside it, because a proof that used to hold is evidence
        /// about the system rather than a mistake to erase (2b30eff4).
        #[arg(long)]
        replace: bool,
        /// Run the probe and report, without recording anything.
        #[arg(long)]
        dry_run: bool,
        /// Run the probe the car ITSELF recorded at park time
        /// (`--park-probe` / `--park-expect` on `boss gate`, copied to
        /// the car's `proof_probe` / `proof_expect`) instead of one
        /// given here. `--verified` defaults to the car's summary.
        /// Runs HERE, on this box — the arrival rule runs the same text
        /// on the forge host, so a probe can pass one place and be
        /// unrunnable in the other (f9304366).
        #[arg(long, conflicts_with_all = ["probe", "expect", "exit_only"])]
        from_car: bool,
        /// Run a probe this verb REFUSES, stating why.
        ///
        /// The refusal it escapes is the one rule `boss gate
        /// --park-probe` and this verb now share: a probe that reads the
        /// system of record with no identity is answered a NARROWER
        /// WORLD in silence, so an absence assertion passes falsely
        /// (61085a9e). The escape exists because a text check cannot see
        /// an identity supplied some other way — and it takes a REASON,
        /// recorded in the proof as `overridden`, because `--recheck`
        /// re-runs a recorded probe later when nobody is watching, and
        /// an override nobody can find is the same defect again.
        #[arg(long, value_name = "REASON")]
        probe_anyway: Option<String>,
    },
    /// Publish a branch to the forge, in one verb, and verify it.
    ///
    /// A workstation holds no forge credential, so the branch travels
    /// via the conductor's clone. Doing that by hand mangled a refspec
    /// on 2026-08-28 (zsh reads `$B:refs/...` as a `:r` modifier); a
    /// verb has no argv for a shell to eat.
    Publish {
        /// Branch to publish. Pushed from the current HEAD.
        branch: String,
        /// Git remote naming the conductor's clone.
        #[arg(long, default_value = "gcp")]
        remote: String,
        /// Show the two hops without pushing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Drain publish-request packets (workflow publish-request v1,
    /// filed 0b1b32f9): a workspace with no forge credential ships its
    /// branch as a bundle on a packet; this verb — run where the forge
    /// credential lives — verifies the bundle against the declared
    /// shas and pushes, or refuses with the reason on the packet.
    /// Never force-pushes. Also runs inside every `boss train run`,
    /// before reconcile/board, so a fresh branch can be gated the same
    /// cycle.
    PublishRequests {
        /// Clone to fetch and push in. Defaults to the working
        /// directory.
        #[arg(long, default_value = ".")]
        clone: String,
        /// Remote naming the forge in that clone. Remote names are
        /// per-clone: the conductor's branch-push remote is `fork`, a
        /// fresh clone's is `origin`.
        #[arg(long, default_value = "origin")]
        remote: String,
        /// List what would be drained without pushing or completing
        /// anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Read the feedback triage board from a terminal. Read-only on
    /// purpose: taking an item, annotating it, or closing it goes
    /// through the board or the step API, so every state change
    /// carries an actor.
    ///
    /// (This doc comment was once orphaned onto `Gate` by a car
    /// resolving a conflict at this enum's contended anchor — the
    /// 84f9fbc0 collision class — and `--help` described `gate` as
    /// the triage board. `tests::about_text_stays_with_its_verb`
    /// pins it in place now.)
    Queue {
        /// Column to show: all | waiting | with-agent |
        /// routed[:disposition] | done
        #[arg(default_value = "all")]
        column: String,
    },
    /// Job-packet network diagnostics (docs/design/packet-loss.md).
    Packet {
        #[command(subcommand)]
        action: PacketAction,
    },
    /// Query the audit log for domain events
    Audit {
        /// Filter by event kind prefix (e.g., "catalog.model")
        #[arg(long)]
        kind: Option<String>,
        /// Filter by source service (e.g., "catalog")
        #[arg(long)]
        source: Option<String>,
        /// Maximum entries to show
        #[arg(short = 'n', long, default_value = "20")]
        limit: u32,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    // ------------------------------------------------------------------
    // Per-module verbs — one `#[command(flatten)]` pair per verb, kept
    // ALPHABETIZED. A new top-level verb registers its own `Cmd` enum
    // (which carries the verb's doc comment and arguments) plus a
    // `dispatch` fn in its own module, and lands here as two lines at
    // its alphabetical position — one `mod` line, this pair, one match
    // arm in `main`. It does NOT add an inline variant above: inline
    // variants all insert at this enum's tail, so any two in-flight
    // subcommand cars conflict there and only one boards each train
    // (84f9fbc0). Single lines at distinct alphabetical positions merge
    // clean — the same measured behavior as the `mod` list. A verb that
    // belongs to an existing group still goes inside that group's
    // action enum (`WorkflowAction`, `JobAction`, ...), which touches
    // no shared line at all.
    // ------------------------------------------------------------------
    #[command(flatten)]
    Credential(credential::Cmd),
    #[command(flatten)]
    Merged(merged::Cmd),
    #[command(flatten)]
    Receipt(receipt::Cmd),
    #[command(flatten)]
    Running(running::Cmd),
}

#[derive(Subcommand)]
enum CarAction {
    /// Open the car for a branch at the START of its build.
    ///
    /// Files the `ship-a-change` packet at `opened`, declares the
    /// `scope` it was given, CLAIMS the `build` step, and records who is
    /// building, on which host, in which worktree, since when. A green
    /// then FINISHES this packet — `boss park` and the dispatcher's
    /// auto-park handler both adopt a car that is already building —
    /// so a branch has exactly ONE packet from its first minute.
    ///
    /// WHY (backlog be025b44): the packet used to be filed at GREEN,
    /// which is the END of the build. On 2026-09-09 four builders ran
    /// for 19–47 minutes each and the yard showed an empty dock
    /// throughout; on 2026-09-08 three builder sessions died mid-flight
    /// and nothing anywhere said so.
    Open {
        /// The branch whose build is starting.
        branch: String,
        /// What the change does. Its first sentence becomes the title.
        #[arg(long)]
        summary: String,
        /// What it deliberately leaves out. The load-bearing half: it
        /// is what keeps the car small enough to review.
        #[arg(long)]
        excludes: String,
        /// Backlog item this change answers (a ref-checked job edge).
        /// This car IS that item's build: the item is routed to `build`
        /// when the car opens — that is when the build starts — and the
        /// arrival rule CLOSES it when the car lands.
        ///
        /// Exactly ONE of this, --partial-item or --no-item is required.
        /// The same three answers `boss gate --park-*` demands, asked
        /// here so the answer is stated once, at build start, with the
        /// gate merely confirming it (90d291bb).
        #[arg(long)]
        backlog_item: Option<String>,
        /// The item this change is ONE PIECE of — recorded as
        /// provenance, NOT as the closing edge.
        ///
        /// For an item that is several separable pieces. `backlog_item`
        /// is one-to-one and the arrival rule closes what it names, so
        /// naming a multi-piece item there closes it with work
        /// outstanding. This records which item the car belongs to under
        /// a key no rule follows, and leaves the item open for the rest.
        #[arg(long)]
        partial_item: Option<String>,
        /// This car answers NO item, and why.
        ///
        /// The escape for the item-less cars that legitimately exist — a
        /// fix David asks for in conversation, a defect found while
        /// building something else. The reason is the answer: it records
        /// which kind, so a later reader can tell a deliberate one from
        /// a forgotten one.
        #[arg(long, value_name = "REASON")]
        no_item: Option<String>,
        /// Report what would be filed, without filing it.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum TrainAction {
    /// Prove the locomotive fit — clone owned by the running user,
    /// both remotes reachable — and exit. A sick locomotive exits 3,
    /// loud in the unit's status, instead of surfacing at departure.
    Preflight {
        /// Say what would happen without writing anywhere
        #[arg(long)]
        dry_run: bool,
    },
    /// Record evidence on open trains — the CI verdict, the merge
    /// (observed, never assumed), the deploys that carried it out.
    /// No boarding. This is the 10-minute early-warning cadence.
    Reconcile {
        /// Say what would happen without writing anywhere
        #[arg(long)]
        dry_run: bool,
    },
    /// Board this window's train without reconciling first: collect
    /// ready ship-a-change Jobs, assemble the train branch, open the
    /// one batched PR.
    Board {
        /// Say what would happen without writing anywhere
        #[arg(long)]
        dry_run: bool,
    },
    /// Reconcile open trains, then board this window's train (what
    /// the retired timers used to enter; now fired by the
    /// `train-window` cadence rule).
    Run {
        /// Say what would happen without writing anywhere
        #[arg(long)]
        dry_run: bool,
    },
    /// Cancel an open train that will not arrive: close its PR
    /// unmerged, release the boarded cars back to the dock (each one
    /// re-enters the next boarding, with the reason on its record),
    /// complete the train's `cancelled` terminal, and delete the
    /// train's own branch — never a car's.
    Cancel {
        /// The train's Job id (or a unique prefix), or its PR url
        train: String,
        /// Why — recorded on the cancelled step and every released car
        #[arg(long)]
        reason: String,
        /// Say what would happen without writing anywhere
        #[arg(long)]
        dry_run: bool,
    },
    /// The cadence loop: evaluate the cadence registry (read over
    /// the jobs API's `/api/cadence/*` door) against boss-clock time
    /// and fire the verbs the rules name, recording every firing
    /// through the same door. The supervised entry
    /// (infra/train/boss-train.service) — the schedule itself is
    /// protocol data (docs/design/protocol-cadence.md).
    Cadence {
        /// Evaluate one tick and exit (operator / test entry)
        #[arg(long)]
        once: bool,
        /// Say what would fire without claiming or running anything
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum PacketAction {
    /// Measure packet conservation across the whole instance and
    /// print what it found: open vs terminal counts by kind, the age
    /// and stall profile of open packets, how many stations would
    /// present each one (zero = ORPHANED — structurally unworkable),
    /// and whether declared `job_edges` still point at Jobs that
    /// exist.
    ///
    /// Read-only and non-raising by design: it files no job, sends no
    /// message, repairs nothing, and exits 0 whatever it finds —
    /// packet-loss.md Q2 defers raising until the base rate this
    /// measures is known.
    Census {
        /// Days without step motion before a packet counts stalled.
        #[arg(long, default_value = "7", value_parser = clap::value_parser!(i64).range(1..))]
        stale_days: i64,
        /// Output as JSON (for jq / a cadence rule recording a series).
        #[arg(long)]
        json: bool,
        /// Cap on open packets evaluated. The default covers today's
        /// volume with room to spare; a truncating run says so in the
        /// output rather than sampling silently.
        #[arg(long, default_value = "2000")]
        max_open: usize,
        /// Cap on Jobs read when resolving edge references that are id
        /// PREFIXES (those need the full id universe — see the module
        /// docs). 0 skips the scan and reports those refs unknown.
        #[arg(long, default_value = "20000")]
        max_scan: usize,
        /// The jobs-api URL. Falls back to BOSS_JOBS_URL; with neither
        /// set the verb refuses rather than guess an instance —
        /// boss-gcp's 127.0.0.1:7900 is a different, older deployment.
        #[arg(long)]
        jobs_url: Option<String>,
    },
}

#[derive(Subcommand)]
enum DeployAction {
    /// List all deployable services
    List,
    /// Deploy a service (or all if no service specified)
    Run {
        /// Service name (e.g. assets, shipping, gateway). Omit for all.
        service: Option<String>,
        /// Skip cargo build (install existing binary only)
        #[arg(long)]
        skip_build: bool,
    },
    /// Build and deploy the web frontend
    Web,
    /// Remove debug build artifacts to free disk space
    Clean,
}

#[derive(Subcommand)]
enum AssetsAction {
    /// Rebuild the `systems` projection table from the `system_events` log.
    /// Idempotent — safe to run on a healthy DB.
    RebuildProjection {
        /// Postgres URL. Defaults to the local assets service DB.
        #[arg(long, default_value = "postgres://boss:boss@127.0.0.1/boss")]
        postgres_url: String,
    },
}

#[derive(Subcommand)]
enum SimAction {
    /// Replay a simulation config against the live service APIs.
    /// Shells out to the installed `boss-sim` binary.
    Replay {
        /// Path to TOML config file (required)
        #[arg(short, long)]
        config: std::path::PathBuf,

        /// Override catalog JSON path from config
        #[arg(long)]
        catalog: Option<std::path::PathBuf>,

        /// API base URL (gateway or direct service)
        #[arg(long, default_value = "http://127.0.0.1:4443")]
        api_url: String,

        /// Live mode: each simulated day posts through real write APIs
        #[arg(long, default_value_t = false)]
        live: bool,
    },
}

#[derive(Subcommand)]
enum LedgerAction {
    /// Rebuild journal entries for every open period. Locked periods are
    /// never touched — their pinned rule version keeps them stable.
    /// Idempotent: running it twice produces the same projection.
    Rebuild {
        /// Postgres URL. Defaults to the local Boss DB.
        #[arg(long, default_value = "postgres://boss:boss@127.0.0.1/boss")]
        postgres_url: String,
        /// Output as JSON (for machine parsing)
        #[arg(long)]
        json: bool,
    },
    /// List all periods with their status + totals.
    Periods {
        #[arg(long, default_value = "postgres://boss:boss@127.0.0.1/boss")]
        postgres_url: String,
        #[arg(long)]
        json: bool,
    },
    /// Lock a period by starting date (YYYY-MM-DD). Pins the active rule
    /// version and writes a checksum. Rejects further writes to that
    /// period until unlocked.
    Lock {
        /// Period starts_on date (e.g. 2026-03-01)
        starts_on: String,
        #[arg(long, default_value = "postgres://boss:boss@127.0.0.1/boss")]
        postgres_url: String,
        /// Identifier recorded as who locked the period.
        #[arg(long, default_value = "operator")]
        locked_by: String,
    },
    /// Unlock a period by starting date. Clears lock fields and returns
    /// status to 'open'. Operator-tier action.
    Unlock {
        starts_on: String,
        #[arg(long, default_value = "postgres://boss:boss@127.0.0.1/boss")]
        postgres_url: String,
    },
}

#[derive(Subcommand)]
enum InspectAction {
    /// List invoices, optionally filtered by status / account_id.
    Invoices {
        /// Filter by status (outstanding | paid | past-due).
        #[arg(long)]
        status: Option<String>,
        /// Filter by account_id (exact match).
        #[arg(long)]
        account_id: Option<String>,
        /// Maximum entries to show.
        #[arg(short = 'n', long, default_value = "20")]
        limit: u32,
        /// Output as JSON (for jq / scripts).
        #[arg(long)]
        json: bool,
        /// Override the gateway URL. Defaults to BOSS_GATEWAY_URL
        /// or http://127.0.0.1:4443.
        #[arg(long)]
        gateway_url: Option<String>,
    },
    /// List accounts, optionally filtered by name substring.
    Accounts {
        /// Case-insensitive substring match against account name.
        #[arg(long, value_name = "NEEDLE")]
        name: Option<String>,
        #[arg(short = 'n', long, default_value = "20")]
        limit: u32,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        gateway_url: Option<String>,
    },
    /// List jobs, optionally filtered by status / kind / account_id.
    Jobs {
        /// Filter by status (open | closed | blocked | ...).
        #[arg(long)]
        status: Option<String>,
        /// Filter by Workflow (e.g. morning-brew, wholesale-keg-order).
        #[arg(long)]
        kind: Option<String>,
        /// Filter by account_id.
        #[arg(long)]
        account_id: Option<String>,
        #[arg(short = 'n', long, default_value = "20")]
        limit: u32,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        gateway_url: Option<String>,
    },
    /// List employees, optionally filtered by role.
    Employees {
        /// Filter by exact role code (e.g. ceo, cto, head-brewer).
        #[arg(long)]
        role: Option<String>,
        #[arg(short = 'n', long, default_value = "20")]
        limit: u32,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        gateway_url: Option<String>,
    },
}

#[derive(Subcommand)]
enum WorkflowAction {
    /// Publish a protocol version through the whole safe sequence:
    /// lint, refuse a dirty registry, create the draft and read it
    /// back, publish, then confirm what actually went live.
    Publish {
        /// Workflow kind, e.g. `ship-a-change`.
        kind: String,
        /// The spec to publish: a JSON file, a workflow bundle file
        /// (`*.toml`), a single kind file
        /// (infra/platform/workflows/<kind>.toml), or the bundle
        /// directory itself (infra/platform/workflows) — the `kind`
        /// row is taken from it, one definition for seed and publish.
        spec: std::path::PathBuf,
        /// Lint and report without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Discard a DRAFT version outright — the resolution the publish
    /// guard's "resolve that draft first" refusal names (ebd7bb70).
    /// Draft-only: an active or retired version is history and refuses.
    Discard {
        /// Workflow kind, e.g. `ship-a-change`.
        kind: String,
        /// The draft version to remove.
        version: i32,
    },
}

#[derive(Subcommand)]
enum JobAction {
    /// One packet in a stable shape: kind and protocol version, every
    /// step by slug/kind/status/holder/authority with the ready one
    /// marked, the gate receipt if it carries one, and what metadata
    /// keys exist. An ambiguous reference LISTS the matches.
    Get {
        /// Full uuid, 8+ characters of the id, or the car's branch.
        job: String,
        /// Print the raw API body instead.
        #[arg(long)]
        json: bool,
    },
    /// What is queued at a station: the station's discipline and WIP
    /// limit, its real depth next to this page's size, then one line
    /// per packet. (`boss queue` is the feedback triage board, a
    /// different thing.)
    Station {
        /// Station name, e.g. `loading-dock`, `q.platform-admin.task`.
        station: String,
        /// Print the normalized rows as JSON for a machine reader.
        #[arg(long)]
        json: bool,
    },
    /// One line per job: short id, kind, status, title.
    List {
        /// Filter by kind, e.g. `backlog-item`.
        #[arg(long)]
        kind: Option<String>,
        /// Job status to list (default open).
        #[arg(long, default_value = "open")]
        status: String,
        /// Max rows (default 50).
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Create a packet with the whole envelope defaulted — no more
    /// one-422-per-missing-field guessing. Confirms by reading the
    /// created packet back; a 201 is not proof.
    File {
        /// Workflow kind, e.g. `backlog-item`.
        #[arg(long)]
        kind: String,
        /// The packet's title.
        #[arg(long)]
        title: String,
        /// standard | urgent (default standard).
        #[arg(long)]
        priority: Option<String>,
        /// JSON file for the packet's metadata.
        #[arg(long)]
        metadata: Option<std::path::PathBuf>,
        /// Subject id (default bosspipeline; kind is always custom).
        #[arg(long)]
        subject_id: Option<String>,
    },
    /// Merge keys into a packet's metadata (null removes a key), then
    /// read it back and FAIL unless every key actually took — a 204
    /// here can be a silent no-op, and has been twice.
    Patch {
        /// Full uuid, 8+ characters of the id, or the car's branch.
        job: String,
        /// JSON object file: key -> value, null to remove.
        patch: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
enum ScriptAction {
    /// List registered scripts
    List {
        /// Filter by category (scraper, monitor, health-check, maintenance)
        #[arg(long)]
        category: Option<String>,
    },
    /// Show details for a script
    Info {
        /// Script ID (e.g., fda-510k-scraper)
        id: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Doctor => doctor::run_install().await,
        Commands::Emit { kind, payload } => cmd_emit(kind, payload).await,
        Commands::Upgrade => upgrade::run().await,
        Commands::Script { action } => match action {
            ScriptAction::List { category } => script::list(category.as_deref()).await,
            ScriptAction::Info { id } => script::info(&id).await,
        },
        Commands::Deploy { action } => match action {
            DeployAction::List => deploy::list().await,
            DeployAction::Run {
                service,
                skip_build,
            } => deploy::run(service.as_deref(), skip_build).await,
            DeployAction::Web => deploy::deploy_web().await,
            DeployAction::Clean => deploy::clean().await,
        },
        Commands::Status { json } => ops::status(json).await,
        Commands::Restart { service, json } => ops::restart(&service, json).await,
        Commands::Logs {
            service,
            lines,
            follow,
            json,
        } => ops::logs(&service, lines, follow, json).await,
        Commands::Backup { json } => ops::backup(json).await,
        Commands::Assets { action } => match action {
            AssetsAction::RebuildProjection { postgres_url } => {
                cmd_assets_rebuild_projection(&postgres_url).await
            }
        },
        Commands::Ledger { action } => match action {
            LedgerAction::Rebuild { postgres_url, json } => {
                cmd_ledger_rebuild(&postgres_url, json).await
            }
            LedgerAction::Periods { postgres_url, json } => {
                cmd_ledger_periods(&postgres_url, json).await
            }
            LedgerAction::Lock {
                starts_on,
                postgres_url,
                locked_by,
            } => cmd_ledger_lock(&postgres_url, &starts_on, &locked_by).await,
            LedgerAction::Unlock {
                starts_on,
                postgres_url,
            } => cmd_ledger_unlock(&postgres_url, &starts_on).await,
        },
        Commands::Inspect { action } => match action {
            InspectAction::Invoices {
                status,
                account_id,
                limit,
                json,
                gateway_url,
            } => {
                let gw = inspect::resolve_gateway_url(gateway_url.as_deref());
                inspect::invoices(status.as_deref(), account_id.as_deref(), limit, json, &gw).await
            }
            InspectAction::Accounts {
                name,
                limit,
                json,
                gateway_url,
            } => {
                let gw = inspect::resolve_gateway_url(gateway_url.as_deref());
                inspect::accounts(name.as_deref(), limit, json, &gw).await
            }
            InspectAction::Jobs {
                status,
                kind,
                account_id,
                limit,
                json,
                gateway_url,
            } => {
                let gw = inspect::resolve_gateway_url(gateway_url.as_deref());
                inspect::jobs(
                    status.as_deref(),
                    kind.as_deref(),
                    account_id.as_deref(),
                    limit,
                    json,
                    &gw,
                )
                .await
            }
            InspectAction::Employees {
                role,
                limit,
                json,
                gateway_url,
            } => {
                let gw = inspect::resolve_gateway_url(gateway_url.as_deref());
                inspect::employees(role.as_deref(), limit, json, &gw).await
            }
        },
        Commands::Train { action } => {
            // The cadence loop reads boss-clock time itself (via
            // ClockClient) — it never takes a wallclock argument.
            if let TrainAction::Cadence { once, dry_run } = action {
                return cadence::run(once, dry_run).await;
            }
            let (phase, dry) = match action {
                TrainAction::Preflight { dry_run } => (train::Phase::Preflight, dry_run),
                TrainAction::Reconcile { dry_run } => (train::Phase::Reconcile, dry_run),
                TrainAction::Board { dry_run } => (train::Phase::Board, dry_run),
                TrainAction::Run { dry_run } => (train::Phase::Run, dry_run),
                TrainAction::Cancel {
                    train,
                    reason,
                    dry_run,
                } => (
                    train::Phase::Cancel {
                        handle: train,
                        reason,
                    },
                    dry_run,
                ),
                TrainAction::Cadence { .. } => unreachable!("handled above"),
            };
            // Wall-clock at the CLI boundary: the train window IS the
            // operator's now (the verb entry a person or the cadence
            // loop fires), and nothing here stamps audit_log directly
            // — jobs-api does that on the far side of the HTTP calls.
            train::run(phase, dry, chrono::Utc::now()).await
        }
        Commands::Park {
            branch,
            summary,
            excludes,
            test,
            verified,
            backlog_item,
            dry_run,
        } => {
            park::run(
                &branch,
                &summary,
                &excludes,
                &test,
                &verified,
                backlog_item,
                dry_run,
                chrono::Utc::now(),
            )
            .await
        }
        Commands::Car { action } => match action {
            CarAction::Open {
                branch,
                summary,
                excludes,
                backlog_item,
                partial_item,
                no_item,
                dry_run,
            } => {
                car::open(
                    &branch,
                    &summary,
                    &excludes,
                    car::ItemAnswer {
                        backlog_item,
                        partial_item,
                        no_item,
                    },
                    dry_run,
                    chrono::Utc::now(),
                )
                .await
            }
        },
        Commands::Workflow { action } => match action {
            WorkflowAction::Publish {
                kind,
                spec,
                dry_run,
            } => workflow::publish(&kind, &spec, dry_run).await,
            WorkflowAction::Discard { kind, version } => workflow::discard(&kind, version).await,
        },
        Commands::Job { action } => match action {
            JobAction::Get { job, json } => job::get(&job, json).await,
            JobAction::Station { station, json } => job::station(&station, json).await,
            JobAction::List {
                kind,
                status,
                limit,
            } => job::list(kind, status, limit).await,
            JobAction::File {
                kind,
                title,
                priority,
                metadata,
                subject_id,
            } => {
                job::file(
                    &kind,
                    &title,
                    priority,
                    metadata,
                    subject_id,
                    chrono::Utc::now(),
                )
                .await
            }
            JobAction::Patch { job, patch } => job::patch(&job, &patch).await,
        },
        Commands::Orient { all } => orient::run(all).await,
        Commands::Brief { packet } => brief::run(packet).await,
        Commands::Channels => channels::run().await,
        Commands::Design {
            title,
            markdown,
            questions,
            no_questions,
            doc_path,
        } => {
            design::run(
                title,
                markdown,
                questions,
                no_questions,
                doc_path,
                chrono::Utc::now(),
            )
            .await
        }

        Commands::Rerail {
            car,
            finish,
            dry_run,
        } => rerail::run(&car, finish, dry_run, chrono::Utc::now()).await,
        Commands::Prove {
            car,
            probe,
            expect,
            exit_only,
            verified,
            method,
            recheck,
            replace,
            dry_run,
            from_car,
            probe_anyway,
        } => {
            prove::run(
                &car,
                probe,
                expect,
                exit_only,
                verified,
                method,
                recheck,
                replace,
                dry_run,
                from_car,
                probe_anyway,
                chrono::Utc::now(),
            )
            .await
        }
        Commands::Publish {
            branch,
            remote,
            dry_run,
        } => publish::run(&branch, &remote, dry_run).await,
        Commands::PublishRequests {
            clone,
            remote,
            dry_run,
        } => {
            // Wall-clock at the CLI boundary: the completion stamp this
            // verb writes derives from the operator's now.
            publish_requests::run(&clone, &remote, dry_run, chrono::Utc::now()).await
        }
        Commands::Gate {
            branch,
            mode,
            manifest,
            namespace,
            wait,
            dry_run,
            park_summary,
            park_excludes,
            park_test,
            park_verified,
            park_backlog_item,
            park_partial_item,
            park_no_item,
            park_after,
            park_probe,
            park_expect,
            park_proof_event,
            force_regate,
            stale_base_anyway,
            hold,
        } => {
            let park = gate::ParkIntent {
                summary: park_summary,
                excludes: park_excludes,
                test: park_test,
                verified: park_verified,
                backlog_item: park_backlog_item,
                partial_item: park_partial_item,
                no_item: park_no_item,
                boards_after: park_after,
                probe: park_probe,
                expect: park_expect,
                proof_event: park_proof_event,
            };
            gate::run(
                &branch,
                mode,
                manifest,
                &namespace,
                wait,
                dry_run,
                park,
                force_regate,
                stale_base_anyway,
                hold,
                // Wall-clock at the CLI boundary, minted once: the queue's
                // ordering key and its heartbeat are real elapsed time on
                // a real build node, not a business date. The waiting loop
                // carries this forward with monotonic elapsed time rather
                // than reading the clock again.
                chrono::Utc::now(),
            )
            .await
        }
        Commands::Queue { column } => queue::run(&column).await,
        Commands::Packet { action } => match action {
            PacketAction::Census {
                stale_days,
                json,
                max_open,
                max_scan,
                jobs_url,
            } => {
                // Wall-clock at the CLI boundary: the census's `now`
                // IS the operator's now, and it stamps nothing — every
                // call it makes is a GET.
                census::run(
                    census::Options {
                        stale_days,
                        json,
                        max_open,
                        max_scan,
                        jobs_url,
                    },
                    chrono::Utc::now(),
                )
                .await
            }
        },
        Commands::Audit {
            kind,
            source,
            limit,
            json,
        } => ops::audit(kind.as_deref(), source.as_deref(), limit, json).await,
        Commands::Sim { action } => match action {
            SimAction::Replay {
                config,
                catalog,
                api_url,
                live,
            } => cmd_sim_replay(&config, catalog.as_deref(), &api_url, live).await,
        },
        // Per-module verbs, one arm each, ALPHABETIZED — the note on
        // `Commands` says why (84f9fbc0).
        Commands::Credential(cmd) => credential::dispatch(cmd).await,
        Commands::Merged(cmd) => merged::dispatch(cmd),
        Commands::Receipt(cmd) => receipt::dispatch(cmd).await,
        Commands::Running(cmd) => running::dispatch(cmd),
    }
}

/// Thin wrapper: forward every flag to the installed `boss-sim`
/// binary and inherit stdio so progress lines stream live.
async fn cmd_sim_replay(
    config: &std::path::Path,
    catalog: Option<&std::path::Path>,
    api_url: &str,
    live: bool,
) -> Result<()> {
    let mut cmd = std::process::Command::new("boss-sim");
    cmd.arg("replay")
        .arg("--config")
        .arg(config)
        .arg("--api-url")
        .arg(api_url);
    if let Some(c) = catalog {
        cmd.arg("--catalog").arg(c);
    }
    if live {
        cmd.arg("--live");
    }
    let status = cmd
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn boss-sim: {e}. Is `boss-sim` on PATH?"))?;
    if !status.success() {
        anyhow::bail!("boss-sim exited with status {status}");
    }
    Ok(())
}

async fn cmd_ledger_rebuild(postgres_url: &str, json: bool) -> Result<()> {
    use sqlx::postgres::PgPoolOptions;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(postgres_url)
        .await?;

    if !json {
        println!("Rebuilding GL projection from financial_facts (open periods only)...");
    }
    let report = boss_ledger::rebuild(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("rebuild failed: {e}"))?;

    if json {
        let out = serde_json::json!({
            "facts_processed": report.facts_processed,
            "entries_dropped": report.entries_dropped,
            "entries_created": report.entries_created,
            "periods_rebuilt": report.periods_rebuilt,
            "total_debits": report.total_debits.to_string(),
            "total_credits": report.total_credits.to_string(),
            "balanced": report.is_balanced(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "  {} facts processed → {} entries created ({} dropped, {} periods rebuilt)",
            report.facts_processed,
            report.entries_created,
            report.entries_dropped,
            report.periods_rebuilt,
        );
        println!(
            "  trial balance: debits=${} credits=${}  {}",
            report.total_debits,
            report.total_credits,
            if report.is_balanced() {
                "BALANCED"
            } else {
                "MISMATCH"
            },
        );
    }

    if !report.is_balanced() {
        anyhow::bail!(
            "trial balance mismatch: debits={} credits={}",
            report.total_debits,
            report.total_credits
        );
    }
    Ok(())
}

async fn cmd_ledger_periods(postgres_url: &str, json: bool) -> Result<()> {
    use sqlx::postgres::PgPoolOptions;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(postgres_url)
        .await?;
    let periods = boss_ledger::periods::list_periods(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("list_periods: {e}"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&periods)?);
    } else {
        println!(
            "{:<12}  {:<8}  {:>8}  {:>14}  {:>14}  LOCKED_BY",
            "STARTS_ON", "STATUS", "ENTRIES", "DEBITS", "CREDITS"
        );
        println!("{}", "-".repeat(80));
        for p in &periods {
            println!(
                "{:<12}  {:<8}  {:>8}  {:>14}  {:>14}  {}",
                p.starts_on,
                p.status,
                p.entry_count,
                p.total_debits,
                p.total_credits,
                p.locked_by.as_deref().unwrap_or("-")
            );
        }
    }
    Ok(())
}

async fn cmd_ledger_lock(postgres_url: &str, starts_on: &str, locked_by: &str) -> Result<()> {
    use sqlx::postgres::PgPoolOptions;
    let date: chrono::NaiveDate = starts_on
        .parse()
        .map_err(|e| anyhow::anyhow!("bad date `{starts_on}`: {e}"))?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(postgres_url)
        .await?;
    let id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM gl_periods WHERE kind = 'month' AND starts_on = $1")
            .bind(date)
            .fetch_optional(&pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no period with starts_on={starts_on}"))?;
    let stamp = boss_core::publisher::EventStamp::new(
        "ledger",
        boss_core::actor::ActorId::Automation("operator-cli".into()),
    );
    let checksum = boss_ledger::periods::lock_period(&pool, id, locked_by, &stamp, locked_by)
        .await
        .map_err(|e| anyhow::anyhow!("lock_period: {e}"))?;
    println!("locked period {starts_on} — {checksum}");
    Ok(())
}

async fn cmd_ledger_unlock(postgres_url: &str, starts_on: &str) -> Result<()> {
    use sqlx::postgres::PgPoolOptions;
    let date: chrono::NaiveDate = starts_on
        .parse()
        .map_err(|e| anyhow::anyhow!("bad date `{starts_on}`: {e}"))?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(postgres_url)
        .await?;
    let id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM gl_periods WHERE kind = 'month' AND starts_on = $1")
            .bind(date)
            .fetch_optional(&pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no period with starts_on={starts_on}"))?;
    let stamp = boss_core::publisher::EventStamp::new(
        "ledger",
        boss_core::actor::ActorId::Automation("operator-cli".into()),
    );
    boss_ledger::periods::unlock_period(&pool, id, &stamp, "operator-cli")
        .await
        .map_err(|e| anyhow::anyhow!("unlock_period: {e}"))?;
    println!("unlocked period {starts_on}");
    Ok(())
}

async fn cmd_assets_rebuild_projection(postgres_url: &str) -> Result<()> {
    use sqlx::postgres::PgPoolOptions;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(postgres_url)
        .await?;
    let assets = boss_assets::PgAssets::new(pool);
    println!("Rebuilding systems projection from system_events log...");
    let written = assets
        .rebuild_projection()
        .await
        .map_err(|e| anyhow::anyhow!("rebuild failed: {e}"))?;
    println!("Wrote {written} rows.");
    Ok(())
}

async fn cmd_emit(kind: String, payload: String) -> Result<()> {
    let payload: serde_json::Value = serde_json::from_str(&payload)?;
    // CLI one-off — boundary tool that builds an Event for stdout
    // inspection (not for publishing). Wall-clock at the boundary
    // matches the operator's `now`; nothing reads this event.
    let event = boss_core::event::Event::new("cli", kind, payload, chrono::Utc::now());
    println!("{}", serde_json::to_string_pretty(&event)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// clap's own structural validation of the whole command tree —
    /// catches a misconfigured `#[command(flatten)]`, a duplicate verb
    /// name, or a broken arg definition at test time instead of at
    /// first invocation.
    #[test]
    fn command_tree_is_valid() {
        Cli::command().debug_assert();
    }

    /// Every top-level verb resolves. This is the smoke contract for
    /// the per-module registration pattern (84f9fbc0): moving a
    /// variant out of `Commands` into its module's `Cmd` enum must not
    /// change what `boss --help` offers.
    #[test]
    fn every_verb_still_resolves() {
        let cmd = Cli::command();
        let names: Vec<&str> = cmd.get_subcommands().map(|c| c.get_name()).collect();
        for expected in [
            "doctor", "emit", "upgrade", "script", "deploy", "status", "restart", "logs", "backup",
            "assets", "sim", "ledger", "inspect", "train", "gate", "park", "merged", "receipt",
            "running", "workflow", "job", "prove", "publish", "queue", "packet", "audit",
        ] {
            assert!(
                names.contains(&expected),
                "subcommand `{expected}` missing from the CLI; present: {names:?}"
            );
        }
    }

    /// The two queue-shaped surfaces are different things, and stay
    /// different. `boss queue` is the FEEDBACK TRIAGE BOARD (columns:
    /// waiting / with-agent / routed / done); `boss job station <name>`
    /// is a station's packet queue. Backlog d44f6152 proposed the
    /// station reader as `boss queue <station>`, which would have
    /// silently changed what the existing verb answers — a reader asking
    /// one and getting the other is the same class of confident wrong
    /// answer the verb exists to end. Pinned so neither absorbs the
    /// other, and so the station reader cannot be orphaned out of
    /// `JobAction` by a conflict resolution (84f9fbc0).
    #[test]
    fn the_station_reader_lives_under_job_and_the_feedback_board_keeps_queue() {
        let cmd = Cli::command();
        let job = cmd
            .get_subcommands()
            .find(|c| c.get_name() == "job")
            .expect("no `job` subcommand");
        let under_job: Vec<&str> = job.get_subcommands().map(|c| c.get_name()).collect();
        for expected in ["get", "station", "list", "file", "patch"] {
            assert!(
                under_job.contains(&expected),
                "`boss job {expected}` is gone; present: {under_job:?}"
            );
        }
        // `station` is NOT a top-level verb: a new packet-read verb
        // belongs inside this grouping, which touches no contended line.
        let top: Vec<&str> = cmd.get_subcommands().map(|c| c.get_name()).collect();
        assert!(!top.contains(&"station"), "present: {top:?}");
        assert!(
            !top.contains(&"show"),
            "`boss job get` already reads a packet — a second way to read one is the \
             thing d44f6152 asked us NOT to add; present: {top:?}"
        );
    }

    /// Each verb owns its own about-text. Pinned because a car
    /// inserting `Gate` at the contended enum anchor (2158ec9 →
    /// 2026-08-27 window) orphaned `Queue`'s doc comment on top of
    /// `Gate`'s — clap concatenated them, `boss --help` described
    /// `gate` as the feedback triage board, and `queue` had no
    /// about-text at all. The scar is exactly the collision class
    /// 84f9fbc0 measures; this test fails if it re-forms.
    #[test]
    fn about_text_stays_with_its_verb() {
        let cmd = Cli::command();
        let about = |name: &str| -> String {
            cmd.get_subcommands()
                .find(|c| c.get_name() == name)
                .unwrap_or_else(|| panic!("no `{name}` subcommand"))
                .get_about()
                .map(|s| s.to_string())
                .unwrap_or_default()
        };
        let cases = [
            ("gate", "Launch a gate for a branch"),
            ("queue", "Read the feedback triage board"),
            ("merged", "Did this branch actually land on main?"),
            (
                "receipt",
                "What does this car's gate receipt actually vouch for?",
            ),
            (
                "running",
                "Merged, deployed and running are three different facts",
            ),
        ];
        for (name, prefix) in cases {
            let got = about(name);
            assert!(
                got.starts_with(prefix),
                "`{name}` about-text drifted: expected it to start with {prefix:?}, got {got:?}"
            );
        }
    }
}
