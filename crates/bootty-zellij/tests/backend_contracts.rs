use std::{cell::RefCell, rc::Rc};

use bootty_mux::{
    command::MuxCommand,
    process::{CommandOutput, CommandRunner},
};
use bootty_zellij::ZellijBackend;

#[derive(Clone, Default)]
struct RecordingRunner {
    calls: Rc<RefCell<Vec<Vec<String>>>>,
    stdout: String,
}

impl CommandRunner for RecordingRunner {
    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        let mut call = vec![program.to_owned()];
        call.extend(args.iter().cloned());
        self.calls.borrow_mut().push(call);
        Ok(CommandOutput {
            success: true,
            stdout: self.stdout.clone(),
            stderr: String::new(),
        })
    }
}

#[test]
fn zellij_translates_session_lifecycle_without_a_tmux_fallback() {
    let runner = RecordingRunner::default();
    let calls = runner.calls.clone();
    let mut backend = ZellijBackend::with_runner(runner);

    backend
        .execute(MuxCommand::CreateProjectSession {
            session_id: "next".to_owned(),
            cwd: "/next".to_owned(),
        })
        .unwrap();
    backend
        .execute(MuxCommand::DitchSession {
            session_id: "next".to_owned(),
        })
        .unwrap();
    backend
        .execute(MuxCommand::RenameSession {
            session_id: "project".to_owned(),
            name: "renamed".to_owned(),
        })
        .unwrap();

    assert_eq!(
        calls.borrow().as_slice(),
        vec![
            vec![
                "zellij",
                "--layout-string",
                "layout {\n  pane\n}",
                "attach",
                "--create-background",
                "next",
                "options",
                "--pane-frames",
                "false",
                "--simplified-ui",
                "true",
                "--show-startup-tips",
                "false",
                "--default-cwd",
                "/next",
            ],
            vec!["zellij", "kill-session", "next"],
            vec!["zellij", "action", "switch-session", "project"],
            vec!["zellij", "action", "rename-session", "renamed"],
        ]
        .into_iter()
        .map(|call| call.into_iter().map(str::to_owned).collect::<Vec<_>>())
        .collect::<Vec<_>>()
        .as_slice()
    );
}

#[test]
fn zellij_snapshot_keeps_backend_session_identity() {
    let backend = ZellijBackend::with_runner(RecordingRunner {
        calls: Rc::default(),
        stdout: "alpha\nbeta\n".to_owned(),
    });

    let snapshot = backend.snapshot().unwrap();

    assert_eq!(snapshot.active_session_id, None);
    assert_eq!(snapshot.sessions[0].id, "alpha");
    assert_eq!(snapshot.sessions[0].anchor.session_id, "alpha");
}
