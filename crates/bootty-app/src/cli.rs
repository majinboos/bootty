use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use bootty_config::config::{BoottyConfig, default_config_path, load_config_from_path};
use clap::{Parser, Subcommand, ValueEnum};

mod config_overrides;

use config_overrides::ConfigOverrides;

#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Download and install the latest Bootty release.
    Update,
    /// List commands exposed by a running Bootty instance.
    Commands,
    /// Describe one command exposed by a running Bootty instance.
    Describe { name: String },
    /// Invoke a command through the owner-local control plane.
    #[command(name = "command")]
    Invoke {
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Legacy remote Space protocol retained while daemon installations roll out.
    #[command(name = "remote-space", hide = true, subcommand)]
    RemoteSpace(RemoteSpaceCommand),
    /// Legacy remote command transport retained while daemon installations roll out.
    #[command(name = "remote-exec", hide = true)]
    RemoteExec { payload: String },
    /// Legacy remote availability probe retained while daemon installations roll out.
    #[command(name = "remote-ping", hide = true)]
    RemotePing,
    /// Legacy remote terminal protocol retained while daemon installations roll out.
    #[command(name = "remote-rmux", hide = true)]
    RemoteRmux { payload: String },
    /// Invoke a command discovered from a running Bootty instance.
    #[command(external_subcommand)]
    Dynamic(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
pub enum RemoteSpaceCommand {
    List,
    Create {
        #[arg(long)]
        name: String,
        #[arg(long, value_enum)]
        backend: RemoteSpaceBackend,
    },
    Snapshot {
        #[arg(long)]
        id: String,
        #[arg(long, value_enum)]
        backend: RemoteSpaceBackend,
    },
    Execute {
        #[arg(long)]
        id: String,
        #[arg(long, value_enum)]
        backend: RemoteSpaceBackend,
        #[arg(long)]
        payload: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RemoteSpaceBackend {
    Rmux,
    Tmux,
    Zellij,
}

impl From<RemoteSpaceBackend> for bootty_config::config::MultiplexerBackendConfig {
    fn from(value: RemoteSpaceBackend) -> Self {
        match value {
            RemoteSpaceBackend::Rmux => Self::Rmux,
            RemoteSpaceBackend::Tmux => Self::Tmux,
            RemoteSpaceBackend::Zellij => Self::Zellij,
        }
    }
}
#[derive(Debug, Parser)]
#[command(name = "bootty", version, about = "Bootty terminal emulator")]
pub struct Cli {
    /// Load config from this TOML file instead of the default XDG path.
    #[arg(long, value_name = "PATH", conflicts_with = "defaults")]
    config: Option<PathBuf>,

    /// Ignore user config and start from built-in defaults with isolated temp sidecar state.
    #[arg(long, conflicts_with = "config")]
    defaults: bool,

    /// Stable persistence identity for this application window.
    #[arg(long, default_value = "main", hide = true)]
    window_state_key: String,

    /// Select a running Bootty process by instance ID.
    #[arg(long, global = true)]
    instance: Option<String>,

    /// Print the exact JSON-RPC response.
    #[arg(long, global = true)]
    json: bool,

    /// Start a Bootty instance when none is running.
    #[arg(long, global = true)]
    start: bool,

    #[command(flatten)]
    overrides: ConfigOverrides,

    #[command(subcommand)]
    command: Option<Command>,
}

impl Cli {
    /// Parse command-line arguments while keeping confirmation separate from
    /// the dynamic trailing argument list.
    ///
    /// Clap intentionally gives a trailing var arg everything after the first
    /// positional value. Consequently `--yes` after a dynamic argument is
    /// present in `arguments` rather than in the generated boolean field. The
    /// raw tokens are the only place where an explicit `--` delimiter remains
    /// observable, so normalize from them after Clap has validated the rest of
    /// the command line.
    pub fn parse() -> Self {
        Self::try_parse_from(std::env::args_os()).unwrap_or_else(|error| error.exit())
    }

    pub fn try_parse_from<I, T>(itr: I) -> std::result::Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let raw: Vec<OsString> = itr.into_iter().map(Into::into).collect();
        let mut cli = <Self as Parser>::try_parse_from(raw.clone())?;
        normalize_invoke_confirmation(&mut cli, &raw)?;
        Ok(cli)
    }

    pub fn load_config(&self) -> Result<BoottyConfig> {
        let path = self.selected_config_path();
        if self.defaults {
            create_parent_dir_for_defaults(&path)?;
        }
        let mut config = load_config_from_path(&path)?;
        self.overrides.apply(&mut config)?;
        Ok(config)
    }

    pub fn window_state_key(&self) -> &str {
        &self.window_state_key
    }

    pub fn subcommand(&self) -> Option<&Command> {
        self.command.as_ref()
    }

    pub fn instance(&self) -> Option<&str> {
        self.instance.as_deref()
    }

    pub fn json(&self) -> bool {
        self.json
    }

    pub fn start(&self) -> bool {
        self.start
    }

    fn selected_config_path(&self) -> PathBuf {
        if self.defaults {
            return isolated_defaults_config_path();
        }
        self.config.clone().unwrap_or_else(default_config_path)
    }
}

fn normalize_invoke_confirmation(
    cli: &mut Cli,
    raw: &[OsString],
) -> std::result::Result<(), clap::Error> {
    let Some(Command::Invoke {
        name: _,
        arguments,
        yes,
    }) = cli.command.as_mut()
    else {
        return Ok(());
    };
    let Some(command_index) = invoke_subcommand_index(raw) else {
        return Ok(());
    };

    let mut saw_name = false;
    let mut delimiter = false;
    let mut confirmation_count = 0;
    let mut normalized_arguments = Vec::new();
    for token in &raw[command_index + 1..] {
        let token = token.to_string_lossy();
        if !saw_name {
            if token == "--yes" {
                confirmation_count += 1;
                continue;
            }
            if token.starts_with("--yes=") {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::InvalidValue,
                    "--yes does not accept a value",
                ));
            }
            saw_name = true;
            continue;
        }
        if !delimiter && token == "--" {
            delimiter = true;
            continue;
        }
        if !delimiter && token == "--yes" {
            confirmation_count += 1;
            continue;
        }
        if !delimiter && token.starts_with("--yes=") {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::InvalidValue,
                "--yes does not accept a value",
            ));
        }
        normalized_arguments.push(token.into_owned());
    }
    if confirmation_count > 1 {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ArgumentConflict,
            "--yes may only be specified once",
        ));
    }
    *arguments = normalized_arguments;
    *yes |= confirmation_count == 1;
    Ok(())
}

fn invoke_subcommand_index(raw: &[OsString]) -> Option<usize> {
    let mut index = 1;
    while index < raw.len() {
        let token = raw[index].to_string_lossy();
        if token == "command" {
            return Some(index);
        }
        if token == "--" {
            return None;
        }
        if let Some(option) = token
            .strip_prefix("--")
            .and_then(|token| token.split('=').next())
            && option_takes_value(option)
            && !token.contains('=')
        {
            index += 1;
        }
        index += 1;
    }
    None
}

fn option_takes_value(option: &str) -> bool {
    matches!(
        option,
        "config"
            | "window-state-key"
            | "instance"
            | "backend"
            | "ssh-remote"
            | "fullscreen-top-offset"
            | "window-decoration"
            | "titlebar"
            | "macos-titlebar-style"
            | "title"
            | "width"
            | "height"
            | "theme"
            | "background"
            | "foreground"
            | "cursor-color"
            | "cursor-text"
            | "selection-background"
            | "selection-foreground"
            | "palette"
            | "font-size"
            | "font-family"
            | "font-feature"
            | "font-cell-width"
            | "font-cell-height"
            | "font-baseline-adjustment"
            | "font-underline-position"
            | "font-underline-thickness"
            | "cursor-style"
            | "shell"
            | "working-directory"
            | "env"
            | "term"
            | "colorterm"
            | "max-scrollback"
            | "macos-option-as-alt"
            | "modifier-remap"
            | "sidebar-position"
            | "sidebar-width"
            | "sidebar-background"
            | "sidebar-foreground"
            | "sidebar-selected"
            | "sidebar-hover"
            | "sidebar-border"
            | "status-height"
            | "chrome-gap"
            | "gap"
            | "unfocused-sidebar-dim"
            | "unfocused-terminal-dim"
            | "stability-trace"
    )
}

fn isolated_defaults_config_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir()
        .join(format!("bootty-defaults-{}-{nanos}", process::id()))
        .join("config.toml")
}

fn create_parent_dir_for_defaults(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create isolated defaults directory {}",
                parent.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use bootty_config::{
        color::Color,
        config::{
            CursorStyleConfig, MacosOptionAsAltConfig, MacosTitlebarStyle,
            MultiplexerBackendConfig, SidebarPosition, WindowDecoration, WindowFullscreen,
        },
    };
    use bootty_terminal::terminal_engine::NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE;
    use clap::CommandFactory;
    use indoc::indoc;

    use super::{Cli, Command};

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn update_subcommand_is_parsed() {
        let cli = Cli::try_parse_from(["bootty", "update"]).unwrap();

        assert_eq!(cli.subcommand(), Some(&Command::Update));
    }

    #[test]
    fn dynamic_control_commands_are_parsed() {
        let list = Cli::try_parse_from(["bootty", "commands"]).unwrap();
        assert_eq!(list.subcommand(), Some(&Command::Commands));

        let invoke =
            Cli::try_parse_from(["bootty", "--instance", "42", "command", "move_tab", "-1"])
                .unwrap();
        assert_eq!(invoke.instance(), Some("42"));
        assert_eq!(
            invoke.subcommand(),
            Some(&Command::Invoke {
                name: "move_tab".to_owned(),
                arguments: vec!["-1".to_owned()],
                yes: false,
            })
        );

        let dynamic =
            Cli::try_parse_from(["bootty", "terminal.scroll-page-lines", "--delta", "-3"]).unwrap();
        assert_eq!(
            dynamic.subcommand(),
            Some(&Command::Dynamic(vec![
                "terminal.scroll-page-lines".to_owned(),
                "--delta".to_owned(),
                "-3".to_owned(),
            ]))
        );

        let words =
            Cli::try_parse_from(["bootty", "--json", "agents", "prompt", "--message", "hello"])
                .unwrap();
        assert!(words.json());
        assert_eq!(
            words.subcommand(),
            Some(&Command::Dynamic(vec![
                "agents".to_owned(),
                "prompt".to_owned(),
                "--message".to_owned(),
                "hello".to_owned(),
            ]))
        );
    }

    #[test]
    fn invoke_confirmation_after_arguments_is_extracted() {
        let cli = Cli::try_parse_from([
            "bootty",
            "command",
            "worktree.remove",
            "/tmp/worktree",
            "--yes",
        ])
        .unwrap();

        assert_eq!(
            cli.subcommand(),
            Some(&Command::Invoke {
                name: "worktree.remove".to_owned(),
                arguments: vec!["/tmp/worktree".to_owned()],
                yes: true,
            })
        );
    }

    #[test]
    fn invoke_subcommand_scan_skips_global_option_values() {
        let cli = Cli::try_parse_from([
            "bootty",
            "--instance",
            "command",
            "command",
            "command",
            "/tmp/worktree",
            "--yes",
        ])
        .unwrap();

        assert_eq!(
            cli.subcommand(),
            Some(&Command::Invoke {
                name: "command".to_owned(),
                arguments: vec!["/tmp/worktree".to_owned()],
                yes: true,
            })
        );
    }

    #[test]
    fn invoke_confirmation_before_name_is_preserved() {
        let cli = Cli::try_parse_from([
            "bootty",
            "command",
            "--yes",
            "worktree.remove",
            "/tmp/worktree",
        ])
        .unwrap();

        assert_eq!(
            cli.subcommand(),
            Some(&Command::Invoke {
                name: "worktree.remove".to_owned(),
                arguments: vec!["/tmp/worktree".to_owned()],
                yes: true,
            })
        );
    }

    #[test]
    fn invoke_confirmation_after_delimiter_is_literal() {
        let cli = Cli::try_parse_from([
            "bootty",
            "command",
            "worktree.remove",
            "/tmp/worktree",
            "--",
            "--yes",
        ])
        .unwrap();

        assert_eq!(
            cli.subcommand(),
            Some(&Command::Invoke {
                name: "worktree.remove".to_owned(),
                arguments: vec!["/tmp/worktree".to_owned(), "--yes".to_owned()],
                yes: false,
            })
        );
    }

    #[test]
    fn invoke_without_confirmation_remains_unconfirmed() {
        let cli =
            Cli::try_parse_from(["bootty", "command", "worktree.remove", "/tmp/worktree"]).unwrap();

        assert_eq!(
            cli.subcommand(),
            Some(&Command::Invoke {
                name: "worktree.remove".to_owned(),
                arguments: vec!["/tmp/worktree".to_owned()],
                yes: false,
            })
        );
    }

    #[test]
    fn invoke_confirmation_cannot_be_repeated() {
        let result = Cli::try_parse_from([
            "bootty",
            "command",
            "worktree.remove",
            "/tmp/worktree",
            "--yes",
            "--yes",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn remote_proxy_commands_are_parsed() {
        let exec = Cli::try_parse_from(["bootty", "remote-exec", "payload"]).unwrap();
        assert_eq!(
            exec.subcommand(),
            Some(&Command::RemoteExec {
                payload: "payload".to_owned(),
            })
        );

        let ping = Cli::try_parse_from(["bootty", "remote-ping"]).unwrap();
        assert_eq!(ping.subcommand(), Some(&Command::RemotePing));

        let rmux = Cli::try_parse_from(["bootty", "remote-rmux", "payload"]).unwrap();
        assert_eq!(
            rmux.subcommand(),
            Some(&Command::RemoteRmux {
                payload: "payload".to_owned(),
            })
        );
    }

    #[test]
    fn config_flag_selects_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-config.toml");
        fs::write(&path, "version = 1\n[multiplexer]\nbackend = \"tmux\"\n").unwrap();

        let cli = Cli::try_parse_from(["bootty", "--config", path.to_str().unwrap()]).unwrap();
        let config = cli.load_config().unwrap();

        assert_eq!(config.config_path, path);
        assert_eq!(config.multiplexer.backend, MultiplexerBackendConfig::Tmux);
    }

    #[test]
    fn explicit_flags_override_loaded_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            indoc! {r#"
                version = 1

                [multiplexer]
                backend = "tmux"
                hide-tmux-status = true

                [window]
                title = "from config"
                width = 900
                height = 500
                fullscreen = false

                [chrome]
                sidebar = false
                status-bar = false
                [font]
                family = ["Config Font"]
                size = 11

                [session]
                shell = "/bin/zsh"
                working-directory = "/tmp/config"
            "#},
        )
        .unwrap();

        let cli = Cli::try_parse_from([
            "bootty",
            "--config",
            config_path.to_str().unwrap(),
            "--backend",
            "rmux",
            "--fullscreen",
            "non-native",
            "--title",
            "from cli",
            "--width",
            "800",
            "--height",
            "600",
            "--sidebar",
            "--status-bar",
            "--bottom-bar",
            "--font-size",
            "14",
            "--font-family",
            "Mono A,Mono B",
            "--shell",
            "/bin/bash",
            "--working-directory",
            "/tmp/cli",
            "--show-tmux-status",
        ])
        .unwrap();

        let config = cli.load_config().unwrap();

        assert_eq!(config.multiplexer.backend, MultiplexerBackendConfig::Rmux);
        assert!(!config.multiplexer.hide_tmux_status);
        assert_eq!(config.window.fullscreen, WindowFullscreen::NonNative);
        assert_eq!(config.window.title, "from cli");
        assert_eq!(config.window.width, 800.0);
        assert_eq!(config.window.height, 600.0);
        assert!(config.chrome.sidebar);
        assert!(config.chrome.top_bar);
        assert!(config.chrome.bottom_bar);
        assert_eq!(config.font.size, 14.0);
        assert_eq!(config.font.family, ["Mono A", "Mono B"]);
        assert_eq!(config.session.shell.as_deref(), Some("/bin/bash"));
        assert_eq!(
            config.session.working_directory,
            Some(PathBuf::from("/tmp/cli"))
        );
    }

    #[test]
    fn expanded_flags_override_loaded_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "version = 1\n").unwrap();

        let cli = Cli::try_parse_from([
            "bootty",
            "--config",
            config_path.to_str().unwrap(),
            "--titlebar",
            "hidden",
            "--window-decoration",
            "none",
            "--fullscreen-top-offset",
            "22",
            "--no-fullscreen-tabs-in-notch",
            "--background",
            "#010203",
            "--foreground",
            "#040506",
            "--cursor-color",
            "#070809",
            "--cursor-text",
            "#0a0b0c",
            "--selection-background",
            "#111213",
            "--selection-foreground",
            "#141516",
            "--palette",
            "#000000,#ffffff",
            "--no-palette-generate",
            "--palette-harmonious",
            "--font-cell-width",
            "9",
            "--font-cell-height",
            "20",
            "--no-fit-cell-height",
            "--font-baseline-adjustment",
            "1.5",
            "--font-underline-position",
            "2.5",
            "--font-underline-thickness",
            "1.25",
            "--font-feature",
            "+liga,ss01",
            "--cursor-style",
            "hollow-block",
            "--no-cursor-blink",
            "--env",
            "EDITOR=nvim",
            "--env",
            "BOOTTY_TEST=1",
            "--term",
            "xterm-test",
            "--colorterm",
            "24bit",
            "--max-scrollback",
            "1234",
            "--no-glyph-protocol",
            "--macos-option-as-alt",
            "left",
            "--modifier-remap",
            "right_alt=left_ctrl,right_super=left_alt",
            "--sidebar-position",
            "right",
            "--sidebar-width",
            "244",
            "--sidebar-background",
            "#202122",
            "--sidebar-foreground",
            "#262728",
            "--sidebar-selected",
            "#292a2b",
            "--sidebar-hover",
            "#2c2d2e",
            "--sidebar-border",
            "#2f3031",
            "--status-height",
            "28",
            "--chrome-gap",
            "3",
            "--unfocused-sidebar-dim",
            "0.2",
            "--unfocused-terminal-dim",
            "0.3",
            "--stability-trace",
            "/tmp/bootty-trace.csv",
        ])
        .unwrap();

        let config = cli.load_config().unwrap();

        assert_eq!(
            config.window.macos_titlebar_style,
            MacosTitlebarStyle::Hidden
        );
        assert_eq!(config.window.window_decoration, WindowDecoration::None);
        assert_eq!(config.window.fullscreen_top_offset, Some(22.0));
        assert!(!config.window.fullscreen_tabs_in_notch);
        assert_eq!(
            config.colors.background,
            Some(Color::from_hex("#010203").unwrap())
        );
        assert_eq!(
            config.colors.foreground,
            Some(Color::from_hex("#040506").unwrap())
        );
        assert_eq!(
            config.colors.cursor,
            Some(Color::from_hex("#070809").unwrap())
        );
        assert_eq!(
            config.colors.cursor_text,
            Some(Color::from_hex("#0a0b0c").unwrap())
        );
        assert_eq!(
            config.colors.selection_background,
            Some(Color::from_hex("#111213").unwrap())
        );
        assert_eq!(
            config.colors.selection_foreground,
            Some(Color::from_hex("#141516").unwrap())
        );
        assert_eq!(
            config.colors.palette,
            [
                Color::from_hex("#000000").unwrap(),
                Color::from_hex("#ffffff").unwrap()
            ]
        );
        assert!(!config.colors.palette_generate);
        assert!(config.colors.palette_harmonious);
        assert_eq!(config.font.cell_width, Some(9.0));
        assert_eq!(config.font.cell_height, Some(20.0));
        assert!(!config.font.fit_cell_height);
        assert_eq!(config.font.baseline_adjustment, 1.5);
        assert_eq!(config.font.underline_position, 2.5);
        assert_eq!(config.font.underline_thickness, 1.25);
        assert_eq!(config.font.features.len(), 3);
        assert_eq!(config.cursor.style, Some(CursorStyleConfig::HollowBlock));
        assert_eq!(config.cursor.blink, Some(false));
        assert_eq!(
            config.session.env,
            [
                ("EDITOR".to_owned(), "nvim".to_owned()),
                ("BOOTTY_TEST".to_owned(), "1".to_owned())
            ]
        );
        assert_eq!(config.session.term, "xterm-test");
        assert_eq!(config.session.colorterm, "24bit");
        assert_eq!(
            config.session.max_scrollback,
            1234 * NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE
        );
        assert!(!config.session.glyph_protocol);
        assert_eq!(
            config.input.macos_option_as_alt,
            MacosOptionAsAltConfig::Left
        );
        assert_eq!(
            config.input.modifier_remap,
            ["right_alt=left_ctrl", "right_super=left_alt"]
        );
        assert_eq!(config.sidebar.position, SidebarPosition::Right);
        assert_eq!(
            config.sidebar.background,
            Some(Color::from_hex("#202122").unwrap())
        );
        assert_eq!(
            config.sidebar.foreground,
            Some(Color::from_hex("#262728").unwrap())
        );
        assert_eq!(
            config.sidebar.selected,
            Some(Color::from_hex("#292a2b").unwrap())
        );
        assert_eq!(
            config.sidebar.hover,
            Some(Color::from_hex("#2c2d2e").unwrap())
        );
        assert_eq!(
            config.sidebar.border,
            Some(Color::from_hex("#2f3031").unwrap())
        );
        assert_eq!(config.chrome.sidebar_width, 244.0);
        assert_eq!(config.chrome.status_height, 28.0);
        assert_eq!(config.chrome.gap, 3.0);
        assert_eq!(config.chrome.unfocused_sidebar_dim, 0.2);
        assert_eq!(config.chrome.unfocused_terminal_dim, 0.3);
        assert_eq!(
            config.diagnostics.stability_trace,
            Some(PathBuf::from("/tmp/bootty-trace.csv"))
        );
    }

    #[test]
    fn theme_flag_resolves_theme_colors_after_config_load() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            indoc! {r##"
                version = 1

                [colors]
                background = "#101112"
            "##},
        )
        .unwrap();

        let cli = Cli::try_parse_from([
            "bootty",
            "--config",
            config_path.to_str().unwrap(),
            "--theme",
            "Catppuccin Mocha",
        ])
        .unwrap();
        let config = cli.load_config().unwrap();

        assert_eq!(config.theme.as_deref(), Some("Catppuccin Mocha"));
        assert_eq!(
            config.colors.background,
            Some(Color::from_hex("#1e1e2e").unwrap())
        );
    }

    #[test]
    fn fullscreen_flag_without_value_uses_native_fullscreen() {
        let cli = Cli::try_parse_from(["bootty", "--defaults", "--fullscreen"]).unwrap();
        let config = cli.load_config().unwrap();

        assert_eq!(config.window.fullscreen, WindowFullscreen::Native);
    }

    #[test]
    fn defaults_mode_uses_temp_config_path_instead_of_xdg_config() {
        let cli = Cli::try_parse_from(["bootty", "--defaults"]).unwrap();
        let config = cli.load_config().unwrap();

        assert!(config.config_path.starts_with(std::env::temp_dir()));
        assert!(config.config_path.ends_with("config.toml"));
        assert_eq!(
            config,
            bootty_config::config::BoottyConfig {
                config_path: config.config_path.clone(),
                ..Default::default()
            }
        );
    }
}
