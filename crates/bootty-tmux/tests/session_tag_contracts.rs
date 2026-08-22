use std::sync::Mutex;

use anyhow::Result;
use bootty_mux::{
    command::MuxCommand,
    process::{CommandOutput, CommandRunner},
    snapshot::MuxSessionTag,
};
use bootty_tmux::TmuxBackend;

/// Answers every tmux invocation from a script and keeps the argv it was given.
#[derive(Default)]
struct RecordingRunner {
    stdout: String,
    calls: Mutex<Vec<Vec<String>>>,
}

impl RecordingRunner {
    fn answering(stdout: &str) -> Self {
        Self {
            stdout: stdout.to_owned(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("recorded tmux calls").clone()
    }
}

impl CommandRunner for RecordingRunner {
    fn run(&self, _program: &str, args: &[String]) -> Result<CommandOutput> {
        self.calls
            .lock()
            .expect("recorded tmux calls")
            .push(args.to_vec());
        Ok(CommandOutput {
            success: true,
            stdout: self.stdout.clone(),
            stderr: String::new(),
        })
    }
}

fn backend(stdout: &str) -> TmuxBackend<RecordingRunner> {
    TmuxBackend::with_runner("tmux", RecordingRunner::answering(stdout))
}

/// tmux renders an option nothing set as the empty string, so an untagged session and a tagged one
/// arrive down the same listing and are told apart by field, not by shape.
#[test]
fn a_snapshot_carries_the_bootty_tag_and_leaves_untagged_sessions_unclaimed() {
    let listing = concat!(
        "s\x1f$0\x1fwork\x1f9f3a\x1fspace-7\x1f1\x1f2\x1f%1\x1f4242\x1f/repo\x1fzsh\n",
        "s\x1f$1\x1fscratch\x1f\x1f\x1f0\x1f1\x1f%2\x1f4243\x1f/tmp\x1fbash\n",
    );
    let snapshot = backend(listing)
        .snapshot()
        .expect("parse the session listing");

    assert_eq!(
        snapshot.sessions[0].tag,
        MuxSessionTag {
            identity: Some("9f3a".to_owned()),
            space: Some("space-7".to_owned()),
        }
    );
    assert!(snapshot.sessions[1].tag.is_empty());
    // The fields after the tag still have to land where they did before it existed.
    assert_eq!(snapshot.sessions[0].name, "work");
    assert!(snapshot.sessions[0].active, "session_attached was 1");
    assert!(!snapshot.sessions[1].active);
    assert_eq!(snapshot.sessions[0].anchor.cwd.as_deref(), Some("/repo"));
    assert_eq!(snapshot.sessions[0].anchor.pane_pid, Some(4242));
    assert_eq!(snapshot.sessions[1].anchor.process.as_deref(), Some("bash"));
}

/// The stamps ride along in the same invocation as the create, so a session is never visible in an
/// untagged state that another Space could claim.
#[test]
fn creating_a_session_stamps_it_in_the_same_invocation() {
    let mut backend = backend("");
    backend
        .execute(MuxCommand::CreateProjectSession {
            session_id: "work".to_owned(),
            cwd: "/repo".to_owned(),
            tag: MuxSessionTag {
                identity: Some("9f3a".to_owned()),
                space: Some("space-7".to_owned()),
            },
        })
        .expect("create the session");

    assert_eq!(
        backend.runner().calls(),
        [[
            "new-session",
            "-d",
            "-s",
            "work",
            "-c",
            "/repo",
            ";",
            "set-option",
            "-t",
            "work",
            "@bootty_id",
            "9f3a",
            ";",
            "set-option",
            "-t",
            "work",
            "@bootty_space",
            "space-7",
        ]]
    );
}

/// A half of the tag that is `None` is a claim being dropped, so it unsets its option rather than
/// writing an empty value -- an empty user option still reads back as set.
#[test]
fn stamping_a_session_writes_both_halves_and_unsets_the_ones_being_dropped() {
    let mut backend = backend("");
    backend
        .execute(MuxCommand::StampSession {
            session_id: "$3".to_owned(),
            tag: MuxSessionTag {
                identity: Some("9f3a".to_owned()),
                space: None,
            },
        })
        .expect("stamp the session");

    assert_eq!(
        backend.runner().calls(),
        [[
            "set-option",
            "-t",
            "$3",
            "@bootty_id",
            "9f3a",
            ";",
            "set-option",
            "-u",
            "-t",
            "$3",
            "@bootty_space",
        ]]
    );
}
