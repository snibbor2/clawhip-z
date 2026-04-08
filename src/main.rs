mod cli;
mod client;
mod config;
mod core;
mod cron;
mod daemon;
mod discord;
mod dispatch;
mod dynamic_tokens;
mod event;
mod events;
mod hooks;
mod keyword_window;
mod lifecycle;
mod memory;
mod native_hooks;
mod plugins;
mod render;
mod router;
mod sink;
mod slack;
mod source;
mod summarize;
mod tmux_wrapper;
mod update;

use std::sync::Arc;

use clap::Parser;

use crate::cli::{
    AgentCommands, Cli, Commands, ConfigCommand, CronCommands, GitCommands, GithubCommands,
    HooksCommands, MemoryCommands, NativeCommands, PluginCommands, TmuxCommands, UpdateCommands,
};
use crate::client::DaemonClient;
use crate::config::AppConfig;
use crate::event::compat::from_incoming_event;
use crate::events::IncomingEvent;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub type DynError = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, DynError>;

#[tokio::main]
async fn main() {
    if let Err(error) = real_main().await {
        eprintln!("clawhip error: {error}");
        std::process::exit(1);
    }
}

fn prepare_event(event: IncomingEvent) -> Result<IncomingEvent> {
    let event = crate::events::normalize_event(event);
    let _typed = from_incoming_event(&event)?;
    Ok(event)
}

async fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config_path();
    let config = Arc::new(AppConfig::load_or_default(&config_path)?);
    let cron_state_path = crate::cron::default_state_path(&config_path);

    match cli.command.unwrap_or(Commands::Start { port: None }) {
        Commands::Start { port } => daemon::run(config, port, cron_state_path).await,
        Commands::Status => {
            let client = DaemonClient::from_config(config.as_ref());
            let health = client.health().await?;
            println!("{}", serde_json::to_string_pretty(&health)?);
            Ok(())
        }
        Commands::Emit(args) => {
            let client = DaemonClient::from_config(config.as_ref());
            send_incoming_event(&client, args.into_event()?).await
        }
        Commands::Setup { webhook } => {
            let mut editable = AppConfig::load_or_default(&config_path)?;
            editable.scaffold_webhook_quickstart(webhook);
            editable.validate()?;
            editable.save(&config_path)?;
            println!("Saved {}", config_path.display());
            Ok(())
        }
        Commands::Send { channel, message } => {
            let client = DaemonClient::from_config(config.as_ref());
            send_incoming_event(&client, IncomingEvent::custom(channel, message)).await
        }
        Commands::Git { command } => {
            let client = DaemonClient::from_config(config.as_ref());
            let event = match command {
                GitCommands::Commit {
                    repo,
                    branch,
                    commit,
                    summary,
                    channel,
                } => IncomingEvent::git_commit(repo, branch, commit, summary, channel),
                GitCommands::BranchChanged {
                    repo,
                    old_branch,
                    new_branch,
                    channel,
                } => IncomingEvent::git_branch_changed(repo, old_branch, new_branch, channel),
            };
            send_incoming_event(&client, event).await
        }
        Commands::Github { command } => {
            let client = DaemonClient::from_config(config.as_ref());
            let event = match command {
                GithubCommands::IssueOpened {
                    repo,
                    number,
                    title,
                    channel,
                } => IncomingEvent::github_issue_opened(repo, number, title, channel),
                GithubCommands::PrStatusChanged {
                    repo,
                    number,
                    title,
                    old_status,
                    new_status,
                    url,
                    channel,
                } => IncomingEvent::github_pr_status_changed(
                    repo, number, title, old_status, new_status, url, channel,
                ),
            };
            send_incoming_event(&client, event).await
        }
        Commands::Agent { command } => {
            let client = DaemonClient::from_config(config.as_ref());
            let event = match command {
                AgentCommands::Started(args) => IncomingEvent::agent_started(
                    args.agent_name,
                    args.session_id,
                    args.project,
                    args.elapsed_secs,
                    args.summary,
                    args.mention,
                    args.channel,
                ),
                AgentCommands::Blocked(args) => IncomingEvent::agent_blocked(
                    args.agent_name,
                    args.session_id,
                    args.project,
                    args.elapsed_secs,
                    args.summary,
                    args.mention,
                    args.channel,
                ),
                AgentCommands::Finished(args) => IncomingEvent::agent_finished(
                    args.agent_name,
                    args.session_id,
                    args.project,
                    args.elapsed_secs,
                    args.summary,
                    args.mention,
                    args.channel,
                ),
                AgentCommands::Failed(args) => IncomingEvent::agent_failed(
                    args.event.agent_name,
                    args.event.session_id,
                    args.event.project,
                    args.event.elapsed_secs,
                    args.event.summary,
                    args.error_message,
                    args.event.mention,
                    args.event.channel,
                ),
            };
            send_incoming_event(&client, event).await
        }
        Commands::Install {
            systemd,
            skip_star_prompt,
        } => lifecycle::install(systemd, skip_star_prompt),
        Commands::Update { command, restart } => match command {
            None => lifecycle::update(restart),
            Some(UpdateCommands::Check) => {
                let http = reqwest::Client::builder()
                    .user_agent(format!("clawhip/{VERSION}"))
                    .build()?;
                match update::check_latest_version(&http).await {
                    Ok(Some((version, url))) => {
                        if update::version_is_newer(&version) {
                            println!("Update available: v{VERSION} -> {version}\n{url}");
                        } else {
                            println!("Already up to date (v{VERSION})");
                        }
                    }
                    Ok(None) => println!("No releases found"),
                    Err(error) => eprintln!("Check failed: {error}"),
                }
                Ok(())
            }
            Some(UpdateCommands::Approve) => {
                let client = DaemonClient::from_config(config.as_ref());
                let result = client.post_update_action("approve").await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
                Ok(())
            }
            Some(UpdateCommands::Dismiss) => {
                let client = DaemonClient::from_config(config.as_ref());
                let result = client.post_update_action("dismiss").await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
                Ok(())
            }
            Some(UpdateCommands::Status) => {
                let client = DaemonClient::from_config(config.as_ref());
                let result = client.get_update_status().await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
                Ok(())
            }
        },
        Commands::Uninstall {
            remove_systemd,
            remove_config,
        } => lifecycle::uninstall(remove_systemd, remove_config),
        Commands::Tmux { command } => match command {
            TmuxCommands::Keyword {
                session,
                keyword,
                line,
                channel,
            } => {
                let client = DaemonClient::from_config(config.as_ref());
                send_incoming_event(
                    &client,
                    IncomingEvent::tmux_keyword(session, keyword, line, channel),
                )
                .await
            }
            TmuxCommands::Stale {
                session,
                pane,
                minutes,
                last_line,
                channel,
            } => {
                let client = DaemonClient::from_config(config.as_ref());
                send_incoming_event(
                    &client,
                    IncomingEvent::tmux_stale(session, pane, minutes, last_line, channel),
                )
                .await
            }
            TmuxCommands::New(args) => tmux_wrapper::run(args, config.as_ref()).await,
            TmuxCommands::Watch(args) => tmux_wrapper::watch(args, config.as_ref()).await,
            TmuxCommands::List(args) => {
                let client = DaemonClient::from_config(config.as_ref());
                let registrations = client.list_tmux().await?;
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&registrations)?);
                } else {
                    render_tmux_list(&registrations, args.compact);
                }
                Ok(())
            }
        },
        Commands::Native { command } => match command {
            NativeCommands::Hook(args) => {
                let client = DaemonClient::from_config(config.as_ref());
                let mut payload = args.read_payload(&mut std::io::stdin())?;
                if let Some(provider) = args.provider.as_deref()
                    && payload.get("provider").is_none()
                    && let Some(object) = payload.as_object_mut()
                {
                    object.insert("provider".into(), serde_json::json!(provider));
                }
                if let Some(source) = args.source.as_deref()
                    && payload.get("source").is_none()
                    && let Some(object) = payload.as_object_mut()
                {
                    object.insert("source".into(), serde_json::json!(source));
                }
                let response = client.send_native_hook(&payload).await?;
                println!("{}", serde_json::to_string(&response)?);
                Ok(())
            }
        },
        Commands::Cron { command } => match command {
            CronCommands::Run { id } => {
                crate::cron::run_configured_job(config.as_ref(), &id).await?;
                println!("Ran cron job {id}");
                Ok(())
            }
        },
        Commands::Config { command } => match command.unwrap_or(ConfigCommand::Interactive) {
            ConfigCommand::Interactive => {
                let mut editable = AppConfig::load_or_default(&config_path)?;
                editable.run_interactive_editor(&config_path)
            }
            ConfigCommand::Show => {
                println!("{}", config.to_pretty_toml()?);
                Ok(())
            }
            ConfigCommand::Path => {
                println!("{}", config_path.display());
                Ok(())
            }
        },
        Commands::Plugin { command } => match command {
            PluginCommands::List => {
                let plugins_dir = plugins::default_plugins_dir()?;
                let discovered = plugins::load_plugins(&plugins_dir)?;

                if discovered.is_empty() {
                    println!("No plugins found in {}", plugins_dir.display());
                    return Ok(());
                }

                println!("NAME\tBRIDGE\tDESCRIPTION");
                for plugin in discovered {
                    println!(
                        "{}\t{}\t{}",
                        plugin.name,
                        plugin.bridge_path.display(),
                        plugin.description.as_deref().unwrap_or("-"),
                    );
                }
                Ok(())
            }
        },
        Commands::Memory { command } => match command {
            MemoryCommands::Init(args) => memory::init(args),
            MemoryCommands::Status(args) => memory::status(args),
        },
        Commands::Hooks { command } => match command {
            HooksCommands::Install(args) => hooks::install(args),
        },
    }
}

async fn send_incoming_event(client: &DaemonClient, event: IncomingEvent) -> Result<()> {
    let event = prepare_event(event)?;
    client.send_event(&event).await
}

fn render_tmux_list(registrations: &[crate::source::RegisteredTmuxSession], compact: bool) {
    print!("{}", format_tmux_list(registrations, compact));
}

fn format_tmux_list(
    registrations: &[crate::source::RegisteredTmuxSession],
    compact: bool,
) -> String {
    use crate::source::RegisteredTmuxSession;
    use time::format_description::well_known::Rfc3339;

    if registrations.is_empty() {
        return "No active tmux sessions registered.\n".to_string();
    }

    let col_session = registrations
        .iter()
        .map(|r| r.session.len())
        .max()
        .unwrap_or(7)
        .max(7);
    let col_status = registrations
        .iter()
        .map(|r| format_live_status(r).len())
        .max()
        .unwrap_or(8)
        .max(8);
    let col_channel = registrations
        .iter()
        .map(|r| r.channel.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(7)
        .max(7);
    let col_features = registrations
        .iter()
        .map(|r| format_features(r).len())
        .max()
        .unwrap_or(8)
        .max(8);

    let mut output = String::new();

    if compact {
        let col_registered = 12usize;
        output.push_str(&format!(
            "{:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {}\n",
            "SESSION",
            "STATUS",
            "CHANNEL",
            "FEATURES",
            "REGISTERED",
            w1 = col_session,
            w2 = col_status,
            w3 = col_channel,
            w4 = col_features,
        ));
        let total = col_session + col_status + col_channel + col_features + col_registered + 10;
        output.push_str(&"\u{2500}".repeat(total));
        output.push('\n');
        for r in registrations {
            output.push_str(&format!(
                "{:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {}\n",
                r.session,
                format_live_status(r),
                r.channel.as_deref().unwrap_or("-"),
                format_features(r),
                format_relative_time(&r.registered_at),
                w1 = col_session,
                w2 = col_status,
                w3 = col_channel,
                w4 = col_features,
            ));
        }
        return output;
    }

    // Full view — all columns
    let col_keywords = registrations
        .iter()
        .map(|r| {
            if r.keywords.is_empty() {
                1
            } else {
                r.keywords.join(",").len()
            }
        })
        .max()
        .unwrap_or(8)
        .max(8);
    let col_mention = registrations
        .iter()
        .map(|r| r.mention.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(7)
        .max(7);
    let col_stale = 5usize;
    let col_source = registrations
        .iter()
        .map(|r| r.registration_source.as_str().len())
        .max()
        .unwrap_or(6)
        .max(6);
    let col_registered = 12usize;
    let col_parent = registrations
        .iter()
        .map(|r| {
            r.parent_process
                .as_ref()
                .map(|p| format!("{}:{}", p.pid, p.name.as_deref().unwrap_or("?")).len())
                .unwrap_or(1)
        })
        .max()
        .unwrap_or(6)
        .max(6);

    output.push_str(&format!(
        "{:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {:<w5$}  {:<w6$}  {:<w7$}  {:<w8$}  {:<w9$}  {}\n",
        "SESSION",
        "STATUS",
        "CHANNEL",
        "KEYWORDS",
        "MENTION",
        "STALE",
        "FEATURES",
        "SOURCE",
        "REGISTERED",
        "PARENT",
        w1 = col_session,
        w2 = col_status,
        w3 = col_channel,
        w4 = col_keywords,
        w5 = col_mention,
        w6 = col_stale,
        w7 = col_features,
        w8 = col_source,
        w9 = col_registered,
    ));
    let total = col_session
        + col_status
        + col_channel
        + col_keywords
        + col_mention
        + col_stale
        + col_features
        + col_source
        + col_registered
        + col_parent
        + 20;
    output.push_str(&"\u{2500}".repeat(total));
    output.push('\n');

    for r in registrations {
        let keywords = if r.keywords.is_empty() {
            "-".to_string()
        } else {
            r.keywords.join(",")
        };
        let mention = r.mention.as_deref().unwrap_or("-");
        let parent = r
            .parent_process
            .as_ref()
            .map(|p| format!("{}:{}", p.pid, p.name.as_deref().unwrap_or("?")))
            .unwrap_or_else(|| "-".to_string());
        output.push_str(&format!(
            "{:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {:<w5$}  {:<w6$}  {:<w7$}  {:<w8$}  {:<w9$}  {}\n",
            r.session,
            format_live_status(r),
            r.channel.as_deref().unwrap_or("-"),
            keywords,
            mention,
            r.stale_minutes,
            format_features(r),
            r.registration_source.as_str(),
            format_relative_time(&r.registered_at),
            parent,
            w1 = col_session,
            w2 = col_status,
            w3 = col_channel,
            w4 = col_keywords,
            w5 = col_mention,
            w6 = col_stale,
            w7 = col_features,
            w8 = col_source,
            w9 = col_registered,
        ));
    }

    return output;

    fn format_live_status(r: &RegisteredTmuxSession) -> String {
        match &r.live_state {
            None => "\u{003F} unknown".to_string(),
            Some(ls) => {
                if ls.pane_dead {
                    return "\u{2715} dead".to_string();
                }
                if ls.is_waiting {
                    return "\u{26A0} waiting".to_string();
                }
                if let Some(last) = &ls.last_activity
                    && let Ok(dt) = time::OffsetDateTime::parse(last, &Rfc3339)
                {
                    let duration: time::Duration = time::OffsetDateTime::now_utc() - dt;
                    let elapsed_secs = duration.whole_seconds().max(0) as u64;
                    let stale_secs = r.stale_minutes * 60;
                    if stale_secs > 0 && elapsed_secs >= stale_secs {
                        let idle_mins = elapsed_secs / 60;
                        return format!("\u{25CC} idle {}m", idle_mins);
                    }
                    return "\u{25CF} active".to_string();
                }
                "\u{25CC} idle".to_string()
            }
        }
    }

    fn format_features(r: &RegisteredTmuxSession) -> String {
        let mut parts = Vec::new();
        let heartbeat = r.heartbeat_interval.max(r.heartbeat_mins);
        if heartbeat > 0.0 {
            parts.push("\u{2665}");
        }
        if r.summarize {
            parts.push("sum");
        }
        if r.detect_waiting {
            parts.push("wait");
        }
        let has_pins =
            r.pin_status || r.pin_summary || r.pin_alerts || r.pin_activity || r.pin_keywords;
        if has_pins {
            parts.push("pins");
        }
        if !r.keywords.is_empty() {
            parts.push("kw");
        }
        if parts.is_empty() {
            "\u{2014}".to_string()
        } else {
            parts.join(" ")
        }
    }

    fn format_relative_time(rfc3339: &str) -> String {
        if let Ok(dt) = time::OffsetDateTime::parse(rfc3339, &Rfc3339) {
            let duration: time::Duration = time::OffsetDateTime::now_utc() - dt;
            let secs = duration.whole_seconds().max(0) as u64;
            if secs < 120 {
                return format!("{}s ago", secs);
            }
            let mins = secs / 60;
            if mins < 120 {
                return format!("{}m ago", mins);
            }
            let hours = mins / 60;
            if hours < 48 {
                return format!("{}h ago", hours);
            }
            return format!("{}d ago", hours / 24);
        }
        rfc3339.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::format_tmux_list;
    use crate::source::tmux::{RegisteredTmuxSession, RegistrationSource, SessionLiveState};

    fn make_registration(session: &str) -> RegisteredTmuxSession {
        RegisteredTmuxSession {
            session: session.into(),
            channel: Some("alerts".into()),
            stale_minutes: 10,
            registered_at: "2026-04-02T00:00:00Z".into(),
            registration_source: RegistrationSource::CliWatch,
            ..Default::default()
        }
    }

    #[test]
    fn format_tmux_list_renders_new_columns() {
        let mut reg = make_registration("issue-105");
        reg.keywords = vec!["error".into()];
        reg.summarize = true;
        reg.heartbeat_interval = 5;
        reg.detect_waiting = true;
        reg.live_state = Some(SessionLiveState {
            is_waiting: false,
            pane_dead: false,
            last_activity: Some(
                time::OffsetDateTime::now_utc()
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap(),
            ),
            last_poll: None,
        });
        let output = format_tmux_list(&[reg], false);

        assert!(output.contains("SESSION"));
        assert!(output.contains("STATUS"));
        assert!(output.contains("CHANNEL"));
        assert!(output.contains("FEATURES"));
        assert!(output.contains("REGISTERED"));
        assert!(output.contains("issue-105"));
        assert!(output.contains("active"));
        assert!(output.contains("alerts"));
        assert!(output.contains("sum"));
        assert!(output.contains("wait"));
        assert!(output.contains("kw"));
    }

    #[test]
    fn format_tmux_list_handles_empty_registry() {
        assert_eq!(
            format_tmux_list(&[], false),
            "No active tmux sessions registered.\n"
        );
    }

    #[test]
    fn format_features_shows_dash_when_no_features() {
        let reg = RegisteredTmuxSession {
            session: "plain".into(),
            registered_at: "2026-04-02T00:00:00Z".into(),
            ..Default::default()
        };
        let output = format_tmux_list(&[reg], false);
        assert!(output.contains("\u{2014}"));
    }

    #[test]
    fn format_features_shows_heartbeat_and_pins() {
        let mut reg = make_registration("feat-test");
        reg.heartbeat_mins = 5.0;
        reg.pin_status = true;
        reg.live_state = Some(SessionLiveState::default());
        let output = format_tmux_list(&[reg], false);
        assert!(output.contains("\u{2665}"));
        assert!(output.contains("pins"));
    }

    #[test]
    fn format_relative_time_seconds() {
        let now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        let mut reg = make_registration("time-test");
        reg.registered_at = now;
        let output = format_tmux_list(&[reg], false);
        assert!(output.contains("s ago"));
    }

    #[test]
    fn format_live_status_dead() {
        let mut reg = make_registration("dead-test");
        reg.live_state = Some(SessionLiveState {
            is_waiting: false,
            pane_dead: true,
            last_activity: None,
            last_poll: None,
        });
        let output = format_tmux_list(&[reg], false);
        assert!(output.contains("dead"));
    }

    #[test]
    fn format_live_status_waiting() {
        let mut reg = make_registration("wait-test");
        reg.live_state = Some(SessionLiveState {
            is_waiting: true,
            pane_dead: false,
            last_activity: None,
            last_poll: None,
        });
        let output = format_tmux_list(&[reg], false);
        assert!(output.contains("waiting"));
    }

    #[test]
    fn format_live_status_unknown() {
        let reg = make_registration("unknown-test");
        let output = format_tmux_list(&[reg], false);
        assert!(output.contains("unknown"));
    }

    #[test]
    fn serde_skip_deserializing_live_state() {
        let json = r#"{"session":"test","stale_minutes":10,"live_state":{"is_waiting":true,"pane_dead":false}}"#;
        let reg: RegisteredTmuxSession = serde_json::from_str(json).unwrap();
        assert!(
            reg.live_state.is_none(),
            "live_state should be None due to skip_deserializing"
        );
    }
}
