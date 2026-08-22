use std::{fs, time::Duration};
#[cfg(unix)]
use std::{sync::Arc, thread, time::Instant};

#[cfg(unix)]
use bootty_runtime::frame_source::TerminalFrameSource;
use bootty_runtime::{
    BenchmarkTrace, PtyBacklog, SessionLaunchConfig, TerminalSession, TerminalSessionConfig,
    TraceValue, drain_pty_backlog,
    geometry::TerminalGeometry,
    perf::{guard_frame_path, record_subprocess},
    scheduler::{RepaintScheduler, RepaintSignal},
};
use bootty_terminal::terminal_engine::TerminalEngine;
#[cfg(unix)]
use bootty_terminal::terminal_engine::{
    TERMINAL_PROGRAM, TERMINAL_PROGRAM_VERSION, TerminalCopyModeAction, TerminalSearchDirection,
    TerminalSelectionFormat,
};

#[test]
fn scheduler_prioritizes_input_and_backlog_over_idle_chrome() {
    let scheduler = RepaintScheduler::default();
    let idle = scheduler.recommend(RepaintSignal {
        drained_bytes: 0,
        drain_elapsed_us: 0,
        pending_bytes: 0,
        dirty_rows: 0,
        cursor_blinking: false,
        input_commands: 0,
    });
    let input = scheduler.recommend(RepaintSignal {
        input_commands: 1,
        ..RepaintSignal {
            drained_bytes: 0,
            drain_elapsed_us: 0,
            pending_bytes: 0,
            dirty_rows: 0,
            cursor_blinking: false,
            input_commands: 0,
        }
    });

    assert_eq!(idle, Duration::from_millis(900));
    assert_eq!(input, Duration::ZERO);
}

#[test]
#[should_panic(expected = "git status spawned a subprocess on the frame path")]
fn frame_path_guard_names_a_forbidden_subprocess() {
    let _guard = guard_frame_path();
    record_subprocess("git status");
}

#[test]
fn benchmark_trace_writes_sampled_json_lines() {
    let directory = tempfile::tempdir().expect("trace directory");
    let path = directory.path().join("trace.jsonl");
    let trace = BenchmarkTrace::create(&path, 2).expect("trace opens");

    trace.emit("first", &[("count", TraceValue::Usize(1))]);
    trace.emit("skipped", &[("count", TraceValue::Usize(2))]);
    trace.emit("third", &[("ready", TraceValue::Bool(true))]);
    drop(trace);

    let lines = fs::read_to_string(path).expect("trace reads");
    assert!(lines.contains("\"event\":\"first\""));
    assert!(!lines.contains("skipped"));
    assert!(lines.contains("\"event\":\"third\""));
}

#[test]
fn pty_backlog_drains_complete_chunks_in_order() {
    let mut backlog = PtyBacklog::new();
    backlog.push_back(b"first".to_vec());
    backlog.push_back(b"second".to_vec());
    let mut output = Vec::new();

    let stats = drain_pty_backlog(&mut backlog, |bytes| output.extend_from_slice(bytes));

    assert_eq!(output, b"firstsecond");
    assert_eq!(stats.bytes, output.len());
    assert_eq!(stats.chunks, 2);
    assert!(backlog.is_empty());
}

#[test]
fn bounded_pty_drain_preserves_split_synchronized_output_control() {
    let geometry = TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    };
    let mut engine = TerminalEngine::new(geometry).expect("terminal engine");
    let mut backlog = PtyBacklog::new();
    let mut bytes = vec![b'x'; 8191];
    bytes.extend_from_slice(b"\x1b[?2026h");
    backlog.push_back(bytes);
    let mut slices = Vec::new();

    let stats = drain_pty_backlog(&mut backlog, |slice| {
        slices.push(slice.to_vec());
        engine.write_vt(slice);
    });

    assert_eq!(stats.bytes, 8199);
    assert_eq!(slices.len(), 2);
    assert_eq!(slices[0].len(), 8192);
    assert_eq!(slices[0].last(), Some(&0x1b));
    assert_eq!(slices[1], b"[?2026h");
    assert!(
        engine
            .is_synchronized_output()
            .expect("query synchronized output mode")
    );
}

#[test]
fn completed_synchronized_output_batch_suppresses_intermediate_publish() {
    let geometry = TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    };
    let mut engine = TerminalEngine::new(geometry).expect("terminal engine");
    let mut backlog = PtyBacklog::new();
    backlog.push_back(b"\x1b[?2026hredraw\x1b[?2026l".to_vec());
    let mut observed = false;

    let stats = drain_pty_backlog(&mut backlog, |slice| {
        engine.write_vt(slice);
        observed |= engine.take_synchronized_output_observed();
    });

    assert_eq!(stats.bytes, 22);
    assert!(observed);
    assert!(
        !engine
            .is_synchronized_output()
            .expect("query synchronized output mode")
    );
    assert!(
        bootty_runtime::terminal_session::sync_output_suppresses_publish(
            false,
            observed,
            Duration::ZERO,
        )
    );
}

#[cfg(unix)]
#[test]
fn dropping_a_terminal_session_kills_its_owned_child() {
    let directory = tempfile::tempdir().expect("pid directory");
    let pid_path = directory.path().join("child.pid");
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/sh".to_owned()),
            args: vec![
                "-c".to_owned(),
                "echo $$ > \"$BOOTTY_TEST_PID\"; while :; do sleep 60; done".to_owned(),
            ],
            env: vec![(
                "BOOTTY_TEST_PID".to_owned(),
                pid_path.to_string_lossy().into_owned(),
            )],
            ..SessionLaunchConfig::default()
        },
        ..TerminalSessionConfig::default()
    };
    let session = TerminalSession::new_with_config(
        TerminalGeometry {
            cols: 20,
            rows: 4,
            cell_width: 8,
            cell_height: 16,
        },
        config,
        Arc::new(|| {}),
    )
    .expect("terminal starts");

    let pid = wait_for_pid(&pid_path);
    drop(session);

    for _ in 0..100 {
        if !process_alive(pid) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("terminal child {pid} survived session drop");
}

#[cfg(unix)]
#[test]
fn resize_updates_grid_size_only_after_worker_result() {
    let initial = TerminalGeometry {
        cols: 20,
        rows: 8,
        cell_width: 8,
        cell_height: 16,
    };
    let resized = TerminalGeometry {
        cols: 24,
        rows: 10,
        ..initial
    };
    let invalid = TerminalGeometry {
        cols: initial.cols,
        rows: 0,
        ..initial
    };
    let mut session = TerminalSession::new_with_repaint_wakeup(initial, Arc::new(|| {}))
        .expect("terminal starts");

    session.resize(resized).expect("terminal resizes");
    assert_eq!(session.grid_size(), (resized.cols, resized.rows));
    let frame = session.extract_frame().expect("resized frame");
    assert_eq!((frame.cols, frame.rows), (resized.cols, resized.rows));
    assert!(session.resize(invalid).is_err());
    assert_eq!(session.grid_size(), (resized.cols, resized.rows));
}

#[cfg(unix)]
#[test]
fn frame_resize_queues_without_waiting_for_worker_publication() {
    let initial = TerminalGeometry {
        cols: 20,
        rows: 8,
        cell_width: 8,
        cell_height: 16,
    };
    let resized = TerminalGeometry {
        cols: 24,
        rows: 10,
        ..initial
    };
    let mut session = TerminalSession::new_with_repaint_wakeup(initial, Arc::new(|| {}))
        .expect("terminal starts");

    let started = Instant::now();
    TerminalFrameSource::resize(&mut session, resized).expect("frame resize queues");

    assert_eq!(session.grid_size(), (resized.cols, resized.rows));
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "frame resize waited for worker publication"
    );
}

#[cfg(unix)]
#[test]
fn terminal_worker_response_operations_preserve_public_results() {
    let geometry = TerminalGeometry {
        cols: 20,
        rows: 8,
        cell_width: 8,
        cell_height: 16,
    };
    let mut session = TerminalSession::new_with_repaint_wakeup(geometry, Arc::new(|| {}))
        .expect("terminal starts");

    session.enter_copy_mode().expect("copy mode starts");
    assert!(session.copy_mode_active().expect("copy mode state reads"));
    let outcome = session
        .handle_copy_mode_action(TerminalCopyModeAction::Cancel)
        .expect("copy mode action completes");
    assert!(!outcome.active);
    assert_eq!(
        session
            .format_selection(TerminalSelectionFormat::PlainText)
            .expect("selection formatting completes"),
        None
    );
    assert!(
        !session
            .search_viewport("", TerminalSearchDirection::Current)
            .expect("search completes")
    );
    session
        .is_mouse_tracking()
        .expect("mouse tracking state reads");
    session
        .discard_pending_output()
        .expect("pending output discard completes");
}

#[cfg(unix)]
#[test]
fn terminal_launch_applies_one_managed_environment_and_process_policy() {
    let directory = tempfile::tempdir().expect("launch directory");
    let working_directory = directory.path().join("working");
    fs::create_dir(&working_directory).expect("working directory");
    let output_path = directory.path().join("launch.txt");
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/sh".to_owned()),
            args: vec![
                "-c".to_owned(),
                "printf '%s' \"$TERM|$COLORTERM|$TERM_PROGRAM|$TERM_PROGRAM_VERSION|${TERMINFO-unset}|${REMOVE_ME-unset}|$PWD|$1\" > \"$BOOTTY_TEST_OUTPUT\""
                    .to_owned(),
                "bootty-contract".to_owned(),
                "argument".to_owned(),
            ],
            working_directory: Some(working_directory.clone()),
            env: vec![
                (
                    "BOOTTY_TEST_OUTPUT".to_owned(),
                    output_path.to_string_lossy().into_owned(),
                ),
                ("TERM".to_owned(), "wrong".to_owned()),
                ("COLORTERM".to_owned(), "wrong".to_owned()),
                ("TERM_PROGRAM".to_owned(), "wrong".to_owned()),
                ("TERM_PROGRAM_VERSION".to_owned(), "wrong".to_owned()),
                ("TERMINFO".to_owned(), "wrong".to_owned()),
                ("REMOVE_ME".to_owned(), "present".to_owned()),
            ],
            env_remove: vec!["REMOVE_ME".to_owned()],
            term: "bootty-contract-term".to_owned(),
            colorterm: "bootty-contract-color".to_owned(),
        },
        ..TerminalSessionConfig::default()
    };
    let _session = TerminalSession::new_with_config(
        TerminalGeometry {
            cols: 20,
            rows: 4,
            cell_width: 8,
            cell_height: 16,
        },
        config,
        Arc::new(|| {}),
    )
    .expect("terminal starts");

    let output = wait_for_file(&output_path);
    let fields = output.split('|').collect::<Vec<_>>();
    assert_eq!(fields.len(), 8, "launch output: {output}");
    assert_eq!(fields[0], "bootty-contract-term");
    assert_eq!(fields[1], "bootty-contract-color");
    assert_eq!(fields[2], TERMINAL_PROGRAM);
    assert_eq!(fields[3], TERMINAL_PROGRAM_VERSION);
    assert_ne!(fields[4], "wrong");
    assert_eq!(fields[5], "unset");
    assert_eq!(
        fields[6],
        working_directory
            .canonicalize()
            .expect("canonical working directory")
            .to_string_lossy()
    );
    assert_eq!(fields[7], "argument");
}

#[cfg(unix)]
fn wait_for_pid(path: &std::path::Path) -> u32 {
    for _ in 0..100 {
        if let Ok(value) = fs::read_to_string(path)
            && let Ok(pid) = value.trim().parse()
        {
            return pid;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("terminal child did not publish its pid");
}

#[cfg(unix)]
fn wait_for_file(path: &std::path::Path) -> String {
    for _ in 0..100 {
        if let Ok(value) = fs::read_to_string(path) {
            return value;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("terminal child did not publish {}", path.display());
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}
