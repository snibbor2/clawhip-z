use tokio::process::Command;

use crate::Result;
use crate::cli::TmuxNewArgs;
use crate::client::DaemonClient;
use crate::config::AppConfig;
use crate::monitor::RegisteredTmuxSession;

pub async fn run(args: TmuxNewArgs, config: &AppConfig) -> Result<()> {
    launch_session(&args).await?;
    let registration = RegisteredTmuxSession {
        session: args.session.clone(),
        channel: args.channel.clone(),
        mention: args.mention.clone(),
        keywords: args.keywords.clone(),
        stale_minutes: args.stale_minutes,
        format: args.format.map(Into::into),
    };
    let client = DaemonClient::from_config(config);
    client.register_tmux(&registration).await?;
    if args.attach {
        attach_session(&args.session).await?;
    }
    Ok(())
}

async fn launch_session(args: &TmuxNewArgs) -> Result<()> {
    let mut command = Command::new(tmux_bin());
    command
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(&args.session);
    if let Some(window_name) = &args.window_name {
        command.arg("-n").arg(window_name);
    }
    if let Some(cwd) = &args.cwd {
        command.arg("-c").arg(cwd);
    }
    if !args.command.is_empty() {
        command.arg("--");
        command.args(&args.command);
    }
    let output = command.output().await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into())
    }
}

async fn attach_session(session: &str) -> Result<()> {
    let output = Command::new(tmux_bin())
        .arg("attach-session")
        .arg("-t")
        .arg(session)
        .output()
        .await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into())
    }
}

fn tmux_bin() -> String {
    std::env::var("CLAWHIP_TMUX_BIN").unwrap_or_else(|_| "tmux".to_string())
}
