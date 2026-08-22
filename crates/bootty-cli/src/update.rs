use std::process::Command;

use anyhow::{Context, Result};

use bootty_identity::ApplicationIdentity;

const REPOSITORY_OWNER: &str = "majindotboo";
const REPOSITORY_NAME: &str = "bootty";
const BINARY_NAME: &str = "bootty";
const SKIP_AUTOMATIC_UPDATE: &str = "BOOTTY_SKIP_AUTOMATIC_UPDATE";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateResult {
    Skipped,
    UpToDate,
    Updated,
}

pub fn automatic_update() -> Result<UpdateResult> {
    if std::env::var_os(SKIP_AUTOMATIC_UPDATE).is_some() {
        return Ok(UpdateResult::Skipped);
    }
    update(false)
}

pub fn update(show_output: bool) -> Result<UpdateResult> {
    if !ApplicationIdentity::current().automatic_updates_enabled() {
        return Ok(UpdateResult::Skipped);
    }

    #[cfg(target_os = "linux")]
    {
        let status = self_update::backends::github::Update::configure()
            .repo_owner(REPOSITORY_OWNER)
            .repo_name(REPOSITORY_NAME)
            .bin_name(BINARY_NAME)
            .bin_path_in_archive(format!(
                "Bootty-linux-{}/bin/{BINARY_NAME}",
                std::env::consts::ARCH
            ))
            .current_version(env!("CARGO_PKG_VERSION"))
            .no_confirm(true)
            .show_output(show_output)
            .show_download_progress(show_output)
            .build()
            .context("configure the Bootty updater")?
            .update()
            .context("update Bootty from GitHub Releases")?;
        Ok(if status.is_updated() {
            UpdateResult::Updated
        } else {
            UpdateResult::UpToDate
        })
    }

    #[cfg(target_os = "macos")]
    {
        let status = self_update::backends::github::Update::configure()
            .repo_owner(REPOSITORY_OWNER)
            .repo_name(REPOSITORY_NAME)
            .bin_name(BINARY_NAME)
            .bin_path_in_archive("Bootty.app/Contents/MacOS/bootty")
            .current_version(env!("CARGO_PKG_VERSION"))
            .no_confirm(true)
            .show_output(show_output)
            .show_download_progress(show_output)
            .build()
            .context("configure the Bootty updater")?
            .update()
            .context("update Bootty from GitHub Releases")?;
        Ok(if status.is_updated() {
            UpdateResult::Updated
        } else {
            UpdateResult::UpToDate
        })
    }

    #[cfg(target_os = "windows")]
    {
        let _ = show_output;
        anyhow::bail!(
            "automatic updates are unavailable on Windows until Bootty ships a single-file installer"
        )
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = show_output;
        anyhow::bail!("automatic updates are unsupported on this platform")
    }
}

pub fn restart_after_update() -> Result<()> {
    let executable =
        std::env::current_exe().context("resolve the updated Bootty executable path")?;
    Command::new(executable)
        .args(std::env::args_os().skip(1))
        .env(SKIP_AUTOMATIC_UPDATE, "1")
        .spawn()
        .context("restart Bootty after updating")?;
    Ok(())
}
