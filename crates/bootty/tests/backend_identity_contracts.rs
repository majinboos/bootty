#[cfg(unix)]
mod unix {
    use std::{
        env,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };

    use bootty_app::application_identity::ApplicationIdentity;
    use bootty_mux::{SshTarget, command::MuxCommand, process::CommandRunner};
    use bootty_remote::ssh::SshRemote;
    use bootty_tmux::TmuxControlRunner;
    use bootty_zellij::ZellijBackend;

    const HELPER_ENV: &str = "BOOTTY_BACKEND_IDENTITY_CONTRACT_HELPER";
    const FIXTURE_ENV: &str = "BOOTTY_BACKEND_IDENTITY_CONTRACT_FIXTURE";

    fn executable(directory: &Path, name: &str, source: &str) -> PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, source).expect("write executable");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make executable");
        path
    }

    #[test]
    fn development_local_backends_use_distinct_server_namespaces() {
        let directory = tempfile::tempdir().expect("temporary directory");
        executable(
            directory.path(),
            "argv-probe",
            "#!/bin/sh\nprintf '%s\\n' \"$@\"\n",
        );
        executable(
            directory.path(),
            "zellij",
            "#!/bin/sh\nprintf '%s\\n' \"$ZELLIJ_SOCKET_DIR\"\n",
        );

        let status =
            std::process::Command::new(std::env::current_exe().expect("current test executable"))
                .args([
                    "--exact",
                    "unix::development_local_backends_use_distinct_server_namespaces_helper",
                ])
                .env(HELPER_ENV, "1")
                .env(FIXTURE_ENV, directory.path())
                .env("PATH", directory.path())
                .status()
                .expect("run isolated backend identity check");

        assert!(status.success());
    }

    #[test]
    fn development_local_backends_use_distinct_server_namespaces_helper() {
        if env::var_os(HELPER_ENV).is_none() {
            return;
        }

        let fixture = PathBuf::from(env::var_os(FIXTURE_ENV).expect("fixture directory"));
        let argv_probe = fixture.join("argv-probe");
        let command = vec![
            "kill-session".to_owned(),
            "-t".to_owned(),
            "build".to_owned(),
        ];

        let production = TmuxControlRunner::for_identity(ApplicationIdentity::Production)
            .run(argv_probe.to_str().expect("probe path"), &command)
            .expect("production tmux command");
        let development = TmuxControlRunner::for_identity(ApplicationIdentity::Development)
            .run(argv_probe.to_str().expect("probe path"), &command)
            .expect("development tmux command");

        assert_eq!(production.stdout, "kill-session\n-t\nbuild\n");
        assert_eq!(
            development.stdout,
            "-L\nbootty-dev\nkill-session\n-t\nbuild\n"
        );

        let remote = SshRemote::new(SshTarget {
            host: "remote.example".to_owned(),
            user: None,
            port: None,
            program: argv_probe.to_string_lossy().into_owned(),
            args: Vec::new(),
        });
        let remote = TmuxControlRunner::for_remote(remote)
            .run("tmux", &command)
            .expect("remote tmux command");
        assert!(!remote.stdout.contains("bootty-dev"));
        assert!(remote.stdout.contains("'tmux' 'kill-session' '-t' 'build'"));

        let production = ZellijBackend::for_identity(ApplicationIdentity::Production)
            .expect("production zellij backend")
            .snapshot()
            .expect("production zellij snapshot");
        let mut development = ZellijBackend::for_identity(ApplicationIdentity::Development)
            .expect("development zellij backend");
        let development_snapshot = development.snapshot().expect("development zellij snapshot");
        development
            .execute(MuxCommand::DitchSession {
                session_id: "unused".to_owned(),
            })
            .expect("development zellij mutation");

        assert_eq!(production.sessions, Vec::new());
        let socket_dir = &development_snapshot.sessions[0].name;
        assert!(socket_dir.ends_with("/zellij"));
        assert!(Path::new(socket_dir).is_dir());
        assert_eq!(
            std::fs::metadata(socket_dir)
                .expect("socket directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}
