use anyhow::{Context, Result};
use session_bridge::{
    client,
    daemon::{self, Config},
    discovery::{self, Sources},
    domain::Provider,
    install,
    lan::model::{PairingCode, PairingOutcome},
    mcp,
    protocol::{Reply, Request},
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
    // A bridge run from a shell may use a state directory of its own, beside
    // the installed service (REQ-021).
    let custom = env::var_os("SESSION_BRIDGE_STATE").map(PathBuf::from);
    if let Some(state) = &custom {
        anyhow::ensure!(
            state.is_absolute(),
            "SESSION_BRIDGE_STATE must be an absolute path: {}",
            state.display()
        );
    }
    Ok(Config {
        supervised: custom.is_none(),
        state: custom.unwrap_or_else(|| home.join(".local").join("state").join("session-bridge")),
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
        notify: env::var("SESSION_BRIDGE_NOTIFY").map_or(true, |v| v != "0"),
        tray: env::var("SESSION_BRIDGE_TRAY").is_ok_and(|v| v == "1"),
        releases: env::var("SESSION_BRIDGE_RELEASES_URL")
            .unwrap_or_else(|_| session_bridge::update::fetch::LATEST.into()),
        crontab: env::var("SESSION_BRIDGE_CRONTAB").unwrap_or_else(|_| "crontab".into()),
        ssh: env::var("SESSION_BRIDGE_SSH").unwrap_or_else(|_| "ssh".into()),
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
/// `network on [--port <n>] [--name <host>]` or `network off`.
async fn network(config: &Config, args: &[String]) -> Result<()> {
    let enabled = match args.first().map(String::as_str) {
        Some("on") => true,
        Some("off") if args.len() == 1 => false,
        _ => anyhow::bail!(
            "Usage: session-bridge network on [--port <n>] [--name <host>] [--local-only] | network off"
        ),
    };
    let (mut port, mut name, mut local_only) = (None, None, None);
    let mut options = args[1..].iter();
    while let Some(option) = options.next() {
        if option == "--local-only" {
            local_only = Some(true);
            continue;
        }
        let value = options
            .next()
            .with_context(|| format!("{option} needs a value"))?;
        match option.as_str() {
            "--port" => {
                port = Some(
                    value
                        .parse()
                        .context("--port needs a number from 0 to 65535")?,
                )
            }
            "--name" => name = Some(value.clone()),
            other => anyhow::bail!("Unknown option {other}"),
        }
    }
    let reply = client::call(
        config,
        &Request::Network {
            enabled,
            port,
            name,
            local_only,
        },
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&reply)?);
    Ok(())
}
/// `remote add <ssh-host> [--port <n>]`, `remote list`, `remote remove <ssh-host>` (REQ-027).
async fn remote(config: &Config, args: &[String]) -> Result<()> {
    let request = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["list"] | [] => Request::Remotes,
        ["add", host] => Request::AddRemote {
            host: (*host).into(),
            port: None,
        },
        ["add", host, "--port", port] => Request::AddRemote {
            host: (*host).into(),
            port: Some(
                port.parse()
                    .context("--port needs a number from 1 to 65535")?,
            ),
        },
        ["remove", host] => Request::RemoveRemote {
            host: (*host).into(),
        },
        _ => anyhow::bail!(
            "Usage: session-bridge remote add <ssh-host> [--port <n>] | remote list | remote remove <ssh-host>"
        ),
    };
    let reply = client::call(config, &request).await?;
    println!("{}", serde_json::to_string_pretty(&reply)?);
    Ok(())
}
async fn read_line() -> Result<Option<String>> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line)? {
            0 => Ok(None),
            _ => Ok(Some(line.trim().to_owned())),
        }
    })
    .await?
}
/// The laptop that chooses: ask `host` to show a code, then type it.
async fn pair_with(config: &Config, host: &str) -> Result<()> {
    let Reply::Pairing {
        id,
        host,
        expires_in,
    } = client::call(
        config,
        &Request::Pair {
            host: host.to_owned(),
        },
    )
    .await?
    else {
        anyhow::bail!("Unexpected bridge reply");
    };
    eprintln!("{host} now shows a pairing code (valid {expires_in} s).");
    loop {
        eprint!("Enter the code: ");
        let line = read_line().await?.context("No code entered")?;
        if PairingCode::parse(&line).is_err() {
            eprintln!("Expected 6 digits such as 482-913.");
            continue;
        }
        match client::call(config, &Request::PairCode { id, code: line }).await? {
            Reply::Paired { host, key } => {
                println!("Paired with {host} (key {key})");
                return Ok(());
            }
            Reply::WrongCode { attempts_left } => {
                eprintln!("Wrong code; {attempts_left} attempts left");
            }
            other => anyhow::bail!("Unexpected bridge reply: {other:?}"),
        }
    }
}
/// The chosen laptop: show incoming codes until a pairing finishes.
async fn show_pairing(config: &Config) -> Result<()> {
    let view = |reply| match reply {
        Reply::Lan { lan } => Ok(lan),
        other => Err(anyhow::anyhow!("Unexpected bridge reply: {other:?}")),
    };
    let start = view(client::call(config, &Request::Lan).await?)?;
    anyhow::ensure!(
        start.network.port.is_some(),
        "Networking is off; run session-bridge network on"
    );
    println!(
        "Waiting for a pairing request. On the other laptop run: session-bridge pair {}",
        start.network.host
    );
    let mut shown = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
    while tokio::time::Instant::now() < deadline {
        let lan = view(client::call(config, &Request::Lan).await?)?;
        if lan.pairing.last != start.pairing.last {
            match lan.pairing.last {
                Some(PairingOutcome::Paired { host, key }) => {
                    println!("Paired with {host} (key {key})");
                    return Ok(());
                }
                Some(PairingOutcome::Failed { host, reason }) => {
                    anyhow::bail!("Pairing with {host} failed: {reason}")
                }
                None => {}
            }
        }
        if let Some(incoming) = lan.pairing.incoming
            && shown.as_ref() != Some(&incoming.code)
        {
            println!(
                "{} ({}) wants to pair. Code {} (expires in {} s)",
                incoming.host, incoming.address, incoming.code, incoming.expires_in
            );
            shown = Some(incoming.code);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    anyhow::bail!("No pairing finished within 5 minutes")
}
async fn open_browser(url: &str) {
    let mut command = if cfg!(target_os = "macos") {
        tokio::process::Command::new("open")
    } else if cfg!(windows) {
        let mut command = tokio::process::Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else {
        tokio::process::Command::new("xdg-open")
    };
    match command.arg(url).status().await {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("Could not open a browser ({status}); open the address above"),
        Err(e) => eprintln!("Could not open a browser ({e}); open the address above"),
    }
}

fn main() -> Result<()> {
    // The menu-bar icon needs the main thread for AppKit, so it starts before
    // any async runtime (REQ-022).
    #[cfg(target_os = "macos")]
    if env::args().nth(1).as_deref() == Some("tray") {
        let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
        return session_bridge::tray::app::run(config(&home)?, home);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
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
        Some("install") => {
            // A separate bridge is refused before anything else is looked up.
            install::default_state_only(&config)?;
            install::install(&config, &home, &executable("claude")?).await
        }
        Some("uninstall") if args.len() == 1 => install::uninstall(&config, &home).await,
        Some("update") if args.len() == 1 => {
            println!(
                "{}",
                session_bridge::update::fetch::update(&config, &home).await?
            );
            Ok(())
        }
        Some("update") if args.len() == 2 && args[1] == "--check" => {
            match session_bridge::update::fetch::check(&config).await? {
                session_bridge::update::Plan::UpToDate { version } => {
                    println!("session-bridge {version} is up to date")
                }
                session_bridge::update::Plan::Update { version, .. } => println!(
                    "Update available: {version} (running {}). Install it with: session-bridge update",
                    env!("CARGO_PKG_VERSION")
                ),
            }
            Ok(())
        }
        Some("remote") => remote(&config, &args[1..]).await,
        Some("scheduled") if args.len() == 2 => {
            let id = uuid::Uuid::parse_str(&args[1]).context("Expected a schedule id")?;
            let reply = client::call(&config, &Request::RunSchedule { id }).await?;
            println!("{}", serde_json::to_string_pretty(&reply)?);
            Ok(())
        }
        Some("stop") if args.len() == 1 => {
            println!("{}", install::stop(&config, &home).await?);
            Ok(())
        }
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
        Some("network") => network(&config, &args[1..]).await,
        Some("discover") if args.len() == 1 => {
            let Reply::Lan { lan } = client::call(&config, &Request::Lan).await? else {
                anyhow::bail!("Unexpected bridge reply");
            };
            println!("{}", serde_json::to_string_pretty(&lan.hosts)?);
            Ok(())
        }
        Some("pair") if args.len() == 2 => pair_with(&config, &args[1]).await,
        Some("pair") if args.len() == 1 => show_pairing(&config).await,
        Some("unpair") if args.len() == 2 => {
            let reply = client::call(
                &config,
                &Request::Unpair {
                    host: args[1].clone(),
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&reply)?);
            Ok(())
        }
        Some("admin") if args.len() <= 2 => {
            let Reply::Admin { url } = client::call(&config, &Request::Admin).await? else {
                anyhow::bail!("Unexpected bridge reply");
            };
            println!("{url}");
            if args.get(1).map(String::as_str) != Some("--no-open") {
                open_browser(&url).await;
            }
            Ok(())
        }
        _ => {
            println!(
                "session-bridge: install | uninstall | daemon | stop | update [--check] | remote add <ssh-host> [--port <n>] | remote list | remote remove <ssh-host> | mcp | list [--project <name-or-path>] | status | send <session_id> <message> [--project <name-or-path>] | network on [--port <n>] [--name <host>] | network off | discover | pair [<host>] | unpair <host> | admin [--no-open]\n  daemon runs the bridge in this shell until Ctrl+C; SESSION_BRIDGE_STATE=<absolute dir> runs a separate one"
            );
            Ok(())
        }
    }
}
