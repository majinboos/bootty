//! Bounded supervised subprocesses owned by one extension generation.

use std::{
    collections::{BTreeMap, VecDeque},
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

#[cfg(windows)]
use process_wrap::std::JobObject;
#[cfg(unix)]
use process_wrap::std::ProcessGroup;
use process_wrap::std::{ChildWrapper, CommandWrap};

const PROCESS_LIMIT: usize = 4;
const ARGUMENT_LIMIT: usize = 64;
const ARGUMENT_BYTES_LIMIT: usize = 64 * 1024;
const INPUT_LINE_LIMIT: usize = 256 * 1024;
const OUTPUT_LINE_LIMIT: usize = 256 * 1024;
const OUTPUT_QUEUE_LIMIT: usize = 64;
const WRITE_LIMIT: Duration = Duration::from_millis(250);
const STOP_LIMIT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProcessEvent {
    Stdout(String),
    Stderr(String),
    Error(String),
    Exit(Option<i32>),
    Dropped(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ProcessStatus {
    pub running: bool,
    pub queued: usize,
    pub dropped: usize,
}

#[derive(Default)]
struct EventQueue {
    events: VecDeque<ProcessEvent>,
    dropped: usize,
}

impl EventQueue {
    fn push(&mut self, event: ProcessEvent) {
        if self.events.len() == OUTPUT_QUEUE_LIMIT {
            self.events.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.events.push_back(event);
    }

    fn drain(&mut self, limit: usize) -> Vec<ProcessEvent> {
        let mut events = Vec::with_capacity(limit.min(self.events.len().saturating_add(1)));
        if self.dropped > 0 && events.len() < limit {
            events.push(ProcessEvent::Dropped(std::mem::take(&mut self.dropped)));
        }
        while events.len() < limit {
            let Some(event) = self.events.pop_front() else {
                break;
            };
            events.push(event);
        }
        events
    }
}

struct WriteRequest {
    line: String,
    response: mpsc::SyncSender<Result<(), String>>,
}

struct StopRequest {
    response: mpsc::SyncSender<Result<(), String>>,
}

struct ManagedProcess {
    writes: mpsc::SyncSender<WriteRequest>,
    stop: mpsc::SyncSender<StopRequest>,
    events: Arc<Mutex<EventQueue>>,
    running: Arc<AtomicBool>,
}

#[derive(Clone, Default)]
pub(super) struct ManagedProcesses {
    inner: Arc<ManagedProcessesInner>,
}

#[derive(Default)]
struct ManagedProcessesInner {
    processes: Mutex<BTreeMap<String, ManagedProcess>>,
    retired: AtomicBool,
}

impl Drop for ManagedProcessesInner {
    fn drop(&mut self) {
        self.retire();
    }
}

impl ManagedProcessesInner {
    fn retire(&self) {
        self.retired.store(true, Ordering::Release);
        if let Ok(mut processes) = self.processes.lock() {
            for process in processes.values() {
                let (response, _) = mpsc::sync_channel(1);
                let _ = process.stop.try_send(StopRequest { response });
            }
            processes.clear();
        }
    }
}

impl ManagedProcesses {
    pub(super) fn start(
        &self,
        id: String,
        argv: &[String],
        cwd: Option<&Path>,
    ) -> Result<ProcessStatus, String> {
        validate_process_id(&id)?;
        validate_argv(argv)?;
        if self.inner.retired.load(Ordering::Acquire) {
            return Err("extension generation is no longer active".to_owned());
        }

        let mut processes = self
            .inner
            .processes
            .lock()
            .map_err(|_| "extension process registry lock poisoned".to_owned())?;
        processes.retain(|_, process| process.running.load(Ordering::Acquire));
        if processes.contains_key(&id) {
            return Err(format!("extension process {id:?} is already running"));
        }
        if processes.len() >= PROCESS_LIMIT {
            return Err(format!(
                "extension process count exceeds the limit of {PROCESS_LIMIT}"
            ));
        }

        let (program, arguments) = argv
            .split_first()
            .ok_or_else(|| "extension process needs a program".to_owned())?;
        let mut command = Command::new(program);
        command.args(arguments);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut command = CommandWrap::from(command);
        #[cfg(unix)]
        command.wrap(ProcessGroup::leader());
        #[cfg(windows)]
        command.wrap(JobObject);
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let streams = (
            child.stdin().take(),
            child.stdout().take(),
            child.stderr().take(),
        );
        let (Some(stdin), Some(stdout), Some(stderr)) = streams else {
            let _ = child.kill();
            return Err("extension process pipes are unavailable".to_owned());
        };
        let events = Arc::new(Mutex::new(EventQueue::default()));
        let running = Arc::new(AtomicBool::new(true));
        let stdout = spawn_reader(stdout, Arc::clone(&events), ProcessStream::Stdout);
        let stderr = spawn_reader(stderr, Arc::clone(&events), ProcessStream::Stderr);
        let (write_tx, write_rx) = mpsc::sync_channel(64);
        let (stop_tx, stop_rx) = mpsc::sync_channel(1);
        spawn_writer(stdin, write_rx, Arc::clone(&events), Arc::clone(&running));
        spawn_supervisor(
            child,
            stop_rx,
            [stdout, stderr],
            Arc::clone(&events),
            Arc::clone(&running),
            Arc::clone(&self.inner),
        );
        let process = ManagedProcess {
            writes: write_tx,
            stop: stop_tx,
            events,
            running,
        };
        let status = status(&process);
        processes.insert(id, process);
        Ok(status)
    }

    pub(super) fn write(&self, id: &str, line: String) -> Result<(), String> {
        if line.len() > INPUT_LINE_LIMIT {
            return Err(format!(
                "extension process input exceeds the limit of {INPUT_LINE_LIMIT} bytes"
            ));
        }
        if line.contains(['\n', '\r']) {
            return Err("extension process input must be one line".to_owned());
        }
        let processes = self
            .inner
            .processes
            .lock()
            .map_err(|_| "extension process registry lock poisoned".to_owned())?;
        let process = processes
            .get(id)
            .ok_or_else(|| format!("extension process {id:?} is not running"))?;
        if !process.running.load(Ordering::Acquire) {
            return Err(format!("extension process {id:?} is not running"));
        }
        let (response, receiver) = mpsc::sync_channel(1);
        process
            .writes
            .try_send(WriteRequest { line, response })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "extension process input queue is full".to_owned(),
                mpsc::TrySendError::Disconnected(_) => {
                    format!("extension process {id:?} is not running")
                }
            })?;
        match receiver.recv_timeout(WRITE_LIMIT) {
            Ok(result) => result,
            Err(_) => match stop_and_wait(process) {
                Ok(()) => Err("extension process write timed out; process tree stopped".to_owned()),
                Err(_) => Err(
                    "extension process write timed out; termination is still pending".to_owned(),
                ),
            },
        }
    }

    pub(super) fn poll(&self, id: &str, limit: usize) -> Result<Vec<ProcessEvent>, String> {
        if limit == 0 || limit > OUTPUT_QUEUE_LIMIT {
            return Err(format!(
                "extension process poll limit must be between 1 and {OUTPUT_QUEUE_LIMIT}"
            ));
        }
        let processes = self
            .inner
            .processes
            .lock()
            .map_err(|_| "extension process registry lock poisoned".to_owned())?;
        let process = processes
            .get(id)
            .ok_or_else(|| format!("extension process {id:?} does not exist"))?;
        let mut events = process
            .events
            .lock()
            .map_err(|_| "extension process output queue lock poisoned".to_owned())?;
        Ok(events.drain(limit))
    }

    pub(super) fn stop(&self, id: &str) -> Result<(), String> {
        let processes = self
            .inner
            .processes
            .lock()
            .map_err(|_| "extension process registry lock poisoned".to_owned())?;
        let process = processes
            .get(id)
            .ok_or_else(|| format!("extension process {id:?} does not exist"))?;
        stop_and_wait(process)
    }

    pub(super) fn retire(&self) {
        self.inner.retire();
    }
}

fn stop_and_wait(process: &ManagedProcess) -> Result<(), String> {
    if !process.running.load(Ordering::Acquire) {
        return Ok(());
    }
    let (response, receiver) = mpsc::sync_channel(1);
    match process.stop.try_send(StopRequest { response }) {
        Ok(()) => match receiver.recv_timeout(STOP_LIMIT) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if process.running.load(Ordering::Acquire) {
                    Err("extension process stop timed out; termination is still pending".to_owned())
                } else {
                    Ok(())
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if process.running.load(Ordering::Acquire) {
                    Err("extension process supervisor is unavailable".to_owned())
                } else {
                    Ok(())
                }
            }
        },
        Err(mpsc::TrySendError::Full(_)) => wait_until_stopped(process),
        Err(mpsc::TrySendError::Disconnected(_)) => {
            if process.running.load(Ordering::Acquire) {
                Err("extension process supervisor is unavailable".to_owned())
            } else {
                Ok(())
            }
        }
    }
}

fn wait_until_stopped(process: &ManagedProcess) -> Result<(), String> {
    let deadline = std::time::Instant::now() + STOP_LIMIT;
    while process.running.load(Ordering::Acquire) {
        if std::time::Instant::now() >= deadline {
            return Err(
                "extension process stop timed out; termination is still pending".to_owned(),
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn status(process: &ManagedProcess) -> ProcessStatus {
    let (queued, dropped) = process
        .events
        .lock()
        .map(|events| (events.events.len(), events.dropped))
        .unwrap_or_default();
    ProcessStatus {
        running: process.running.load(Ordering::Acquire),
        queued,
        dropped,
    }
}

fn validate_process_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("extension process identity is invalid".to_owned());
    }
    Ok(())
}

fn validate_argv(argv: &[String]) -> Result<(), String> {
    if argv.is_empty() {
        return Err("extension process needs a program".to_owned());
    }
    if argv.len() > ARGUMENT_LIMIT {
        return Err(format!(
            "extension process argument count exceeds the limit of {ARGUMENT_LIMIT}"
        ));
    }
    let bytes = argv.iter().map(String::len).sum::<usize>();
    if bytes > ARGUMENT_BYTES_LIMIT {
        return Err(format!(
            "extension process arguments exceed the limit of {ARGUMENT_BYTES_LIMIT} bytes"
        ));
    }
    if argv.iter().any(|argument| argument.contains('\0')) {
        return Err("extension process arguments contain a null byte".to_owned());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ProcessStream {
    Stdout,
    Stderr,
}

fn spawn_reader(
    reader: impl Read + Send + 'static,
    events: Arc<Mutex<EventQueue>>,
    stream: ProcessStream,
) -> thread::JoinHandle<()> {
    thread::spawn(move || read_lines(BufReader::new(reader), &events, stream))
}

fn read_lines(mut reader: impl BufRead, events: &Mutex<EventQueue>, stream: ProcessStream) {
    let mut line = Vec::new();
    let mut truncated = false;
    loop {
        let buffer = match reader.fill_buf() {
            Ok([]) => break,
            Ok(buffer) => buffer,
            Err(error) => {
                push_event(events, ProcessEvent::Error(error.to_string()));
                break;
            }
        };
        let end = buffer.iter().position(|byte| *byte == b'\n');
        let consumed = end.map_or(buffer.len(), |index| index + 1);
        let content = &buffer[..end.unwrap_or(buffer.len())];
        if line.len() < OUTPUT_LINE_LIMIT {
            let remaining = OUTPUT_LINE_LIMIT - line.len();
            line.extend_from_slice(&content[..content.len().min(remaining)]);
            truncated |= content.len() > remaining;
        } else if !content.is_empty() {
            truncated = true;
        }
        reader.consume(consumed);
        if end.is_some() {
            publish_line(events, stream, &mut line, truncated);
            truncated = false;
        }
    }
    if !line.is_empty() || truncated {
        publish_line(events, stream, &mut line, truncated);
    }
}

fn publish_line(
    events: &Mutex<EventQueue>,
    stream: ProcessStream,
    line: &mut Vec<u8>,
    truncated: bool,
) {
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    let mut text = String::from_utf8_lossy(line).into_owned();
    line.clear();
    if truncated {
        text.push_str("\u{2026}[truncated]");
    }
    push_event(
        events,
        match stream {
            ProcessStream::Stdout => ProcessEvent::Stdout(text),
            ProcessStream::Stderr => ProcessEvent::Stderr(text),
        },
    );
}

fn push_event(events: &Mutex<EventQueue>, event: ProcessEvent) {
    if let Ok(mut events) = events.lock() {
        events.push(event);
    }
}

fn spawn_writer(
    mut stdin: ChildStdin,
    writes: mpsc::Receiver<WriteRequest>,
    events: Arc<Mutex<EventQueue>>,
    running: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        while running.load(Ordering::Acquire) {
            let request = match writes.recv_timeout(Duration::from_millis(10)) {
                Ok(request) => request,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            let result = writeln!(stdin, "{}", request.line)
                .and_then(|()| stdin.flush())
                .map_err(|error| format!("failed to write process stdin: {error}"));
            if let Err(error) = &result {
                push_event(&events, ProcessEvent::Error(error.clone()));
            }
            let failed = result.is_err();
            let _ = request.response.send(result);
            if failed {
                return;
            }
        }
    });
}

fn spawn_supervisor(
    mut child: Box<dyn ChildWrapper>,
    stop: mpsc::Receiver<StopRequest>,
    readers: [thread::JoinHandle<()>; 2],
    events: Arc<Mutex<EventQueue>>,
    running: Arc<AtomicBool>,
    owner: Arc<ManagedProcessesInner>,
) {
    thread::spawn(move || {
        let mut stop_response = None;
        loop {
            if owner.retired.load(Ordering::Acquire) {
                break;
            }
            if let Ok(request) = stop.try_recv() {
                stop_response = Some(request.response);
                break;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    // The protocol process can exit while one of its tool children still runs.
                    // Terminate the complete process tree before this generation reports exit.
                    let _ = child.start_kill();
                    let status = child.wait().unwrap_or(status);
                    join_readers(readers);
                    push_event(&events, ProcessEvent::Exit(status.code()));
                    running.store(false, Ordering::Release);
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    push_event(
                        &events,
                        ProcessEvent::Error(format!("failed to inspect process: {error}")),
                    );
                    break;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = child.start_kill();
        let result = match child.wait() {
            Ok(status) => {
                join_readers(readers);
                push_event(&events, ProcessEvent::Exit(status.code()));
                Ok(())
            }
            Err(error) => {
                let error = format!("failed to reap process: {error}");
                push_event(&events, ProcessEvent::Error(error.clone()));
                Err(error)
            }
        };
        running.store(false, Ordering::Release);
        if let Some(response) = stop_response {
            let _ = response.send(result);
        }
    });
}

fn join_readers(readers: [thread::JoinHandle<()>; 2]) {
    for reader in readers {
        let _ = reader.join();
    }
}
