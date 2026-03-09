# Claw OS Architecture — clawhip v0.3.0+

> clawhip evolves from a Discord notification router into **Claw OS** — the operating system for AI development teams.

## Design Principles

1. **Small traits, clear boundaries** — Source, Renderer, Sink, Router are separate concerns
2. **Events are the universal language** — strongly-typed event envelopes with typed bodies
3. **Sinks are swappable** — Discord today, Slack tomorrow, both next week
4. **Local-first** — daemon runs on your machine, no cloud dependency
5. **Zero-config defaults** — works out of the box, scales with config
6. **Incremental migration** — every step is shippable; no big-bang refactor

## Current Architecture (v0.2.0)

```
┌─────────────────────────────────────────────┐
│                  clawhip daemon              │
│                                              │
│  CLI ──→ HTTP API ──→ Router ──→ Discord     │
│                         ↑                    │
│  Git Monitor ───────────┤                    │
│  tmux Monitor ──────────┤                    │
│  GitHub Webhooks ───────┘                    │
└─────────────────────────────────────────────┘
```

**Problems:**
- `DiscordClient` hardcoded in router dispatch + monitor loop
- Router returns `DeliveryTarget::Channel | Webhook` — Discord-specific
- Single-match routing: `route_for()` uses `.find()`, only one target per event
- Events are stringly-typed (`kind: String`, `payload: Value`)
- Duplicate tmux monitoring in `monitor.rs` and `tmux_wrapper.rs`
- Dead code: `watch.rs`, `server.rs` not wired into module tree

## Target Architecture (v0.3.0)

```
┌──────────────────────────────────────────────────────────┐
│                      clawhip daemon                       │
│                                                           │
│  ┌──────────┐     ┌──────────┐     ┌──────────────────┐  │
│  │ Sources   │────→│  mpsc    │────→│   Dispatcher     │  │
│  │           │     │  queue   │     │                  │  │
│  │ • Git     │     └──────────┘     │  Route resolve   │  │
│  │ • GitHub  │                      │  → 0..N matches  │  │
│  │ • tmux    │                      │  → render each   │  │
│  │ • Agent   │                      │  → deliver each  │  │
│  │ • HTTP in │                      └────────┬─────────┘  │
│  └──────────┘                                │            │
│                                     ┌────────┴─────────┐  │
│                                     │      Sinks       │  │
│                                     │  • Discord       │  │
│                                     │  • (Slack 0.4+)  │  │
│                                     └──────────────────┘  │
│                                                           │
│  ┌──────────────────────────────────────────────────┐     │
│  │  Optional: broadcast mirror for observability    │     │
│  └──────────────────────────────────────────────────┘     │
└──────────────────────────────────────────────────────────┘
```

## Core Abstractions

### 1. Event Model (`crate::event`)

Strongly-typed event envelopes with typed bodies — no more stringly-typed kind + untyped payload:

```rust
pub struct EventEnvelope {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub source: SourceId,
    pub body: EventBody,
    pub metadata: EventMetadata,
}

/// Typed event bodies — each variant carries its own typed struct.
pub enum EventBody {
    // Git
    GitCommit(GitCommitEvent),
    GitCommitAggregated(GitCommitAggregatedEvent),
    GitBranchChanged(GitBranchChangedEvent),

    // GitHub
    GitHubIssueOpened(GitHubIssueEvent),
    GitHubPROpened(GitHubPREvent),
    GitHubPRMerged(GitHubPREvent),
    GitHubPRStatusChanged(GitHubPRStatusEvent),
    GitHubCIFailed(GitHubCIEvent),

    // tmux
    TmuxKeyword(TmuxKeywordEvent),
    TmuxKeywordAggregated(TmuxKeywordAggregatedEvent),
    TmuxStale(TmuxStaleEvent),

    // Agent lifecycle
    AgentStarted(AgentEvent),
    AgentBlocked(AgentEvent),
    AgentFinished(AgentEvent),
    AgentFailed(AgentEvent),

    // Custom (escape hatch — JSON payload for user events)
    Custom(CustomEvent),
}

// Example typed body:
pub struct GitCommitEvent {
    pub repo: String,
    pub branch: String,
    pub sha: String,
    pub summary: String,
}

pub struct TmuxKeywordEvent {
    pub session: String,
    pub keyword: String,
    pub line: String,
}

pub struct CustomEvent {
    pub message: String,
    pub payload: Option<Value>,  // only Custom keeps dynamic JSON
}

pub struct EventMetadata {
    pub channel_hint: Option<String>,
    pub mention: Option<String>,
    pub format: Option<MessageFormat>,
    pub priority: EventPriority,
}

pub enum EventPriority {
    Low,      // routine updates
    Normal,   // standard notifications  
    High,     // failures, blockers
    Critical, // system down
}
```

**Key decision:** `Custom` variant is the only one with dynamic `Value`. All built-in events use typed structs. This makes routing filters type-safe for built-in events.

### 2. Source Trait (`crate::source`)

Monitors that produce events. Each source owns its polling loop:

```rust
#[async_trait]
pub trait Source: Send + Sync {
    fn name(&self) -> &str;

    /// Start polling/watching, emit events into the sender.
    async fn run(&self, tx: mpsc::Sender<EventEnvelope>) -> Result<()>;
}
```

**Built-in sources:**
- `GitSource` — local git repo polling (commits, branches)
- `GitHubSource` — GitHub API polling (PRs, issues, CI status)  
- `TmuxSource` — tmux pane monitoring (keywords, stale) — **consolidates monitor.rs + tmux_wrapper.rs**
- `AgentSource` — OMC/OMX lifecycle events
- `InboundSource` — HTTP webhook receiver

### 3. Renderer (`crate::render`)

Formats events into messages. Separate from transport:

```rust
pub trait Renderer: Send + Sync {
    /// Render an event for a specific sink type and format.
    fn render(
        &self,
        event: &EventEnvelope,
        format: &MessageFormat,
        template: Option<&str>,
    ) -> Result<RenderedMessage>;
}

pub struct RenderedMessage {
    pub text: String,
    pub rich: Option<Value>,  // sink-specific rich format (embeds, blocks)
}
```

**Default renderer** handles all built-in event bodies with compact/alert/inline formats. Sinks can provide their own renderer for platform-specific formatting (e.g., Slack blocks).

### 4. Sink Trait (`crate::sink`)

Pure transport — accepts a rendered message, delivers it:

```rust
#[async_trait]
pub trait Sink: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> SinkCapabilities;

    /// Deliver a rendered message to a target.
    async fn send(&self, target: &SinkTarget, message: &RenderedMessage) -> Result<()>;
}

pub struct SinkCapabilities {
    pub reactions: bool,
    pub threads: bool,
    pub rich_formatting: bool,
}

/// Sink-specific target — not a generic string.
pub enum SinkTarget {
    DiscordChannel(String),
    DiscordWebhook(String),
    SlackChannel(String),       // future
    SlackWebhook(String),       // future
    Custom(String),
}
```

**Key decision:** `SinkTarget` is an enum, not a generic string. This prevents stringly-typed target bugs and enables validation at config parse time.

### 5. Router (`crate::router`)

Event → 0..N resolved deliveries:

```rust
pub struct Router {
    rules: Vec<RouteRule>,
    defaults: RouteDefaults,
}

pub struct RouteRule {
    pub event_pattern: EventPattern,       // typed pattern matching
    pub filters: Vec<EventFilter>,         // typed field filters
    pub sink: String,                      // "discord", "slack"
    pub target: SinkTarget,
    pub format: Option<MessageFormat>,
    pub mention: Option<String>,
    pub template: Option<String>,
}

pub struct ResolvedDelivery {
    pub sink: String,
    pub target: SinkTarget,
    pub message: RenderedMessage,
    pub mention: Option<String>,
}

impl Router {
    /// Resolve an event to 0..N deliveries.
    /// Multiple rules can match the same event.
    pub fn resolve(&self, event: &EventEnvelope) -> Vec<ResolvedDelivery>;
}
```

### 6. Dispatcher (`crate::dispatch`)

Central coordinator — consumes from mpsc, routes, delivers:

```rust
pub struct Dispatcher {
    rx: mpsc::Receiver<EventEnvelope>,
    router: Router,
    renderer: Box<dyn Renderer>,
    sinks: HashMap<String, Box<dyn Sink>>,
    observer: Option<broadcast::Sender<EventEnvelope>>,  // optional mirror
}

impl Dispatcher {
    pub async fn run(&mut self) -> Result<()> {
        while let Some(event) = self.rx.recv().await {
            // Optional: mirror to observers
            if let Some(ref obs) = self.observer {
                let _ = obs.send(event.clone());
            }

            let deliveries = self.router.resolve(&event);
            for delivery in deliveries {
                if let Some(sink) = self.sinks.get(&delivery.sink) {
                    if let Err(e) = sink.send(&delivery.target, &delivery.message).await {
                        eprintln!("delivery failed to {}/{:?}: {e}", delivery.sink, delivery.target);
                        // best-effort: log and continue, don't fail other deliveries
                    }
                }
            }
        }
        Ok(())
    }
}
```

## Delivery Semantics

Explicitly defined (not left to implementers):

| Aspect | Behavior |
|--------|----------|
| **Ordering** | Per-source FIFO via mpsc. No global ordering across sources. |
| **Multi-route failure** | Best-effort. Each delivery is independent. One failure doesn't block others. |
| **Retries** | None in v0.3. Sink errors are logged. Retry/DLQ is v0.4+. |
| **Deduplication** | Source-level (keyword window dedup already exists). No dispatcher-level dedup. |
| **Backpressure** | mpsc provides natural backpressure. Slow dispatcher = sources block. |
| **Idempotency** | Events have UUID. Sinks may use for idempotency if needed. |

## Pipeline Choice: mpsc vs broadcast

| | mpsc (primary) | broadcast (observer) |
|---|---|---|
| **Use** | Source → Dispatcher | Dispatcher → debug/metrics/dashboard |
| **Why** | Single consumer, backpressure, no message loss | Multi-subscriber, non-critical, lag-tolerant |
| **Failure** | Sources block if dispatcher is slow | Slow observers get lag errors (acceptable) |

## Security Model

| Boundary | Policy |
|----------|--------|
| **Dynamic tokens** (`{sh:...}`) | Only evaluated when route has `allow_dynamic_tokens = true` |
| **Inbound webhooks** | Never trigger dynamic token evaluation |
| **Sink credentials** | Scoped per-sink in config. Sinks only access their own tokens. |
| **Templates** | User-controlled. No shell execution unless explicit opt-in. |

## Config Evolution

```toml
[daemon]
port = 25294

# v0.3: providers.* replaces top-level [discord]
# Legacy [discord] section still works (auto-mapped)
[providers.discord]
token = "..."
default_channel = "1234567890"

# Future (v0.4+):
# [providers.slack]
# webhook_url = "https://hooks.slack.com/..."

# Sources replace monitors.*
# Legacy monitors.* still works (auto-mapped)
[sources.git]
poll_interval_secs = 60

[[sources.git.repos]]
path = "/home/user/project"
github_repo = "org/repo"

[sources.tmux]
poll_interval_secs = 30

[[sources.tmux.sessions]]
session = "issue-*"
keywords = ["panic", "SIGKILL"]
stale_minutes = 30
keyword_window_secs = 30

# Routes: new `sink` field (defaults to "discord" for compat)
[[routes]]
event = "github.*"
sink = "discord"
target = "1234567890"
mention = "<@bot>"

# Route without sink = discord (backward compat)
[[routes]]
event = "tmux.stale"
target = "1234567890"
```

## Config Migration (v0.2 → v0.3)

Explicit mapping, backed by tests:

| v0.2 | v0.3 | Notes |
|------|------|-------|
| `[discord].token` | `[providers.discord].token` | Auto-mapped if legacy exists |
| `[discord].default_channel` | `[providers.discord].default_channel` | |
| `route.channel` | `route.target` + `sink = "discord"` | SinkTarget::DiscordChannel |
| `route.webhook` | `route.target` + `sink = "discord"` | SinkTarget::DiscordWebhook |
| `route` without `sink` | `sink = "discord"` implied | Backward compat default |
| `[monitors.git]` | `[sources.git]` | Alias |
| `[monitors.tmux]` | `[sources.tmux]` | Alias |

**Implementation:** Parse old config into old structs, normalize into new internal model. Two separate serde models, not one with optional fields.

**Required golden tests:**
- Old Discord-only config parses correctly
- Mixed legacy + new config
- Routes without sink field
- Routes with webhook target
- Conflicting legacy/new fields → clear error
- monitors → sources alias

## Cleanup Before Refactor

**Remove/quarantine before v0.3 implementation:**
- `src/watch.rs` — dead alternate architecture, not in module tree
- `src/server.rs` — unused, not wired
- Consolidate tmux monitoring: `monitor.rs` tmux logic + `tmux_wrapper.rs` monitor logic → single `TmuxSource`

## Implementation Phases (Vertical Slices)

Each phase is independently shippable and testable:

### Phase 1: Internal Event Model
- [ ] Define `EventEnvelope`, `EventBody` enum with typed structs
- [ ] Wrap current `IncomingEvent` → `EventEnvelope` at ingress boundary
- [ ] Keep all external behavior identical
- [ ] Remove `watch.rs` and `server.rs`

### Phase 2: Router Generalization  
- [ ] Router resolves 0..N deliveries (not single `.find()`)
- [ ] `ResolvedDelivery` with `SinkTarget` enum
- [ ] Discord still the only sink
- [ ] Tests for multi-match behavior

### Phase 3: Source Extraction
- [ ] `Source` trait + `mpsc` pipeline
- [ ] `GitSource` — extract from `monitor.rs`
- [ ] `TmuxSource` — consolidate `monitor.rs` + `tmux_wrapper.rs`
- [ ] `GitHubSource` — extract GitHub API polling
- [ ] Dispatcher consumes from mpsc, replaces direct monitor→router→discord calls

### Phase 4: Sink Extraction + Renderer
- [ ] `Sink` trait with `DiscordSink` as first implementation
- [ ] `Renderer` trait with default implementation
- [ ] Config migration layer (`[discord]` → `[providers.discord]`)
- [ ] `sink` field in route config (default: "discord")

### Phase 5: Second Sink (v0.4+)
- [ ] Slack sink (#28)
- [ ] Slack-specific renderer (blocks format)
- [ ] Multi-sink delivery tested end-to-end

### Phase 6: State + Orchestration (v0.5+)
- [ ] SessionManager — track active coding sessions
- [ ] ProjectStore — persistent project context
- [ ] Notion/Jira sinks (#29, #31)
- [ ] Obsidian sync (#32)
- [ ] Work queue / auto-spawn

## File Structure (Target v0.3)

```
src/
├── main.rs
├── cli.rs
├── config/
│   ├── mod.rs           # unified config
│   ├── legacy.rs        # v0.2 compat parsing
│   └── migration.rs     # old → new normalization
├── daemon.rs
│
├── event/
│   ├── mod.rs           # EventEnvelope, EventBody, EventMetadata
│   ├── body.rs          # typed event structs
│   └── compat.rs        # IncomingEvent → EventEnvelope bridge
│
├── source/
│   ├── mod.rs           # Source trait
│   ├── git.rs           # Git repo monitor
│   ├── github.rs        # GitHub API monitor
│   ├── tmux.rs          # tmux pane monitor (consolidated)
│   ├── agent.rs         # Agent lifecycle
│   └── inbound.rs       # HTTP webhook receiver
│
├── router/
│   ├── mod.rs           # Router, RouteRule
│   ├── filter.rs        # Event pattern matching
│   └── template.rs      # Dynamic tokens + templates
│
├── render/
│   ├── mod.rs           # Renderer trait
│   └── default.rs       # Default compact/alert/inline rendering
│
├── sink/
│   ├── mod.rs           # Sink trait, SinkTarget
│   └── discord.rs       # Discord sink (bot + webhook)
│
├── dispatch.rs          # Dispatcher (mpsc consumer → router → sinks)
│
└── util/
    ├── dynamic_tokens.rs
    └── keyword_window.rs
```

## Key Decisions Log

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | **Separate traits: Source, Renderer, Sink** | Provider trait was too fat. Current code naturally separates rendering (events.rs), routing (router.rs), transport (discord.rs). |
| 2 | **mpsc for primary pipeline** | Single dispatcher consumer. Backpressure. No message loss. broadcast only for observers. |
| 3 | **Typed event bodies (enum)** | No more stringly-typed kind + untyped payload. Custom variant is the only escape hatch. |
| 4 | **SinkTarget enum, not generic String** | Prevents stringly-typed target bugs. Enables validation at config parse time. |
| 5 | **Best-effort multi-delivery** | One failure doesn't block others. Retry/DLQ deferred to v0.4+. |
| 6 | **Two-model config migration** | Parse legacy into old structs, normalize to new. Not one struct with optional fields everywhere. |
| 7 | **Vertical slice phases** | Each phase is shippable. No layer-by-layer refactor that blocks shipping. |
| 8 | **Compile-time sinks only** | No runtime plugin loading until v0.5+. Feature flags for optional sinks. |
| 9 | **Clean dead code first** | Remove watch.rs, server.rs before adding new abstractions. |

---

*This is a living document. Updated as implementation progresses.*
*Revised after architecture self-review (ARCHITECTURE-REVIEW.md).*

—
*[repo owner's gaebal-gajae (clawdbot) 🦞]*
