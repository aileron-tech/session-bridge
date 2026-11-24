use anyhow::{Context, Result};
use session_bridge::{
    client,
    daemon::{self, Config},
    discovery::{self, Sources},
    domain::Provider,
    install, mcp,
    protocol::Request,
};
use std::{
    env,
    path::{Path, PathBuf},
};

fn executable(name: &str) -> Result<PathBuf> {
    env::split_paths(&env::var_os("PATH").context("PATH is not set")?)
        .flat_map(|dir| {
            // Windows resolves commands through executable extensions.
            let extensions: &[&str] = if cfg!(windows) {
                &[".exe", ".cmd", ".bat"]
            } else {
                &[""]
            };
            extensions
                .iter()
                .map(move |extension| dir.join(format!("{name}{extension}")))
        })
        .find(|p| p.is_file())
        .with_context(|| format!("{name} is not on PATH"))
}
fn config(home: &Path) -> Result<Config> {
    Ok(Config {
        state: home.join(".local/state/session-bridge"),
        sources: Sources {
            codex_home: env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex")),
            claude_home: env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude")),
        },
        codex: match env::var_os("SESSION_BRIDGE_CODEX_BIN") {
            Some(path) => PathBuf::from(path),
            // Windows often installs Codex as a .cmd shim, which needs its full path.
            None if cfg!(windows) => executable("codex").unwrap_or_else(|_| PathBuf::from("codex")),
            None => PathBuf::from("codex"),
        },
    })
}
async fn sender(config: &Config) -> Result<String> {
    if let Ok(id) = env::var("CODEX_THREAD_ID") {
        return Ok(format!("codex:{id}"));
    }
    if let Ok(id) = env::var("CLAUDE_CODE_SESSION_ID") {
        return Ok(format!("claude:{id}"));
    }
    let ancestors = discovery::ancestor_pids().await?;
    let sessions = discovery::discover(&config.sources).await?;
    ancestors
        .iter()
        .find_map(|pid| {
            sessions
                .iter()
                .find(|s| s.pid == *pid && s.agent == Provider::Claude)
                .map(|s| s.session_id.clone())
        })
        .context("No caller session identity; run send from Codex or Claude's shell tool")
}
#[tokio::main]
async fn main() -> Result<()> {
    run().await
}

async fn run() -> Result<()> {
    let home_variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let home = PathBuf::from(
        env::var_os(home_variable).with_context(|| format!("{home_variable} is not set"))?,
    );
    let config = config(&home)?;
    let args: Vec<_> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("daemon") => {
            #[cfg(windows)]
            if args.get(1).map(String::as_str) == Some("--background") {
                session_bridge::store::ensure_private_dir(&config.state)?;
                session_bridge::platform::detach_to_log(&config.state.join("daemon.stderr.log"))?;
            }
            daemon::run(config).await
        }
        Some("mcp") => mcp::serve(config).await,
        Some("native-peer") => session_bridge::native_peers::serve(config).await,
        Some("install") => install::install(&config, &home, &executable("claude")?).await,
        Some("list") if args.len() == 1 || (args.len() == 3 && args[1] == "--project") => {
            let reply = client::call(
                &config,
                &Request::List {
                    sender: None,
                    project: Some(match args.get(2) {
                        Some(project) => project.clone(),
                        None => env::current_dir()?
                            .to_str()
                            .context("Current directory is not valid UTF-8")?
                            .to_owned(),
                    }),
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&reply)?);
            Ok(())
        }
        Some("status") => {
            let reply = client::call(&config, &Request::Status).await?;
            println!("{}", serde_json::to_string_pretty(&reply)?);
            Ok(())
        }
        Some("send") if args.len() == 3 || (args.len() == 5 && args[3] == "--project") => {
            let reply = client::call(
                &config,
                &Request::Send {
                    sender: sender(&config).await?,
                    session_id: args[1].clone(),
                    message: args[2].clone(),
                    project: args.get(4).cloned(),
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&reply)?);
            Ok(())
        }
        _ => {
            println!(
                "session-bridge: install | daemon | mcp | list [--project <name-or-path>] | status | send <session_id> <message> [--project <name-or-path>]"
            );
            Ok(())
        }
    }
}
