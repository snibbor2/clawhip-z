use std::fs;
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use serde_json::{Map, Value, json};

use crate::Result;
use crate::cli::NativeInstallArgs;

const PROJECT_METADATA: &str = ".clawhip/project.json";
const HOOK_SCRIPT: &str = ".clawhip/hooks/native-hook.mjs";
const AUGMENT_SCRIPT: &str = ".clawhip/hooks/augment.mjs";
const CLAUDE_SETTINGS: &str = ".claude/settings.json";
const CODEX_CONFIG: &str = ".codex/config.toml";
const CODEX_HOOKS: &str = ".codex/hooks.json";
const SHARED_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "PreToolUse",
    "PostToolUse",
    "UserPromptSubmit",
    "Stop",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NativeProvider {
    Codex,
    Claude,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NativeInstallScope {
    Project,
    Global,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    pub scope: NativeInstallScope,
    pub root: PathBuf,
    pub hook_script: PathBuf,
    pub project_metadata: PathBuf,
    pub augment_script: PathBuf,
    pub claude_settings: Option<PathBuf>,
    pub codex_config: Option<PathBuf>,
    pub codex_hooks: Option<PathBuf>,
}

pub fn install(args: NativeInstallArgs) -> Result<()> {
    let report = install_with_paths(&args, None)?;

    println!(
        "Installed provider-native hooks ({:?} scope) in {}",
        report.scope,
        report.root.display()
    );
    println!("  {}", report.project_metadata.display());
    println!("  {}", report.hook_script.display());
    println!("  {}", report.augment_script.display());
    if let Some(path) = &report.claude_settings {
        println!("  {}", path.display());
    }
    if let Some(path) = &report.codex_config {
        println!("  {}", path.display());
    }
    if let Some(path) = &report.codex_hooks {
        println!("  {}", path.display());
    }
    Ok(())
}

pub fn install_with_paths(
    args: &NativeInstallArgs,
    home_override: Option<&Path>,
) -> Result<InstallReport> {
    let scope = args.scope;
    let root = install_root(args, home_override)?;
    let hook_script = root.join(HOOK_SCRIPT);
    let project_metadata = root.join(PROJECT_METADATA);
    let augment_script = root.join(AUGMENT_SCRIPT);

    fs::create_dir_all(hook_script.parent().expect("hook script parent"))?;
    fs::create_dir_all(project_metadata.parent().expect("project metadata parent"))?;

    fs::write(&hook_script, native_hook_script())?;
    if !project_metadata.exists() {
        fs::write(
            &project_metadata,
            serde_json::to_string_pretty(&default_project_metadata(&root))? + "\n",
        )?;
    }
    if !augment_script.exists() {
        fs::write(&augment_script, augment_script_template())?;
    }

    let mut report = InstallReport {
        scope,
        root: root.clone(),
        hook_script,
        project_metadata,
        augment_script,
        claude_settings: None,
        codex_config: None,
        codex_hooks: None,
    };

    if args.provider.installs_claude() {
        let path = root.join(CLAUDE_SETTINGS);
        write_claude_settings(
            &path,
            provider_command(&root, scope, NativeProvider::Claude),
        )?;
        report.claude_settings = Some(path);
    }

    if args.provider.installs_codex() {
        let config_path = root.join(CODEX_CONFIG);
        let hooks_path = root.join(CODEX_HOOKS);
        write_codex_config(&config_path)?;
        write_codex_hooks(
            &hooks_path,
            provider_command(&root, scope, NativeProvider::Codex),
        )?;
        report.codex_config = Some(config_path);
        report.codex_hooks = Some(hooks_path);
    }

    Ok(report)
}

pub fn incoming_event_from_native_hook_json(
    payload: &Value,
) -> Result<crate::events::IncomingEvent> {
    let provider = normalize_provider(
        first_string(
            payload,
            &[
                "/provider",
                "/source/provider",
                "/context/provider",
                "/payload/provider",
            ],
        )
        .as_deref(),
    );
    let event_name = first_string(
        payload,
        &[
            "/event_name",
            "/event",
            "/hook_event_name",
            "/hookEventName",
            "/payload/hook_event_name",
            "/payload/hookEventName",
        ],
    )
    .ok_or_else(|| "missing native hook event name".to_string())?;

    let canonical = map_common_event(&event_name)
        .ok_or_else(|| format!("unsupported native hook event '{event_name}'"))?;

    let directory = first_string(
        payload,
        &[
            "/directory",
            "/cwd",
            "/context/directory",
            "/context/cwd",
            "/payload/cwd",
            "/payload/directory",
            "/repo_path",
            "/projectPath",
            "/context/projectPath",
        ],
    );
    let repo_path = first_string(
        payload,
        &[
            "/repo_path",
            "/context/repo_path",
            "/payload/repo_path",
            "/git/repo_path",
        ],
    )
    .or_else(|| directory.clone());
    let worktree_path = first_string(
        payload,
        &[
            "/worktree_path",
            "/context/worktree_path",
            "/payload/worktree_path",
            "/payload/cwd",
        ],
    )
    .or_else(|| directory.clone());
    let metadata = payload
        .get("project_identity")
        .cloned()
        .or_else(|| payload.get("project_metadata").cloned())
        .or_else(|| {
            directory
                .as_deref()
                .and_then(|dir| load_project_metadata_from_dir(Path::new(dir)))
        });

    let project = first_string(
        payload,
        &[
            "/project",
            "/project_name",
            "/projectName",
            "/context/project",
            "/context/project_name",
            "/context/projectName",
            "/payload/project",
            "/payload/project_name",
        ],
    )
    .or_else(|| metadata.as_ref().and_then(project_name_from_metadata))
    .or_else(|| {
        repo_path.as_deref().and_then(|path| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToString::to_string)
        })
    })
    .or_else(|| {
        worktree_path.as_deref().and_then(|path| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToString::to_string)
        })
    });
    let repo_name = first_string(
        payload,
        &["/repo_name", "/context/repo_name", "/payload/repo_name"],
    )
    .or_else(|| metadata.as_ref().and_then(repo_name_from_metadata))
    .or_else(|| project.clone());
    let session_id = first_string(
        payload,
        &[
            "/session_id",
            "/sessionId",
            "/payload/session_id",
            "/payload/sessionId",
            "/context/session_id",
            "/context/sessionId",
        ],
    );
    let turn_id = first_string(
        payload,
        &["/turn_id", "/turnId", "/payload/turn_id", "/payload/turnId"],
    );
    let tool_name = first_string(
        payload,
        &[
            "/tool_name",
            "/toolName",
            "/payload/tool_name",
            "/payload/toolName",
        ],
    );
    let command = first_string(
        payload,
        &[
            "/command",
            "/tool_input/command",
            "/payload/tool_input/command",
            "/payload/command",
        ],
    );
    let prompt = first_string(payload, &["/prompt", "/payload/prompt"]);
    let model = first_string(payload, &["/model", "/payload/model"]);
    let session_source =
        first_string(payload, &["/payload/source", "/source"]).filter(|value| value != &provider);

    let event_payload = payload
        .get("event_payload")
        .cloned()
        .or_else(|| payload.get("payload").cloned())
        .unwrap_or_else(|| json!({}));

    let mut base_context = Map::new();
    base_context.insert("provider".into(), json!(provider.clone()));
    base_context.insert("event_name".into(), json!(event_name.clone()));
    base_context.insert(
        "normalized_event".into(),
        json!(canonical.trim_start_matches("session.")),
    );
    if let Some(directory) = directory.clone() {
        base_context.insert("directory".into(), json!(directory));
    }
    if let Some(repo_path) = repo_path.clone() {
        base_context.insert("repo_path".into(), json!(repo_path));
    }
    if let Some(worktree_path) = worktree_path.clone() {
        base_context.insert("worktree_path".into(), json!(worktree_path));
    }
    if let Some(repo_name) = repo_name.clone() {
        base_context.insert("repo_name".into(), json!(repo_name));
    }
    if let Some(project) = project.clone() {
        base_context.insert("project".into(), json!(project));
    }
    if let Some(metadata) = metadata.clone() {
        base_context.insert("project_identity".into(), metadata);
    }
    if let Some(session_id) = session_id.clone() {
        base_context.insert("session_id".into(), json!(session_id));
    }
    if let Some(turn_id) = turn_id.clone() {
        base_context.insert("turn_id".into(), json!(turn_id));
    }
    if let Some(model) = model.clone() {
        base_context.insert("model".into(), json!(model));
    }
    if let Some(tool_name) = tool_name.clone() {
        base_context.insert("tool_name".into(), json!(tool_name));
    }
    if let Some(command) = command.clone() {
        base_context.insert("command".into(), json!(command));
    }
    if let Some(prompt) = prompt.clone() {
        base_context.insert("prompt".into(), json!(prompt));
    }
    if let Some(session_source) = session_source.clone() {
        base_context.insert("session_source".into(), json!(session_source));
    }

    let mut normalized = Map::new();
    normalized.insert("provider".into(), json!(provider.clone()));
    normalized.insert("source".into(), json!(provider.clone()));
    normalized.insert("tool".into(), json!(provider.clone()));
    normalized.insert("agent_name".into(), json!(provider.clone()));
    normalized.insert("event_name".into(), json!(event_name));
    normalized.insert(
        "normalized_event".into(),
        json!(canonical.trim_start_matches("session.")),
    );
    normalized.insert("context".into(), Value::Object(base_context));
    normalized.insert("event_payload".into(), event_payload);
    normalized.insert("payload".into(), payload.clone());

    insert_string(&mut normalized, "directory", directory.clone());
    insert_string(&mut normalized, "repo_path", repo_path.clone());
    insert_string(&mut normalized, "worktree_path", worktree_path.clone());
    insert_string(&mut normalized, "repo_name", repo_name.clone());
    insert_string(&mut normalized, "project", project);
    insert_string(&mut normalized, "session_id", session_id);
    insert_string(&mut normalized, "turn_id", turn_id);
    insert_string(&mut normalized, "tool_name", tool_name);
    insert_string(&mut normalized, "command", command);
    insert_string(&mut normalized, "prompt", prompt);
    insert_string(&mut normalized, "model", model);
    insert_string(&mut normalized, "session_source", session_source);
    if let Some(metadata) = metadata {
        normalized.insert("project_identity".into(), metadata);
    }

    if let Some(status) = status_for_canonical_event(canonical) {
        normalized.insert("status".into(), json!(status));
    }

    apply_augmentation(
        &mut normalized,
        payload
            .get("augmentation")
            .or_else(|| payload.get("augment"))
            .unwrap_or(&Value::Null),
    );

    Ok(crate::events::IncomingEvent {
        kind: canonical.to_string(),
        channel: None,
        mention: None,
        format: None,
        template: None,
        payload: Value::Object(normalized),
    })
}

fn install_root(args: &NativeInstallArgs, home_override: Option<&Path>) -> Result<PathBuf> {
    match args.scope {
        NativeInstallScope::Project => Ok(args.root.clone().unwrap_or(std::env::current_dir()?)),
        NativeInstallScope::Global => Ok(home_override
            .map(Path::to_path_buf)
            .or_else(|| std::env::var("HOME").ok().map(PathBuf::from))
            .ok_or_else(|| "HOME environment variable not set".to_string())?),
    }
}

fn default_project_metadata(root: &Path) -> Value {
    let project = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown");
    json!({
        "project": project,
        "repo_name": project,
        "providers": ["claude", "codex"],
        "shared_events": SHARED_HOOK_EVENTS,
        "augment": {
            "script": ".clawhip/hooks/augment.mjs"
        }
    })
}

fn provider_command(root: &Path, scope: NativeInstallScope, provider: NativeProvider) -> String {
    let provider = match provider {
        NativeProvider::Codex => "codex",
        NativeProvider::Claude => "claude",
        NativeProvider::All => "codex",
    };

    match scope {
        NativeInstallScope::Project => format!(
            "bash -lc 'root=\"$(git -C \"{}\" rev-parse --show-toplevel 2>/dev/null || printf %s \"{}\")\"; node \"$root/.clawhip/hooks/native-hook.mjs\" --provider {}'",
            root.display(),
            root.display(),
            provider
        ),
        NativeInstallScope::Global => format!(
            "node \"{}\" --provider {}",
            root.join(HOOK_SCRIPT).display(),
            provider
        ),
    }
}

fn write_claude_settings(path: &Path, command: String) -> Result<()> {
    let mut root = read_json_object(path)?;
    let hooks = ensure_object(&mut root, "hooks");
    for event in SHARED_HOOK_EVENTS {
        ensure_command_hook(hooks, event, &command, None);
    }
    write_json(path, &root)
}

fn write_codex_config(path: &Path) -> Result<()> {
    let mut config = if path.exists() {
        fs::read_to_string(path)?
            .parse::<toml::Value>()
            .unwrap_or_else(|_| toml::Value::Table(Default::default()))
    } else {
        toml::Value::Table(Default::default())
    };

    let Some(table) = config.as_table_mut() else {
        return Err("codex config must be a TOML table".into());
    };
    let features = table
        .entry("features")
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let Some(features) = features.as_table_mut() else {
        return Err("codex [features] must be a TOML table".into());
    };
    features.insert("codex_hooks".into(), toml::Value::Boolean(true));

    fs::create_dir_all(path.parent().expect("codex config parent"))?;
    fs::write(path, toml::to_string_pretty(&config)? + "\n")?;
    Ok(())
}

fn write_codex_hooks(path: &Path, command: String) -> Result<()> {
    let mut root = read_json_object(path)?;
    let hooks = ensure_object(&mut root, "hooks");
    for event in SHARED_HOOK_EVENTS {
        let matcher = match *event {
            "SessionStart" => Some("startup|resume|clear|compact"),
            _ => None,
        };
        ensure_command_hook(hooks, event, &command, matcher);
    }
    write_json(path, &root)
}

fn ensure_command_hook(
    hooks: &mut Map<String, Value>,
    event: &str,
    command: &str,
    matcher: Option<&str>,
) {
    let groups = hooks
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(groups) = groups.as_array_mut() else {
        *groups = Value::Array(Vec::new());
        let Some(groups) = groups.as_array_mut() else {
            return;
        };
        ensure_command_hook_array(groups, command, matcher);
        return;
    };
    ensure_command_hook_array(groups, command, matcher);
}

fn ensure_command_hook_array(groups: &mut Vec<Value>, command: &str, matcher: Option<&str>) {
    let already_present = groups.iter().any(|group| {
        group.get("matcher").and_then(Value::as_str).map(str::trim) == matcher
            && group
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|handlers| {
                    handlers.iter().any(|handler| {
                        handler.get("type").and_then(Value::as_str) == Some("command")
                            && handler.get("command").and_then(Value::as_str) == Some(command)
                    })
                })
    });
    if already_present {
        return;
    }

    let mut group = Map::new();
    if let Some(matcher) = matcher {
        group.insert("matcher".into(), json!(matcher));
    }
    group.insert(
        "hooks".into(),
        Value::Array(vec![json!({
            "type": "command",
            "command": command,
        })]),
    );
    groups.push(Value::Object(group));
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let raw = fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| format!("{} must contain a JSON object", path.display()).into())
}

fn write_json(path: &Path, object: &Map<String, Value>) -> Result<()> {
    fs::create_dir_all(path.parent().expect("json parent"))?;
    fs::write(path, serde_json::to_string_pretty(object)? + "\n")?;
    Ok(())
}

fn ensure_object<'a>(object: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    let value = object
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value.as_object_mut().expect("object value")
}

fn normalize_provider(provider: Option<&str>) -> String {
    match provider
        .unwrap_or("unknown")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "claude-code" | "claude" => "claude".to_string(),
        "codex" => "codex".to_string(),
        other if !other.is_empty() => other.to_string(),
        _ => "unknown".to_string(),
    }
}

fn project_name_from_metadata(value: &Value) -> Option<String> {
    first_string(
        value,
        &["/project", "/project_name", "/name", "/slug", "/id"],
    )
}

fn repo_name_from_metadata(value: &Value) -> Option<String> {
    first_string(
        value,
        &["/repo_name", "/repo", "/project", "/name", "/slug"],
    )
}

fn load_project_metadata_from_dir(directory: &Path) -> Option<Value> {
    let mut current = Some(directory);
    while let Some(path) = current {
        let candidate = path.join(PROJECT_METADATA);
        if candidate.is_file() {
            let raw = fs::read_to_string(candidate).ok()?;
            let parsed: Value = serde_json::from_str(&raw).ok()?;
            if parsed.is_object() {
                return Some(parsed);
            }
        }
        current = path.parent();
    }
    None
}

fn map_common_event(value: &str) -> Option<&'static str> {
    match value
        .trim()
        .replace(['_', '.'], "-")
        .to_ascii_lowercase()
        .as_str()
    {
        "sessionstart" | "session-start" | "started" => Some("session.started"),
        "pretooluse" | "pre-tool-use" => Some("session.pre-tool-use"),
        "posttooluse" | "post-tool-use" => Some("session.post-tool-use"),
        "userpromptsubmit" | "user-prompt-submit" => Some("session.user-prompt-submit"),
        "sessionend" | "session-end" | "finished" | "stop" => Some("session.finished"),
        _ => None,
    }
}

fn status_for_canonical_event(kind: &str) -> Option<&'static str> {
    match kind {
        "session.started" => Some("started"),
        "session.pre-tool-use" => Some("pre-tool-use"),
        "session.post-tool-use" => Some("post-tool-use"),
        "session.user-prompt-submit" => Some("user-prompt-submit"),
        "session.finished" => Some("finished"),
        _ => None,
    }
}

fn first_string(payload: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn insert_string(object: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        object.insert(key.to_string(), json!(value));
    }
}

fn apply_augmentation(target: &mut Map<String, Value>, augmentation: &Value) {
    let Some(augmentation) = augmentation.as_object() else {
        return;
    };

    for (key, value) in augmentation {
        if is_reserved_augmentation_key(key) {
            continue;
        }

        match target.get_mut(key) {
            Some(existing) if existing.is_object() && value.is_object() => {
                merge_object_additive(
                    existing.as_object_mut().expect("object"),
                    value.as_object().expect("object"),
                );
            }
            Some(_) => {}
            None => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

fn merge_object_additive(target: &mut Map<String, Value>, source: &Map<String, Value>) {
    for (key, value) in source {
        match target.get_mut(key) {
            Some(existing) if existing.is_object() && value.is_object() => merge_object_additive(
                existing.as_object_mut().expect("object"),
                value.as_object().expect("object"),
            ),
            Some(_) => {}
            None => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

fn is_reserved_augmentation_key(key: &str) -> bool {
    matches!(
        key,
        "provider"
            | "source"
            | "tool"
            | "agent_name"
            | "event_name"
            | "normalized_event"
            | "directory"
            | "repo_path"
            | "worktree_path"
            | "repo_name"
            | "project"
            | "project_identity"
            | "session_id"
            | "turn_id"
            | "tool_name"
            | "command"
            | "prompt"
            | "model"
            | "event_payload"
            | "payload"
            | "status"
    )
}

fn augment_script_template() -> &'static str {
    r#"export default async function augment(_ctx) {
  return {
    context: {
      hook_augmented: true
    }
  };
}
"#
}

fn native_hook_script() -> &'static str {
    r#"#!/usr/bin/env node
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

function arg(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : '';
}

function readStdin() {
  return new Promise((resolveOut) => {
    const chunks = [];
    process.stdin.on('data', (chunk) => chunks.push(chunk));
    process.stdin.on('end', () => resolveOut(Buffer.concat(chunks).toString('utf8')));
    process.stdin.on('error', () => resolveOut(''));
  });
}

function readJson(path) {
  if (!existsSync(path)) return null;
  try {
    return JSON.parse(readFileSync(path, 'utf8'));
  } catch {
    return null;
  }
}

function findProjectMetadata(start) {
  let current = resolve(start);
  while (true) {
    const candidate = resolve(current, '.clawhip', 'project.json');
    if (existsSync(candidate)) return candidate;
    const parent = dirname(current);
    if (parent === current) return null;
    current = parent;
  }
}

function detectRepoPath(cwd) {
  const result = spawnSync('git', ['-C', cwd, 'rev-parse', '--show-toplevel'], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'ignore'],
  });
  if (result.status === 0) {
    return result.stdout.trim() || cwd;
  }
  return cwd;
}

async function loadAugmentation(basePayload, projectMetadataPath, projectMetadata) {
  const configured = projectMetadata?.augment?.script || projectMetadata?.hooks?.augment_script;
  const projectRoot = projectMetadataPath ? dirname(dirname(projectMetadataPath)) : basePayload.directory;
  const candidates = [];
  if (configured) {
    candidates.push(resolve(projectRoot, configured));
  }
  candidates.push(resolve(projectRoot, '.clawhip', 'hooks', 'augment.mjs'));
  candidates.push(resolve(projectRoot, '.clawhip', 'hooks', 'augment.js'));

  for (const candidate of candidates) {
    if (!existsSync(candidate)) continue;
    try {
      const mod = await import(pathToFileURL(candidate).href);
      const fn = mod.default || mod.augment || mod.buildAugmentation;
      if (typeof fn !== 'function') continue;
      const result = await fn({
        provider: basePayload.provider,
        eventName: basePayload.event_name,
        input: basePayload.event_payload,
        basePayload,
        projectMetadata,
      });
      if (result && typeof result === 'object' && !Array.isArray(result)) {
        return result;
      }
    } catch {
      // ignore augmentation failures so hooks stay non-blocking
    }
  }
  return null;
}

async function main() {
  const provider = arg('--provider') || process.env.CLAWHIP_PROVIDER || 'unknown';
  const cwd = process.cwd();
  const raw = await readStdin();
  let input = {};
  try {
    input = raw.trim() ? JSON.parse(raw) : {};
  } catch {}

  const eventName =
    input.hook_event_name ||
    input.hookEventName ||
    input.event_name ||
    input.event ||
    process.env.CLAWHIP_HOOK_EVENT ||
    'unknown';

  const projectMetadataPath = findProjectMetadata(cwd);
  const projectMetadata = projectMetadataPath ? readJson(projectMetadataPath) : null;
  const repoPath = input.repo_path || detectRepoPath(cwd);
  const repoName =
    input.repo_name ||
    projectMetadata?.repo_name ||
    projectMetadata?.project ||
    repoPath.split('/').filter(Boolean).pop() ||
    cwd.split('/').filter(Boolean).pop() ||
    'unknown';
  const worktreePath = input.worktree_path || cwd;
  const project = input.project || projectMetadata?.project || repoName;
  const basePayload = {
    provider,
    event_name: eventName,
    directory: input.cwd || input.directory || cwd,
    repo_path: repoPath,
    worktree_path: worktreePath,
    repo_name: repoName,
    project,
    project_identity: projectMetadata || undefined,
    session_id: input.session_id || input.sessionId || undefined,
    turn_id: input.turn_id || input.turnId || undefined,
    tool_name: input.tool_name || input.toolName || undefined,
    command: input.tool_input?.command || input.command || undefined,
    prompt: input.prompt || undefined,
    model: input.model || undefined,
    event_payload: input,
  };

  const augmentation = await loadAugmentation(basePayload, projectMetadataPath, projectMetadata);
  const forwardPayload = augmentation
    ? { ...basePayload, augmentation }
    : basePayload;

  spawnSync('clawhip', ['native', 'hook', '--provider', provider], {
    input: JSON.stringify(forwardPayload),
    encoding: 'utf8',
    stdio: ['pipe', 'ignore', 'ignore'],
  });

  console.log(JSON.stringify({ continue: true, suppressOutput: true }));
}

main().catch(() => {
  console.log(JSON.stringify({ continue: true, suppressOutput: true }));
});
"#
}

impl NativeProvider {
    fn installs_claude(self) -> bool {
        matches!(self, Self::Claude | Self::All)
    }

    fn installs_codex(self) -> bool {
        matches!(self, Self::Codex | Self::All)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn native_hook_maps_all_shared_events() {
        let cases = [
            ("SessionStart", "session.started"),
            ("PreToolUse", "session.pre-tool-use"),
            ("PostToolUse", "session.post-tool-use"),
            ("UserPromptSubmit", "session.user-prompt-submit"),
            ("Stop", "session.finished"),
        ];

        for (event_name, expected_kind) in cases {
            let event = incoming_event_from_native_hook_json(&json!({
                "provider": "claude-code",
                "repo_path": "/repo/clawhip",
                "worktree_path": "/repo/clawhip/.worktrees/issue-1",
                "repo_name": "clawhip",
                "project": "clawhip",
                "event_name": event_name,
                "event_payload": {"session_id": "sess-1"}
            }))
            .expect("event");
            assert_eq!(event.kind, expected_kind);
            assert_eq!(event.payload["provider"], json!("claude"));
            assert_eq!(event.payload["repo_name"], json!("clawhip"));
        }
    }

    #[test]
    fn loads_project_metadata_from_directory() {
        let dir = tempdir().expect("tempdir");
        let project_root = dir.path().join("repo");
        fs::create_dir_all(project_root.join(".clawhip")).unwrap();
        fs::create_dir_all(project_root.join("nested/worktree")).unwrap();
        fs::write(
            project_root.join(PROJECT_METADATA),
            serde_json::to_string(&json!({
                "project": "realign-hooks",
                "repo_name": "clawhip"
            }))
            .unwrap(),
        )
        .unwrap();

        let event = incoming_event_from_native_hook_json(&json!({
            "provider": "codex",
            "directory": project_root.join("nested/worktree").display().to_string(),
            "event_name": "Stop",
            "event_payload": {}
        }))
        .expect("event");

        assert_eq!(event.payload["project"], json!("realign-hooks"));
        assert_eq!(event.payload["repo_name"], json!("clawhip"));
        assert_eq!(
            event.payload["project_identity"]["project"],
            json!("realign-hooks")
        );
    }

    #[test]
    fn augmentation_cannot_override_core_fields() {
        let event = incoming_event_from_native_hook_json(&json!({
            "provider": "codex",
            "directory": "/repo/clawhip",
            "event_name": "UserPromptSubmit",
            "event_payload": {},
            "augmentation": {
                "provider": "evil",
                "repo_name": "evil",
                "summary": "extra",
                "context": {
                    "custom": true,
                    "provider": "evil"
                }
            }
        }))
        .expect("event");

        assert_eq!(event.payload["provider"], json!("codex"));
        assert_eq!(event.payload["repo_name"], json!("clawhip"));
        assert_eq!(event.payload["summary"], json!("extra"));
        assert_eq!(event.payload["context"]["provider"], json!("codex"));
        assert_eq!(event.payload["context"]["custom"], json!(true));
    }

    #[test]
    fn install_project_scope_writes_provider_native_files() {
        let dir = tempdir().expect("tempdir");
        let report = install_with_paths(
            &NativeInstallArgs {
                provider: NativeProvider::All,
                scope: NativeInstallScope::Project,
                root: Some(dir.path().to_path_buf()),
            },
            None,
        )
        .expect("install");

        assert!(report.project_metadata.is_file());
        assert!(report.hook_script.is_file());
        assert!(report.augment_script.is_file());
        assert!(report.claude_settings.expect("claude settings").is_file());
        assert!(report.codex_config.expect("codex config").is_file());
        assert!(report.codex_hooks.expect("codex hooks").is_file());

        let codex_hooks: Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join(CODEX_HOOKS)).unwrap())
                .unwrap();
        assert!(codex_hooks["hooks"]["SessionStart"].is_array());
        assert!(codex_hooks["hooks"]["Stop"].is_array());
        assert!(
            fs::read_to_string(dir.path().join(CODEX_CONFIG))
                .unwrap()
                .contains("codex_hooks = true")
        );
    }

    #[test]
    fn install_global_scope_writes_user_level_provider_files() {
        let home = tempdir().expect("tempdir");
        let report = install_with_paths(
            &NativeInstallArgs {
                provider: NativeProvider::Claude,
                scope: NativeInstallScope::Global,
                root: None,
            },
            Some(home.path()),
        )
        .expect("install");

        assert_eq!(report.root, home.path());
        assert!(home.path().join(CLAUDE_SETTINGS).is_file());
        assert!(home.path().join(HOOK_SCRIPT).is_file());
        assert!(!home.path().join(CODEX_HOOKS).exists());
    }
}
