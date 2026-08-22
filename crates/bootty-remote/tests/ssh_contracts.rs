use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use bootty_mux::process::{CommandOutput, CommandRunner};
use bootty_mux_model::SshTarget;
use bootty_remote::{
    run_remote_command,
    ssh::{SshCommandRunner, SshRemote},
};

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn target(host: &str) -> SshTarget {
    SshTarget {
        host: host.to_owned(),
        user: None,
        port: None,
        program: "ssh".to_owned(),
        args: Vec::new(),
    }
}

#[test]
fn direct_remote_commands_quote_login_shell_arguments() {
    let remote = SshRemote::new(target("devbox"));
    let (program, argv) = remote.command("tmux", &args(&["rename-session", "-t", "it's $HOME"]));

    assert_eq!(program, "ssh");
    assert_eq!(
        argv.last().map(String::as_str),
        Some("'tmux' 'rename-session' '-t' 'it'\\''s $HOME'")
    );
    assert!(argv.contains(&"devbox".to_owned()));
}

#[test]
fn proxied_commands_preserve_hostile_arguments_without_remote_shell_parsing() {
    let remote = SshRemote::new(target("devbox"));
    let (_, argv) = remote
        .proxy_command(
            "sh",
            &args(&["-c", "test \"$1\" = \"it's remote\"", "sh", "it's remote"]),
        )
        .unwrap();
    let command = argv.last().expect("remote command");
    let mut fields = command.split_whitespace();
    assert!(fields.next().unwrap().contains("bootty-daemon"));
    assert_eq!(fields.next(), Some("remote-exec"));
    let payload = fields.next().expect("encoded command");
    assert_eq!(fields.next(), None);

    assert_eq!(run_remote_command(payload).unwrap(), 0);
}

#[test]
fn only_attach_clients_request_a_remote_terminal() {
    let remote = SshRemote::new(target("devbox"));
    let (_, polled) = remote
        .proxy_command("tmux", &args(&["list-sessions"]))
        .unwrap();
    let (_, attached) = remote
        .proxy_tty_command("tmux", &args(&["attach-session"]))
        .unwrap();

    assert!(
        polled
            .windows(2)
            .any(|pair| pair == ["-o", "BatchMode=yes"])
    );
    assert!(!polled.contains(&"-t".to_owned()));
    assert!(attached.contains(&"-t".to_owned()));
    assert!(
        !attached
            .windows(2)
            .any(|pair| pair == ["-o", "BatchMode=yes"])
    );
}

#[test]
fn explicit_connection_policy_precedes_bounded_defaults() {
    let remote = SshRemote::new(SshTarget {
        host: "10.0.0.4".to_owned(),
        user: Some("dev".to_owned()),
        port: Some(2222),
        program: "ssh".to_owned(),
        args: args(&["-o", "ServerAliveInterval=30", "-i", "C:\\keys\\id_ed25519"]),
    });
    let (_, argv) = remote.command("tmux", &args(&["list-sessions"]));

    let options = argv
        .windows(2)
        .filter(|pair| pair[0] == "-o")
        .map(|pair| pair[1].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        options
            .iter()
            .find(|option| option.starts_with("ServerAliveInterval")),
        Some(&"ServerAliveInterval=30".to_owned())
    );
    assert!(options.contains(&"ConnectTimeout=5".to_owned()));
    assert!(options.contains(&"ServerAliveCountMax=3".to_owned()));
    assert!(argv.windows(2).any(|pair| pair == ["-p", "2222"]));
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["-i", "C:\\keys\\id_ed25519"])
    );
    let separator = argv.iter().position(|arg| arg == "--").unwrap();
    assert_eq!(argv[separator + 1], "dev@10.0.0.4");
}

#[test]
fn cloned_remote_serializes_daemon_readiness() {
    #[derive(Clone)]
    struct PingRunner(Arc<AtomicUsize>);

    impl CommandRunner for PingRunner {
        fn run(&self, _program: &str, args: &[String]) -> Result<CommandOutput> {
            assert!(args.last().expect("ping").ends_with("remote-ping"));
            self.0.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(10));
            Ok(CommandOutput {
                success: true,
                stdout: format!("2:{}", env!("CARGO_PKG_VERSION")),
                stderr: String::new(),
            })
        }
    }

    let remote = SshRemote::new(target("devbox"));
    let calls = Arc::new(AtomicUsize::new(0));
    let handles = (0..4)
        .map(|_| {
            let remote = remote.clone();
            let runner = PingRunner(calls.clone());
            std::thread::spawn(move || remote.ensure_daemon_with(&runner))
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

type RecordedCalls = Rc<RefCell<Vec<(String, Vec<String>)>>>;

#[derive(Clone, Default)]
struct RecordingRunner(RecordedCalls);

impl CommandRunner for RecordingRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        self.0
            .borrow_mut()
            .push((program.to_owned(), args.to_vec()));
        Ok(CommandOutput {
            success: true,
            stdout: if args.last().is_some_and(|arg| arg.ends_with("remote-ping")) {
                format!("2:{}", env!("CARGO_PKG_VERSION"))
            } else {
                String::new()
            },
            stderr: String::new(),
        })
    }
}

#[test]
fn ssh_runner_composes_daemon_readiness_and_backend_execution() {
    let recorder = RecordingRunner::default();
    let calls = recorder.0.clone();
    let runner = SshCommandRunner::new(SshRemote::new(target("devbox")), recorder);

    runner
        .run("session-client", &args(&["list-sessions"]))
        .unwrap();

    let calls = calls.borrow();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "ssh");
    assert!(calls[0].1.last().unwrap().ends_with("remote-ping"));
    assert_eq!(calls[1].0, "ssh");
    let command = calls[1].1.last().unwrap();
    assert!(command.contains(" remote-exec "));
    assert!(!command.contains("session-client"));
}
