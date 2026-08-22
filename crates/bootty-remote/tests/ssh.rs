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
use pretty_assertions::assert_eq;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn remote() -> SshRemote {
    SshRemote::new(SshTarget::for_host("devbox"))
}

#[test]
fn direct_remote_commands_quote_login_shell_arguments() {
    let (program, argv) = remote().command("tmux", &args(&["rename-session", "-t", "it's $HOME"]));

    let observed = (
        program,
        argv.contains(&"devbox".to_owned()),
        argv.last().cloned(),
    );
    assert_eq!(
        observed,
        (
            "ssh".to_owned(),
            true,
            Some("'tmux' 'rename-session' '-t' 'it'\\''s $HOME'".to_owned())
        )
    );
}

#[test]
fn proxied_commands_preserve_hostile_arguments_without_remote_shell_parsing() {
    let (_, argv) = remote()
        .proxy_command(
            "sh",
            &args(&["-c", "test \"$1\" = \"it's remote\"", "sh", "it's remote"]),
        )
        .unwrap();
    let fields = argv
        .last()
        .expect("remote command")
        .split_whitespace()
        .collect::<Vec<_>>();
    assert_eq!(
        (fields[0].contains("bootty-daemon"), fields[1], fields.len()),
        (true, "remote-exec", 3)
    );
    assert_eq!(run_remote_command(fields[2]).unwrap(), 0);
}

#[test]
fn only_attach_clients_request_a_remote_terminal() {
    let remote = remote();
    let (_, polled) = remote
        .proxy_command("tmux", &args(&["list-sessions"]))
        .unwrap();
    let (_, attached) = remote
        .proxy_tty_command("tmux", &args(&["attach-session"]))
        .unwrap();

    let observed = (
        polled
            .windows(2)
            .any(|pair| pair == ["-o", "BatchMode=yes"]),
        polled.contains(&"-t".to_owned()),
        attached.contains(&"-t".to_owned()),
        attached
            .windows(2)
            .any(|pair| pair == ["-o", "BatchMode=yes"]),
    );
    assert_eq!(observed, (true, false, true, false));
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
    let observed = (
        options
            .iter()
            .find(|option| option.starts_with("ServerAliveInterval"))
            .map(String::as_str),
        options.contains(&"ConnectTimeout=5".to_owned()),
        options.contains(&"ServerAliveCountMax=3".to_owned()),
        argv.windows(2).any(|pair| pair == ["-p", "2222"]),
        argv.windows(2)
            .any(|pair| pair == ["-i", "C:\\keys\\id_ed25519"]),
    );
    assert_eq!(
        observed,
        (Some("ServerAliveInterval=30"), true, true, true, true)
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

    let remote = remote();
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
fn ssh_runner_checks_readiness_before_proxying_backend_execution() {
    let recorder = RecordingRunner::default();
    let calls = Rc::clone(&recorder.0);
    let runner = SshCommandRunner::new(remote(), recorder);

    runner
        .run("session-client", &args(&["list-sessions"]))
        .expect("execute proxied backend command");

    let calls = calls.borrow();
    let [(ping_program, ping_args), (command_program, command_args)] = calls.as_slice() else {
        panic!("expected readiness and backend calls, got {calls:?}");
    };
    let ping = ping_args.last().expect("readiness command");
    let command = command_args.last().expect("proxied command");
    assert_eq!(
        (
            ping_program.as_str(),
            ping.ends_with("remote-ping"),
            command_program.as_str(),
            command.contains(" remote-exec "),
            command.contains("session-client"),
        ),
        ("ssh", true, "ssh", true, false)
    );
}
