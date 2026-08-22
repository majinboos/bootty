#[cfg(unix)]
mod unix {
    use std::{
        env,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::Mutex,
    };

    use bootty_identity::ApplicationIdentity;
    use bootty_mux::{
        command::MuxCommand, process::CommandRunner, ssh::SshRemote,
        tmux_control::TmuxControlRunner, zellij::ZellijBackend,
    };
    use bootty_mux_model::SshTarget;

    static PATH_LOCK: Mutex<()> = Mutex::new(());

    struct PathGuard(Option<std::ffi::OsString>);

    impl Drop for PathGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(path) = self.0.take() {
                    env::set_var("PATH", path);
                } else {
                    env::remove_var("PATH");
                }
            }
        }
    }

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
        let argv_probe = executable(
            directory.path(),
            "argv-probe",
            "#!/bin/sh\nprintf '%s\\n' \"$@\"\n",
        );
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

        executable(
            directory.path(),
            "zellij",
            "#!/bin/sh\nprintf '%s\\n' \"$ZELLIJ_SOCKET_DIR\"\n",
        );
        let _path_lock = PATH_LOCK.lock().expect("PATH lock");
        let _path_guard = PathGuard(env::var_os("PATH"));
        unsafe { env::set_var("PATH", directory.path()) };

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

        assert!(production.sessions.is_empty());
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
