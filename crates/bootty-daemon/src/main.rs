use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use bootty_daemon::catalog::{Backend, Catalog};
use bootty_identity::{APPLICATION_IDENTITY_ENV, ApplicationIdentity};

fn main() -> Result<()> {
    let (identity, args) = parse_application_identity(std::env::args().skip(1).collect())?;
    bootty_mux::prepare_local_rmux_daemon(identity)?;
    if let Some(code) = bootty_mux::run_embedded_rmux_daemon()? {
        std::process::exit(code);
    }
    match args.first().map(String::as_str) {
        Some("remote-ping") => {
            println!(
                "{}:{}",
                bootty_mux::REMOTE_DAEMON_PROTOCOL_VERSION,
                env!("CARGO_PKG_VERSION")
            );
            Ok(())
        }
        Some("remote-exec") => {
            let payload = args.get(1).context("remote-exec requires a payload")?;
            reject_extra(args.iter().skip(2))?;
            std::process::exit(bootty_mux::run_remote_command(payload)?);
        }
        Some("remote-rmux") => {
            let payload = args.get(1).context("remote-rmux requires a payload")?;
            reject_extra(args.iter().skip(2))?;
            std::process::exit(bootty_mux::run_remote_rmux_command(payload)?);
        }
        Some("remote-space") => run_remote_space(&args[1..], identity),
        Some("remote-project") => run_remote_project(&args[1..]),
        Some("remote-worktree") => run_remote_worktree(&args[1..]),
        Some(command) => bail!("unknown command {command:?}"),
        None => bail!("bootty-daemon requires a command"),
    }
}

fn run_remote_space(args: &[String], identity: ApplicationIdentity) -> Result<()> {
    let Some((command, arguments)) = args.split_first() else {
        bail!("remote-space requires a command")
    };
    let mut catalog = Catalog::open(&state_path(identity)?, identity)?;
    match command.as_str() {
        "list" => {
            if !arguments.is_empty() {
                bail!("remote-space list takes no arguments")
            }
            println!("{}", serde_json::to_string(&catalog.list()?)?);
        }
        "create" => {
            let name = required_option(arguments, "--name")?;
            let backend = Backend::parse(&required_option(arguments, "--backend")?)?;
            println!(
                "{}",
                serde_json::to_string(&catalog.create(&name, backend)?)?
            );
        }
        "snapshot" => {
            let id = required_option(arguments, "--id")?;
            let backend = Backend::parse(&required_option(arguments, "--backend")?)?;
            println!(
                "{}",
                serde_json::to_string(&catalog.snapshot(&id, backend)?)?
            );
        }
        "execute" => {
            let id = required_option(arguments, "--id")?;
            let backend = Backend::parse(&required_option(arguments, "--backend")?)?;
            let payload = required_option(arguments, "--payload")?;
            let command = bootty_mux::decode_remote_space_command(&payload)?;
            catalog.execute(&id, backend, command)?;
        }
        _ => bail!("unknown remote-space command {command:?}"),
    }
    Ok(())
}

fn run_remote_project(args: &[String]) -> Result<()> {
    let Some((command, arguments)) = args.split_first() else {
        bail!("remote-project requires a command")
    };
    let home = home_dir();
    match command.as_str() {
        "list" => {
            if !arguments.is_empty() {
                bail!("remote-project list takes no arguments")
            }
            println!(
                "{}",
                serde_json::to_string(&bootty_mux::project::discover_project_picker_entries(
                    home.as_deref()
                ))?
            );
        }
        "favorite" => {
            let path = required_option(arguments, "--path")?;
            println!(
                "{}",
                bootty_mux::project::toggle_favorite_project_path(home.as_deref(), &path)?
            );
        }
        _ => bail!("unknown remote-project command {command:?}"),
    }
    Ok(())
}

fn run_remote_worktree(args: &[String]) -> Result<()> {
    let Some((command, arguments)) = args.split_first() else {
        bail!("remote-worktree requires a command")
    };
    match command.as_str() {
        "list" => {
            let project = required_option(arguments, "--project")?;
            let open_cwds = option_values(arguments, "--open-cwd")?;
            let mut entries = bootty_mux::project::discover_worktree_picker_entries(&project);
            bootty_mux::project::mark_occupied_worktrees(&mut entries, &open_cwds);
            println!("{}", serde_json::to_string(&entries)?);
        }
        "create" => {
            let project = required_option(arguments, "--project")?;
            let branch = required_option(arguments, "--branch")?;
            println!(
                "{}",
                serde_json::to_string(
                    &bootty_mux::project::add_worktree(&project, &branch)
                        .map_err(anyhow::Error::msg)?
                )?
            );
        }
        _ => bail!("unknown remote-worktree command {command:?}"),
    }
    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    bootty_mux::project::home_dir()
}

fn required_option(args: &[String], name: &str) -> Result<String> {
    let mut chunks = args.chunks_exact(2);
    let mut value = None;
    for chunk in &mut chunks {
        if chunk[0] == name && value.replace(chunk[1].clone()).is_some() {
            bail!("duplicate option {name}")
        }
    }
    if !chunks.remainder().is_empty() {
        bail!("options require values")
    }
    value.with_context(|| format!("missing option {name}"))
}
fn option_values(args: &[String], name: &str) -> Result<Vec<String>> {
    let mut chunks = args.chunks_exact(2);
    let values = chunks
        .by_ref()
        .filter(|chunk| chunk[0] == name)
        .map(|chunk| chunk[1].clone())
        .collect();
    if !chunks.remainder().is_empty() {
        bail!("options require values")
    }
    Ok(values)
}

fn reject_extra<'a>(mut args: impl Iterator<Item = &'a String>) -> Result<()> {
    if let Some(extra) = args.next() {
        bail!("unexpected argument {extra:?}")
    }
    Ok(())
}

fn parse_application_identity(args: Vec<String>) -> Result<(ApplicationIdentity, Vec<String>)> {
    let inherited = std::env::var_os(APPLICATION_IDENTITY_ENV);
    parse_application_identity_with_inherited(args, inherited.as_deref())
}

fn parse_application_identity_with_inherited(
    mut args: Vec<String>,
    inherited: Option<&std::ffi::OsStr>,
) -> Result<(ApplicationIdentity, Vec<String>)> {
    if args
        .first()
        .is_some_and(|arg| arg == "--application-identity")
    {
        if args.len() < 2 {
            bail!("--application-identity requires a value")
        }
        let value = args.remove(1);
        let identity = ApplicationIdentity::parse(&value)
            .with_context(|| format!("unknown application identity {value:?}"))?;
        args.remove(0);
        Ok((identity, args))
    } else if args
        .first()
        .is_some_and(|argument| argument == bootty_mux::INTERNAL_RMUX_DAEMON_FLAG)
    {
        Ok((inherited_application_identity(inherited)?, args))
    } else {
        Ok((ApplicationIdentity::Production, args))
    }
}

fn inherited_application_identity(value: Option<&std::ffi::OsStr>) -> Result<ApplicationIdentity> {
    let Some(value) = value else {
        return Ok(ApplicationIdentity::Production);
    };
    let value = value
        .to_str()
        .context("application identity environment value is not UTF-8")?;
    ApplicationIdentity::parse(value)
        .with_context(|| format!("unknown application identity {value:?}"))
}

fn state_path(identity: ApplicationIdentity) -> Result<PathBuf> {
    let explicit = std::env::var_os("BOOTTY_DAEMON_STATE").map(PathBuf::from);
    #[cfg(windows)]
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(windows)]
    let app_data = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(windows)]
    let path = bootty_identity::windows_daemon_state_path(
        identity,
        explicit.as_deref(),
        local_app_data.as_deref(),
        app_data.as_deref(),
    );
    #[cfg(not(windows))]
    let xdg_state_home = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME").map(PathBuf::from);
    #[cfg(not(windows))]
    let path = bootty_identity::unix_daemon_state_path(
        identity,
        explicit.as_deref(),
        xdg_state_home.as_deref(),
        home.as_deref(),
    );
    path.context("daemon state root is unavailable")
}
