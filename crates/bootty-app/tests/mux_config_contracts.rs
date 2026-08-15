use bootty_app::cli::Cli;
use bootty_app::config::MultiplexerBackendConfig;
use clap::Parser;

#[test]
fn ssh_remote_override_changes_only_the_host_before_validation() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
[multiplexer]
backend = "tmux"

[multiplexer.remote]
host = "old-host"
user = "dev"
port = 2222
program = "ssh-wrapper"
args = ["-i", "key"]
"#,
    )
    .expect("write config");

    let cli = Cli::parse_from([
        "bootty",
        "--config",
        path.to_str().expect("UTF-8 config path"),
        "--ssh-remote",
        "new-host",
    ]);
    let config = cli.load_config().expect("load overridden config");
    let remote = config
        .multiplexer
        .remote
        .expect("SSH override must keep a remote target");

    assert_eq!(config.multiplexer.backend, MultiplexerBackendConfig::Tmux);
    assert_eq!(remote.host, "new-host");
    assert_eq!(remote.user.as_deref(), Some("dev"));
    assert_eq!(remote.port, Some(2222));
    assert_eq!(remote.program, "ssh-wrapper");
    assert_eq!(remote.args, ["-i", "key"]);
}
