use std::ffi::OsStr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use bootty_daemon::catalog::{Backend, Catalog, LegacyCatalogPaths};
use bootty_identity::{APPLICATION_IDENTITY_ENV, ApplicationIdentity};

pub(crate) fn run_remote_ping() {
    println!(
        "{}:{}",
        bootty_remote::REMOTE_DAEMON_PROTOCOL_VERSION,
        env!("CARGO_PKG_VERSION")
    );
}

pub(crate) fn run_remote_exec(args: &[String]) -> Result<()> {
    let payload = args.first().context("remote-exec requires a payload")?;
    reject_extra(args.iter().skip(1))?;
    std::process::exit(bootty_remote::run_remote_command(payload)?);
}

pub(crate) fn run_remote_rmux(args: &[String]) -> Result<()> {
    let payload = args.first().context("remote-rmux requires a payload")?;
    reject_extra(args.iter().skip(1))?;
    std::process::exit(bootty_rmux::run_remote_rmux_command(payload)?);
}

pub(crate) struct RemoteSpacePaths {
    state: PathBuf,
    legacy: Option<LegacyCatalogPaths>,
}

pub(crate) fn run_remote_space(args: &[String], paths: &RemoteSpacePaths) -> Result<()> {
    let Some((command, arguments)) = args.split_first() else {
        bail!("remote-space requires a command")
    };
    bootty_rmux::link();
    bootty_tmux::link();
    let backends = std::sync::Arc::new(bootty_mux::provider::MuxBackendRegistry::collect([
        bootty_mux::MuxBackendKind::Rmux,
        bootty_mux::MuxBackendKind::Tmux,
    ])?);
    let mut catalog = Catalog::open(&paths.state, paths.legacy.as_ref(), backends)?;
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
            let command = bootty_remote::space_protocol::decode_command(&payload)?;
            catalog.execute(&id, backend, command)?;
        }
        _ => bail!("unknown remote-space command {command:?}"),
    }
    Ok(())
}

pub(crate) fn run_remote_project(args: &[String]) -> Result<()> {
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

pub(crate) fn run_remote_worktree(args: &[String]) -> Result<()> {
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

pub(crate) fn parse_application_identity(
    args: Vec<String>,
) -> Result<(ApplicationIdentity, Vec<String>)> {
    let inherited = std::env::var_os(APPLICATION_IDENTITY_ENV);
    parse_application_identity_with_inherited(args, inherited.as_deref())
}

fn parse_application_identity_with_inherited(
    mut args: Vec<String>,
    inherited: Option<&OsStr>,
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
        .is_some_and(|argument| argument == bootty_rmux::INTERNAL_DAEMON_FLAG)
    {
        Ok((inherited_application_identity(inherited)?, args))
    } else {
        Ok((ApplicationIdentity::Production, args))
    }
}

fn inherited_application_identity(value: Option<&OsStr>) -> Result<ApplicationIdentity> {
    let Some(value) = value else {
        return Ok(ApplicationIdentity::Production);
    };
    let value = value
        .to_str()
        .context("application identity environment value is not UTF-8")?;
    ApplicationIdentity::parse(value)
        .with_context(|| format!("unknown application identity {value:?}"))
}

pub(crate) fn remote_space_paths_from_environment(
    identity: ApplicationIdentity,
) -> Result<RemoteSpacePaths> {
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
    let state = path.context("daemon state root is unavailable")?;
    let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let legacy = bootty_identity::legacy_config_path_from_env(
        identity,
        xdg_config_home.as_deref(),
        home.as_deref(),
    )
    .map(LegacyCatalogPaths::from_config_path);
    Ok(RemoteSpacePaths { state, legacy })
}

fn home_dir() -> Option<PathBuf> {
    bootty_mux::project::home_dir()
}

fn required_option(args: &[String], name: &str) -> Result<String> {
    let (chunks, remainder) = args.as_chunks::<2>();
    let mut value = None;
    for chunk in chunks {
        if chunk[0] == name && value.replace(chunk[1].clone()).is_some() {
            bail!("duplicate option {name}")
        }
    }
    if !remainder.is_empty() {
        bail!("options require values")
    }
    value.with_context(|| format!("missing option {name}"))
}

fn option_values(args: &[String], name: &str) -> Result<Vec<String>> {
    let (chunks, remainder) = args.as_chunks::<2>();
    let values = chunks
        .iter()
        .filter(|chunk| chunk[0] == name)
        .map(|chunk| chunk[1].clone())
        .collect();
    if !remainder.is_empty() {
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
