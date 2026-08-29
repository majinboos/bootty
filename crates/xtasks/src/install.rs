use std::env;
#[cfg(unix)]
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use crate::filesystem;
use crate::package::{self, Args as PackageArgs};

#[derive(Clone, Debug, ClapArgs)]
#[group(id = "install")]
pub struct Args {
    #[command(flatten)]
    pub package: PackageArgs,
}

pub fn run(args: &Args) -> Result<()> {
    package::run(args.package)?;
    let layout = package::Layout::from_args(args.package);
    install_platform(&layout)
}

#[cfg(target_os = "macos")]
fn install_platform(layout: &package::Layout) -> Result<()> {
    install_macos(layout)
}

#[cfg(target_os = "linux")]
fn install_platform(layout: &package::Layout) -> Result<()> {
    install_linux(layout)
}

#[cfg(windows)]
fn install_platform(layout: &package::Layout) -> Result<()> {
    install_windows(layout)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn install_platform(_layout: &package::Layout) -> Result<()> {
    bail!("installation is unsupported on this operating system")
}

#[cfg(target_os = "macos")]
fn install_macos(layout: &package::Layout) -> Result<()> {
    use std::os::unix::fs::symlink;

    let install_dir = env::var_os("BOOTTY_INSTALL_DIR")
        .map_or_else(|| PathBuf::from("/Applications"), PathBuf::from);
    let source = layout.dist_dir.join(format!("{}.app", layout.app_name));
    if !source.is_dir() {
        bail!("packaged app not found at {}", source.display());
    }
    let target = install_dir.join(format!("{}.app", layout.app_name));
    remove_install_target(&target)?;
    filesystem::copy_dir(&source, &target)?;

    let home = home_dir()?;
    let cli_dir = select_cli_dir(&home);
    fs::create_dir_all(&cli_dir)?;
    let cli = cli_dir.join(layout.cli_name);
    remove_link_target(&cli)?;
    symlink(target.join("Contents/MacOS/bootty"), &cli)
        .with_context(|| format!("failed to create {}", cli.display()))?;
    ensure_user_path(&cli_dir)?;
    println!("Installed {} and {}", target.display(), cli.display());
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_linux(layout: &package::Layout) -> Result<()> {
    let home = home_dir()?;
    let prefix =
        env::var_os("BOOTTY_INSTALL_PREFIX").map_or_else(|| home.join(".local"), PathBuf::from);
    let root = layout
        .dist_dir
        .join(format!("{}-linux-{}", layout.app_name, env::consts::ARCH));
    if !root.is_dir() {
        bail!("packaged app not found at {}", root.display());
    }
    filesystem::copy_executable(
        &root.join("bin").join(layout.cli_name),
        &prefix.join("bin").join(layout.cli_name),
    )?;
    filesystem::copy_executable(
        &root.join("bin/bootty-daemon"),
        &prefix.join("bin/bootty-daemon"),
    )?;
    copy_directory_files(&root.join("lib"), &prefix.join("lib"), Some("so"), false)?;
    copy_directory_files(
        &root.join("share/bootty/daemons"),
        &prefix.join("share/bootty/daemons"),
        None,
        true,
    )?;
    filesystem::copy_file(
        &root
            .join("share/applications")
            .join(format!("{}.desktop", layout.bundle_identifier)),
        &prefix
            .join("share/applications")
            .join(format!("{}.desktop", layout.bundle_identifier)),
    )?;
    filesystem::copy_file(
        &root.join("share/icons/hicolor/256x256/apps/bootty.png"),
        &prefix.join("share/icons/hicolor/256x256/apps/bootty.png"),
    )?;
    filesystem::copy_file(
        &root.join("share/icons/hicolor/scalable/apps/bootty.svg"),
        &prefix.join("share/icons/hicolor/scalable/apps/bootty.svg"),
    )?;
    ensure_user_path(&prefix.join("bin"))?;
    run_optional_cache_update(
        "update-desktop-database",
        &prefix.join("share/applications"),
    );
    run_optional_cache_update("gtk-update-icon-cache", &prefix.join("share/icons/hicolor"));
    println!(
        "Installed {}",
        prefix.join("bin").join(layout.cli_name).display()
    );
    Ok(())
}

#[cfg(windows)]
fn install_windows(layout: &package::Layout) -> Result<()> {
    let local_app_data = env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    let install_dir = env::var_os("BOOTTY_INSTALL_DIR").map_or_else(
        || PathBuf::from(local_app_data).join("Programs/Bootty"),
        PathBuf::from,
    );
    let arch = if env::var("PROCESSOR_ARCHITECTURE").is_ok_and(|arch| arch == "ARM64") {
        "arm64"
    } else {
        "x64"
    };
    let bundle = layout.dist_dir.join(format!("Bootty-windows-{arch}"));
    if !bundle.is_dir() {
        bail!("packaged app not found at {}", bundle.display());
    }
    remove_install_target(&install_dir)?;
    filesystem::copy_dir(&bundle, &install_dir)?;

    add_windows_user_path(&install_dir)?;
    create_windows_shortcut(&install_dir)?;
    println!(
        "Installed {} and added {} to the user PATH",
        install_dir.join("bootty.exe").display(),
        install_dir.display()
    );
    Ok(())
}

#[cfg(windows)]
fn add_windows_user_path(install_dir: &Path) -> Result<()> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

    let (environment, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)
        .context("failed to open the user environment registry key")?;
    let current = environment
        .get_value::<String, _>("Path")
        .unwrap_or_default();
    let install = install_dir.to_string_lossy();
    let present = current.split(';').any(|entry| {
        entry
            .trim_end_matches(['\\', '/'])
            .eq_ignore_ascii_case(install.trim_end_matches(['\\', '/']))
    });
    if !present {
        let updated = if current.trim().is_empty() {
            install.into_owned()
        } else {
            format!("{current};{install}")
        };
        environment
            .set_value("Path", &updated)
            .context("failed to update the user PATH")?;
    }
    Ok(())
}

#[cfg(windows)]
fn create_windows_shortcut(install_dir: &Path) -> Result<()> {
    let Some(app_data) = env::var_os("APPDATA") else {
        return Ok(());
    };
    let menu = PathBuf::from(app_data).join("Microsoft/Windows/Start Menu/Programs");
    if !menu.is_dir() {
        return Ok(());
    }
    let binary = install_dir.join("bootty.exe");
    let working_dir =
        env::var_os("USERPROFILE").map_or_else(|| install_dir.to_path_buf(), PathBuf::from);
    let mut shortcut =
        mslnk::ShellLink::new(&binary).context("failed to create Bootty shortcut")?;
    shortcut.set_working_dir(Some(working_dir.to_string_lossy().into_owned()));
    shortcut.set_icon_location(Some(binary.to_string_lossy().into_owned()));
    shortcut
        .create_lnk(menu.join("Bootty.lnk"))
        .context("failed to write the Bootty Start Menu shortcut")
}

#[cfg(unix)]
fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

#[cfg(unix)]
fn ensure_user_path(directory: &Path) -> Result<()> {
    if env::split_paths(&env::var_os("PATH").unwrap_or_default()).any(|path| path == directory) {
        return Ok(());
    }
    let home = home_dir()?;
    let shell_value = env::var_os("SHELL");
    let shell = shell_value
        .as_deref()
        .and_then(|shell| Path::new(shell).file_name())
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    let (profile, line) = match shell {
        "fish" => (
            home.join(".config/fish/config.fish"),
            format!("fish_add_path \"{}\"", directory.display()),
        ),
        "zsh" => (
            home.join(".zprofile"),
            format!("export PATH=\"{}:$PATH\"", directory.display()),
        ),
        _ => (
            home.join(".profile"),
            format!("export PATH=\"{}:$PATH\"", directory.display()),
        ),
    };
    if let Some(parent) = profile.parent() {
        fs::create_dir_all(parent)?;
    }
    let existing = match fs::read_to_string(&profile) {
        Ok(existing) => existing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", profile.display()));
        }
    };
    if !existing.lines().any(|candidate| candidate == line) {
        let separator = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        fs::write(&profile, format!("{existing}{separator}\n{line}\n"))?;
    }
    println!(
        "Added {} to PATH in {}",
        directory.display(),
        profile.display()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn select_cli_dir(home: &Path) -> PathBuf {
    let candidates = [
        home.join(".local/bin"),
        home.join("bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
    ];
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .find(|path| candidates.contains(path) && path.is_dir() && writable(path))
        .unwrap_or_else(|| home.join(".local/bin"))
}

#[cfg(target_os = "macos")]
fn writable(path: &Path) -> bool {
    std::process::Command::new("test")
        .args([OsStr::new("-w"), path.as_os_str()])
        .status()
        .is_ok_and(|status| status.success())
}

fn remove_install_target(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .with_context(|| format!("failed to replace {}", path.display()))
}

#[cfg(target_os = "macos")]
fn remove_link_target(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        bail!("refusing to replace directory at {}", path.display());
    }
    fs::remove_file(path).with_context(|| format!("failed to replace {}", path.display()))
}

#[cfg(target_os = "linux")]
fn copy_directory_files(
    source: &Path,
    destination: &Path,
    extension: Option<&str>,
    executable: bool,
) -> Result<()> {
    if !source.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        if extension.is_some_and(|extension| {
            !entry
                .file_name()
                .to_string_lossy()
                .contains(&format!(".{extension}"))
        }) {
            continue;
        }
        let destination = destination.join(entry.file_name());
        if executable {
            filesystem::copy_executable(&entry.path(), &destination)?;
        } else {
            filesystem::copy_file(&entry.path(), &destination)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_optional_cache_update(program: &str, directory: &Path) {
    if crate::command::program_exists(program) {
        let _ = std::process::Command::new(program).arg(directory).status();
    }
}
