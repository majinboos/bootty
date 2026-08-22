#[cfg(unix)]
mod unix {
    use std::{
        env,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };

    use bootty_identity::ApplicationIdentity;
    use bootty_mux::{SshTarget, process::CommandRunner};
    use bootty_remote::ssh::SshRemote;
    use bootty_tmux::TmuxControlRunner;

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
    fn development_tmux_uses_a_distinct_server_namespace() {
        let directory = tempfile::tempdir().expect("temporary directory");
        executable(
            directory.path(),
            "argv-probe",
            "#!/bin/sh\nprintf '%s\\n' \"$@\"\n",
        );
        let status =
            std::process::Command::new(std::env::current_exe().expect("current test executable"))
                .args([
                    "--exact",
                    "unix::development_tmux_uses_a_distinct_server_namespace_helper",
                ])
                .env(HELPER_ENV, "1")
                .env(FIXTURE_ENV, directory.path())
                .env("PATH", directory.path())
                .status()
                .expect("run isolated backend identity check");

        assert!(status.success());
    }

    #[test]
    fn development_tmux_uses_a_distinct_server_namespace_helper() {
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
    }
}
