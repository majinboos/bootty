use bootty_identity::ApplicationIdentity;

pub(crate) fn ensure_cli_link() -> std::io::Result<()> {
    let executable = std::env::current_exe()?;
    if !executable.ends_with("Contents/MacOS/bootty") {
        return Ok(());
    }
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return Ok(());
    };
    if let Some(path) = std::env::var_os("PATH") {
        let local_bin = home.join(".local/bin");
        let home_bin = home.join("bin");
        for directory in std::env::split_paths(&path).filter(|directory| {
            directory == &local_bin
                || directory == &home_bin
                || matches!(
                    directory.to_str(),
                    Some("/usr/local/bin" | "/opt/homebrew/bin")
                )
        }) {
            match install_cli_link_at(&executable, &directory) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
                Err(error) => return Err(error),
            }
        }
    }
    let directory = home.join(".local/bin");
    install_cli_link_at(&executable, &directory)?;
    ensure_local_bin_path(&home, std::env::var_os("SHELL").as_deref())
}

fn ensure_local_bin_path(
    home: &std::path::Path,
    shell: Option<&std::ffi::OsStr>,
) -> std::io::Result<()> {
    let shell_name = shell.and_then(|shell| std::path::Path::new(shell).file_name());
    let fish = shell_name.is_some_and(|name| name == "fish");
    let (profile, line) = if fish {
        (
            home.join(".config/fish/config.fish"),
            r#"fish_add_path "$HOME/.local/bin""#,
        )
    } else {
        (
            home.join(if shell_name.is_some_and(|name| name == "zsh") {
                ".zprofile"
            } else {
                ".profile"
            }),
            r#"export PATH="$HOME/.local/bin:$PATH""#,
        )
    };
    if let Some(parent) = profile.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = std::fs::read_to_string(&profile).unwrap_or_default();
    if !contents.lines().any(|existing| existing == line) {
        use std::io::Write as _;
        writeln!(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(profile)?,
            "{line}"
        )?;
    }
    Ok(())
}

fn install_cli_link_at(
    executable: &std::path::Path,
    directory: &std::path::Path,
) -> std::io::Result<()> {
    let link = directory.join(ApplicationIdentity::current().cli_name());
    std::fs::create_dir_all(directory)?;
    match std::fs::symlink_metadata(&link) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if std::fs::read_link(&link)? == executable {
                return Ok(());
            }
            std::fs::remove_file(&link)?;
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::os::unix::fs::symlink(executable, link)
}
