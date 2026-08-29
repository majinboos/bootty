#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::env;
#[cfg(unix)]
use std::fs;
use std::path::PathBuf;

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;
use clap::Args as ClapArgs;
#[cfg(unix)]
use toml_edit::DocumentMut;

#[derive(Clone, Copy, Debug, Default, ClapArgs)]
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// Use the fast-release Cargo profile.
    #[arg(long)]
    pub fast: bool,
    /// Link the application statically where supported.
    #[arg(long)]
    pub r#static: bool,
    /// Package the isolated `BoottyDev` identity.
    #[arg(long)]
    pub dev: bool,
    /// Build and bundle daemons for every supported target.
    #[arg(long)]
    pub all_daemons: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Linkage {
    Dynamic,
    Static,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Layout {
    pub app_name: &'static str,
    #[cfg(unix)]
    pub cli_name: &'static str,
    #[cfg(unix)]
    pub bundle_identifier: &'static str,
    pub profile: &'static str,
    #[cfg(unix)]
    pub daemon_profile: &'static str,
    pub linkage: Linkage,
    pub dist_dir: PathBuf,
    pub target_root: PathBuf,
    pub daemon_output_dir: PathBuf,
    #[cfg(unix)]
    pub all_daemons: bool,
}

impl Layout {
    pub(crate) fn from_args(args: Args) -> Self {
        let target_root =
            env::var_os("CARGO_TARGET_DIR").map_or_else(|| PathBuf::from("target"), PathBuf::from);
        let daemon_output_dir = env::var_os("BOOTTY_DAEMON_OUTPUT_DIR")
            .map_or_else(|| target_root.join("bootty-daemons"), PathBuf::from);
        let linkage = if args.r#static {
            Linkage::Static
        } else {
            Linkage::Dynamic
        };
        let profile = if args.fast {
            "fast-release"
        } else if linkage == Linkage::Dynamic {
            "dynamic-release"
        } else {
            "release"
        };
        Self {
            app_name: if args.dev { "BoottyDev" } else { "Bootty" },
            #[cfg(unix)]
            cli_name: if args.dev { "bootty-dev" } else { "bootty" },
            #[cfg(unix)]
            bundle_identifier: if args.dev {
                "dev.bootty.desktop.dev"
            } else {
                "dev.bootty.desktop"
            },
            profile,
            #[cfg(unix)]
            daemon_profile: if args.fast {
                "fast-release"
            } else {
                "daemon-release"
            },
            linkage,
            dist_dir: env::var_os("BOOTTY_DIST_DIR")
                .map_or_else(|| PathBuf::from("dist"), PathBuf::from),
            target_root,
            daemon_output_dir,
            #[cfg(unix)]
            all_daemons: args.all_daemons || env::var_os("BOOTTY_DAEMON_OUTPUT_DIR").is_some(),
        }
    }

    #[cfg(unix)]
    pub(crate) fn cargo_profile_args(&self) -> Vec<&'static str> {
        match self.profile {
            "release" => vec!["--release"],
            profile => vec!["--profile", profile],
        }
    }

    #[cfg(unix)]
    pub(crate) fn daemon_profile_args(&self) -> Vec<&'static str> {
        match self.daemon_profile {
            "release" => vec!["--release"],
            profile => vec!["--profile", profile],
        }
    }
}

pub fn run(args: Args) -> Result<()> {
    let layout = Layout::from_args(args);
    run_platform(args, &layout)
}

#[cfg(windows)]
fn run_platform(args: Args, layout: &Layout) -> Result<()> {
    windows::run(args, layout)
}

#[cfg(unix)]
fn run_platform(args: Args, layout: &Layout) -> Result<()> {
    unix::run(args, layout)
}

#[cfg(not(any(unix, windows)))]
fn run_platform(_args: Args, _layout: &Layout) -> Result<()> {
    anyhow::bail!("packaging is unsupported on this operating system")
}

#[cfg(unix)]
pub(crate) fn workspace_version() -> Result<String> {
    if let Some(version) = env::var_os("BOOTTY_VERSION").filter(|version| !version.is_empty()) {
        return Ok(version.to_string_lossy().into_owned());
    }
    let manifest = fs::read_to_string("Cargo.toml").context("failed to read Cargo.toml")?;
    let document = manifest
        .parse::<DocumentMut>()
        .context("failed to parse Cargo.toml")?;
    document["workspace"]["package"]["version"]
        .as_str()
        .map(str::to_owned)
        .context("Cargo.toml has no workspace.package.version")
}

pub(crate) fn print_dist_files(layout: &Layout) -> Result<()> {
    for path in crate::filesystem::files_recursive(&layout.dist_dir)? {
        println!("{}", path.display());
    }
    Ok(())
}
