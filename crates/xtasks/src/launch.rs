use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

#[derive(Clone, Debug, ClapArgs)]
#[command(trailing_var_arg = true)]
pub struct Args {
    /// Arguments passed to Bootty.
    #[arg(allow_hyphen_values = true)]
    pub arguments: Vec<OsString>,
}

pub fn run(args: Args) -> Result<()> {
    let build = crate::build::BuildArgs {
        fast: false,
        static_linkage: false,
    };
    crate::build::run_with_features(&build, cfg!(unix))?;

    let target_root =
        env::var_os("CARGO_TARGET_DIR").map_or_else(|| PathBuf::from("target"), PathBuf::from);
    let profile = target_root.join("dynamic-release");
    let binary = profile.join(if cfg!(windows) {
        "bootty.exe"
    } else {
        "bootty"
    });
    if !binary.is_file() {
        bail!("built binary not found at {}", binary.display());
    }
    let rust_libdir =
        crate::command::stdout(Command::new("rustc").args(["--print", "target-libdir"]))?;
    let mut library_dirs = vec![profile.join("deps"), PathBuf::from(rust_libdir.trim())];
    #[cfg(windows)]
    if let Some(directory) = find_ghostty_dll(&profile)?.parent() {
        library_dirs.push(directory.to_path_buf());
    }
    let inherited = env::var_os(library_path_variable()).unwrap_or_default();
    if !inherited.is_empty() {
        library_dirs.extend(env::split_paths(&inherited));
    }
    let library_path = env::join_paths(library_dirs).context("failed to construct library path")?;

    let mut command = Command::new(&binary);
    command
        .args(args.arguments)
        .env(library_path_variable(), library_path);
    execute(command, &binary)
}

#[cfg(unix)]
fn execute(mut command: Command, binary: &std::path::Path) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let error = command.exec();
    Err(error).with_context(|| format!("failed to execute {}", binary.display()))
}

#[cfg(windows)]
fn execute(mut command: Command, _binary: &std::path::Path) -> Result<()> {
    crate::command::run(&mut command)
}

fn library_path_variable() -> &'static str {
    if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else if cfg!(windows) {
        "PATH"
    } else {
        "LD_LIBRARY_PATH"
    }
}

#[cfg(windows)]
fn find_ghostty_dll(profile: &std::path::Path) -> Result<PathBuf> {
    crate::filesystem::files_recursive(profile)?
        .into_iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "ghostty-vt.dll")
        })
        .context("ghostty-vt.dll was not built")
}
