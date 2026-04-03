# clawhip × OMX native hook bridge

This directory ships a clawhip-side OMX integration that forwards native OMX hook envelopes to the clawhip daemon without making OMX users hand-roll generic `IncomingEvent` HTTP payloads.

For OMC/OMX-integrated operator workflows, clawhip is the source of truth for routing doctrine, setup, and troubleshooting. Session skills should stay thin and point back here.

## Recommended default setup

For new clawhip + OMX installs, this bridge is the default/recommended integration path: install the hook assets from this directory, let the SDK forward the frozen v1 envelope, and prefer `clawhip omx hook` when the CLI is available, falling back to `/api/omx/hook` over HTTP when needed. Use generic event-payload translation or legacy local wrapper emits only when you need compatibility with older setups.

## What is included

- `clawhip-sdk.mjs` — small OMX-facing client that hides clawhip discovery + transport details
- `clawhip-hook.mjs` — sample `.omx/hooks/*.mjs` plugin that forwards contract-compliant events to clawhip
- `install-hook.sh` — copies the sample hook into `.omx/hooks/` and the SDK into `.omx/hooks/lib/` so `omx hooks validate` only sees real plugins

## Transport and discovery order

The SDK chooses the lightest transport that preserves native semantics:

1. **CLI transport** — preferred when `clawhip` is available (`CLAWHIP_BIN` or `PATH`)
   - sends the raw v1 envelope to `clawhip omx hook`
2. **HTTP transport** — fallback when a daemon URL is discoverable
   - checks `CLAWHIP_OMX_DAEMON_URL`
   - then `CLAWHIP_DAEMON_URL`
   - then `CLAWHIP_CONFIG` / `~/.clawhip/config.toml`
   - finally falls back to `http://127.0.0.1:25294`

Override the transport explicitly with:

```bash
export CLAWHIP_OMX_TRANSPORT=cli   # or http
```

## Install into an OMX workspace

```bash
./integrations/omx/install-hook.sh /path/to/repo/.omx/hooks
```

The installer keeps the SDK outside the top-level plugin scan path so validation/tests stay clean. Then validate inside that OMX workspace:

```bash
omx hooks validate
omx hooks test
```

If you already have a serialized v1 hook envelope, clawhip also exposes a matching thin client:

```bash
clawhip omx hook --file payload.json
# or
cat payload.json | clawhip omx hook
```

## Gajae operator runbook

### 1. Setup

1. Confirm the clawhip binary and daemon runtime you intend to use:
   ```bash
   which clawhip
   clawhip --version
   clawhip status
   ```
2. Install the native hook bridge into the OMX workspace you actually run.
3. Add a native session route in `~/.clawhip/config.toml`:
   ```toml
   [[routes]]
   event = "session.*"
   filter = { tool = "omx", repo_name = "clawhip" }
   channel = "1480171113253175356"
   format = "compact"
   ```
4. Keep `[defaults].channel` as a fallback only. If a session route misses, clawhip can still deliver to the default channel.
5. If OMC also emits into clawhip, reuse the same event family and switch only the `tool` filter where needed.

### 2. Verify

1. Re-run `omx hooks validate` and `omx hooks test` in the target workspace.
2. Read the route you expect clawhip to match. The critical fields are:
   - `event = "session.*"`
   - `filter.tool = "omx"`
   - `filter.repo_name = "<repo>"`
3. Start or resume a real OMX session and confirm the first native session notification lands in the intended channel.
4. If the notification lands in the default channel instead, treat that as a route miss first.
5. If you are also testing built-in cron behavior, confirm `[[cron.jobs]]` is populated before treating `clawhip cron run` output as meaningful.

### 3. Fix the April 3, 2026 failure modes

#### daemon version/runtime mismatch

**Symptom**
- Hook install/validation succeeds, but live delivery still behaves like an older clawhip runtime.

**Verify**
- Compare `which clawhip` and `clawhip --version` in the shell used by OMX.
- Confirm the running daemon was restarted after the last clawhip update/install.
- If you pin `CLAWHIP_BIN`, confirm it points at the binary you just updated.

**Fix**
- Reinstall or update clawhip.
- Restart the daemon/service so the running process matches the installed binary.
- If the wrong binary keeps winning on `PATH`, set `CLAWHIP_BIN` explicitly for the hook environment.

#### OMX hook installed but no `session.*` route

**Symptom**
- The hook is present and validates, but native OMX lifecycle notifications do not land in the intended session channel.

**Verify**
- Run `omx hooks validate` and `omx hooks test` again in the target workspace.
- Inspect `~/.clawhip/config.toml` and confirm a `[[routes]]` entry exists for `event = "session.*"`.
- Confirm the route filters match the live payload you expect (`tool = "omx"`, `repo_name = "..."`).

**Fix**
- Add the missing `session.*` route.
- Keep old `agent.*` routes only for compatibility; do not rely on them as the primary native route family.
- Re-test with a real session event after saving the config.

#### session route misconfigured with `repo` instead of `repo_name`

**Symptom**
- GitHub or git routes still work, but native OMC/OMX session routes miss.

**Verify**
- Look for a route like `filter = { tool = "omx", repo = "clawhip" }`.
- Compare it to the normalized native contract, which promotes `repo_name` onto the top-level session payload.

**Fix**
- Change the native session route to `repo_name`:
  ```toml
  [[routes]]
  event = "session.*"
  filter = { tool = "omx", repo_name = "clawhip" }
  channel = "1480171113253175356"
  ```
- Keep `repo` filters for GitHub/git families where they still make sense.

#### fallback to default channel when the route misses

**Symptom**
- A native session notification arrives in `[defaults].channel` instead of the repo-specific room you expected.

**Verify**
- Check whether `[defaults].channel` is configured.
- Confirm the session-specific `[[routes]]` entry did not match because of the wrong event family or filter key.
- Re-read the route resolution rules in the main README if needed.

**Fix**
- Treat wrong-channel delivery as evidence that transport worked but routing missed.
- Correct the `session.*` route and its filters.
- Keep the default channel as a safety net, not the steady-state destination for session traffic.

#### `clawhip cron run` needs configured jobs before it is meaningful

**Symptom**
- You run `clawhip cron run` and expect follow-up activity, but nothing useful happens.

**Verify**
- Inspect `~/.clawhip/config.toml` for both `[cron]` and at least one `[[cron.jobs]]` entry.
- Confirm each job has an `id`, `schedule`, and delivery target/message fields.

**Fix**
- Define one or more cron jobs first.
- Then re-run `clawhip cron run` or let the daemon-managed cron worker pick them up.
- Treat cron as adjacent ops plumbing: useful only after job config exists.

## Contract boundary notes

The SDK only forwards the frozen v1 `normalized_event` surface:

- `started`
- `blocked`
- `finished`
- `failed`
- `retry-needed`
- `pr-created`
- `test-started`
- `test-finished`
- `test-failed`
- `handoff-needed`

`tool.use` is intentionally **not** a new v1 canonical event. Use `tool_name`, `command`, and `error_summary` metadata on one of the frozen events instead.

## Future UX note

If this operator flow keeps recurring, the next small UX layer should probably be documentation-backed commands rather than more skill prose:

- `clawhip omx subscribe` — scaffold a canonical `session.*` route
- `clawhip omx doctor` — validate hook install, transport discovery, and route keys
- `clawhip omx unsubscribe` — remove the canonical session route cleanly

That is intentionally a docs-only proposal for now.

## Manual usage

```js
import { createClawhipOmxClient } from './clawhip-sdk.mjs';

const client = await createClawhipOmxClient();
await client.emitSessionStarted({
  context: {
    session_name: 'issue-65-native-sdk',
    repo_path: '/repo/clawhip',
    branch: 'feat/issue-65-native-sdk',
    status: 'started',
  },
});
```

Or forward an existing OMX hook event from a plugin:

```js
export async function onHookEvent(event, sdk) {
  const client = await createClawhipOmxClient();
  return await client.emitFromHookEvent(event);
}
```
