use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Sender},
    },
};

use anyhow::Result;
use portable_pty::{MasterPty, PtySize};

use super::{
    CellMetrics, DrainStats, PublishedFrame, SessionLaunchConfig, TerminalCommand,
    TerminalGeometry, TerminalSession, WorkerHealth, spawn_shell,
};

#[derive(Debug)]
struct RetryResizeMaster {
    calls: Arc<AtomicUsize>,
}

impl MasterPty for RetryResizeMaster {
    fn resize(&self, _size: PtySize) -> Result<()> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            anyhow::bail!("first resize fails")
        }
        Ok(())
    }

    fn get_size(&self) -> Result<PtySize> {
        Ok(PtySize::default())
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>> {
        Ok(Box::new(std::io::empty()))
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>> {
        Ok(Box::new(std::io::sink()))
    }

    #[cfg(unix)]
    fn process_group_leader(&self) -> Option<i32> {
        None
    }

    #[cfg(unix)]
    fn as_raw_fd(&self) -> Option<std::os::fd::RawFd> {
        None
    }

    #[cfg(unix)]
    fn tty_name(&self) -> Option<PathBuf> {
        None
    }
}

fn session_for_delivery_test(
    pty_master: Box<dyn MasterPty + Send>,
    command_tx: Sender<TerminalCommand>,
) -> TerminalSession {
    let geometry = TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    };
    TerminalSession {
        command_tx,
        latest_frame: Arc::new(PublishedFrame::new()),
        latest_drain: Arc::new(Mutex::new(DrainStats::default())),
        pending_pty_len: Arc::new(AtomicUsize::new(0)),
        worker_health: Arc::new(WorkerHealth::default()),
        current_working_directory: Arc::new(Mutex::new(None)),
        geometry,
        display_scale: 1.0,
        render_cell: CellMetrics::new(8.0, 16.0),
        pty_master,
        child: None,
        tty_name: None,
    }
}

#[test]
fn failed_pty_resize_keeps_identical_retry_live() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (command_tx, _command_rx) = mpsc::channel();
    let mut session = session_for_delivery_test(
        Box::new(RetryResizeMaster {
            calls: Arc::clone(&calls),
        }),
        command_tx,
    );
    let geometry = TerminalGeometry {
        cols: 100,
        rows: 30,
        cell_width: 8,
        cell_height: 16,
    };

    assert!(session.resize(geometry).is_err());
    assert!(session.resize(geometry).is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn failed_display_scale_delivery_keeps_identical_retry_live() {
    let (command_tx, command_rx) = mpsc::channel();
    drop(command_rx);
    let mut session = session_for_delivery_test(
        Box::new(RetryResizeMaster {
            calls: Arc::new(AtomicUsize::new(0)),
        }),
        command_tx,
    );

    assert!(session.set_display_scale(2.0).is_err());
    assert!(session.set_display_scale(2.0).is_err());
}

#[test]
fn failed_cell_metric_delivery_keeps_identical_retry_live() {
    let (command_tx, command_rx) = mpsc::channel();
    drop(command_rx);
    let mut session = session_for_delivery_test(
        Box::new(RetryResizeMaster {
            calls: Arc::new(AtomicUsize::new(0)),
        }),
        command_tx,
    );
    let cell = CellMetrics::new(10.0, 20.0);

    assert!(session.set_render_cell_metrics(cell).is_err());
    assert!(session.set_render_cell_metrics(cell).is_err());
}

#[cfg(unix)]
#[test]
fn pending_child_owner_kills_a_real_spawned_process() {
    use std::{
        process::{Command, Stdio},
        thread,
        time::Duration,
    };

    let geometry = TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    };
    let (_, _, _, child, _) = spawn_shell(
        geometry,
        &SessionLaunchConfig {
            shell: Some("/bin/sh".to_owned()),
            args: vec!["-c".to_owned(), "while :; do sleep 60; done".to_owned()],
            ..Default::default()
        },
    )
    .expect("spawn pending terminal child");
    let pid = child
        .0
        .as_ref()
        .and_then(|child| child.process_id())
        .expect("pending child pid");

    drop(child);

    for _ in 0..100 {
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !alive {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
    panic!("pending child owner left process {pid} running");
}
