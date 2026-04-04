# Canonical contract cleanup audit and incremental plan

This document records the Issue #132 cleanup pass for clawhip's event, delivery, and rendered-message contracts.

## Scope audited

- source adapters / ingress shims
- routing and filter keys
- delivery / channel resolution
- batching keys
- renderer assumptions

## Current audit snapshot

### 1) Source adapters and native ingress

| Surface | Current entrypoint | Current canonicalization behavior | Notes |
|---|---|---|---|
| Git source | `src/source/git.rs` | Emits `git.commit` and `git.branch-changed` with `repo`, `branch`, `repo_path`, `worktree_path` | Native git payloads do **not** emit `repo_name` directly. |
| GitHub source | `src/source/github.rs` | Emits `github.*` payloads with `repo`, `number`, status/title/url fields | CI batching already depends on stable `repo`/`number`/`sha`/run-id. |
| tmux source | `src/source/tmux.rs` | Emits `tmux.keyword` / `tmux.stale` with `session`, `pane`, keyword fields | Native tmux payloads do **not** emit `session_name` directly. |
| workspace source | `src/source/workspace.rs` | Emits `workspace.*` payloads with workspace/state metadata | Already closer to a canonical metadata shape. |
| OMX SDK / hook | `integrations/omx/clawhip-sdk.mjs`, `integrations/omx/clawhip-hook.mjs` | Restricts forwarded events to the documented native contract and sends `context.normalized_event` | Good contract boundary already exists. |
| OMC/native session ingress | `src/events.rs`, `src/event/compat.rs` | Normalizes `routeKey` / `normalized_event` into `session.*` and backfills metadata | This is the strongest canonical layer today. |

### 2) Routing and filter keys

Current routing matches on `IncomingEvent::template_context()` via `src/router.rs`.

Observed filter-key families today:

- git / GitHub routes commonly use `repo`
- tmux routes commonly use `session`
- native session routes commonly use `repo_name`, `session_name`, `issue_number`, `pr_number`, `tool`
- compatibility routing already accepts some event-family aliases (`agent.*` <-> `session.*`, `git.*` <-> `github.*` family fallbacks)

#### Gap found

Different source families expose different primary filter keys for the same conceptual entity:

- repository: `repo` vs `repo_name`
- session: `session` vs `session_name`
- channel hint: top-level `channel` vs metadata `channel_hint`

That makes route rules more brittle than they need to be when a team wants one canonical filter vocabulary.

### 3) Delivery and channel resolution

Resolved in `src/router.rs` today:

1. matching route sink
2. route webhook target, if any
3. route channel
4. event top-level channel
5. default channel

This precedence is already coherent and covered by tests.

### 4) Batching keys

#### GitHub CI batching

`src/dispatch.rs::ci_batch_key()` batches on:

- `repo`
- PR number when present
- `sha`
- workflow run id extracted from URL, else the raw URL

This is already a stable contract key for CI summaries.

#### Routine batching

Before this cleanup pass, routine batching grouped only by delivery shape:

- sink
- target
- format
- mention
- template
- dynamic-token flag

#### Gap found

That allowed different canonical event kinds to collapse into the same routine batch if they happened to target the same delivery. This was message-contract slop: unrelated event families could share one batched send.

### 5) Renderer assumptions

`src/render/default.rs` currently assumes these message contracts:

- `session.*` is the canonical low-noise operational family
- `workspace.*` is renderer-owned and uses workspace/state metadata
- git / tmux aggregated forms are encoded inside the same event kind with extra payload fields
- session rendering prefers canonical metadata like `repo_name`, `session_name`, `issue_number`, `pr_number`, `branch`, `test_runner`

This means renderer behavior is already standardized around the native `session.*` metadata contract, but older source families still expose mixed routing keys.

## Decisions landed in this pass

### Decision 1: standardize route-filter aliases in context, not by rewriting every source payload

To keep the pass incremental and backward-compatible, clawhip now normalizes alias keys in `IncomingEvent::template_context()`:

- `repo` <-> `repo_name`
- `session` <-> `session_name`
- `channel` <-> `channel_hint`
- `event` / `contract_event` default to the canonical event kind
- `route_key` defaults to the canonical event kind when upstream did not provide one

Why here:

- no config breakage
- no source rewrite required
- route rules can converge on one vocabulary immediately
- payloads remain backward-compatible for existing sinks / renderers / docs

### Decision 2: routine batches must stay within one canonical event kind

Routine batching now includes `event.canonical_kind()` in the batch key.

Effect:

- `tmux.keyword` and `git.commit` no longer collapse into the same routine batch just because they share a Discord target
- batch content stays semantically coherent
- this remains backward-compatible for existing single-family burst batching

## Initial canonical contract rules

For incremental adoption, route authors should prefer this vocabulary:

- `event` / `contract_event`
- `tool`
- `repo_name`
- `session_name`
- `issue_number`
- `pr_number`
- `branch`
- `channel_hint`

Legacy aliases remain supported:

- `repo`
- `session`
- `channel`

## Follow-up slices

### Slice A: source payload parity

Optionally teach git / GitHub / tmux sources to emit the canonical names directly in payloads (`repo_name`, `session_name`) in addition to legacy keys.

### Slice B: explicit delivery contract type

Promote a documented delivery identity object around:

- canonical event kind
- sink
- target
- format
- mention
- template
- dynamic-token policy

### Slice C: renderer contract fixtures

Add snapshot-style tests for one canonical example per event family:

- `session.*`
- `workspace.*`
- `git.*`
- `github.*`
- `tmux.*`
- batched CI / routine summaries

## Files tied to this pass

- `src/events.rs`
- `src/router.rs`
- `src/dispatch.rs`
- `docs/canonical-contract-cleanup.md`
