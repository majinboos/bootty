use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    process::{Command, Stdio},
};

use super::snapshot::MuxSession;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SidebarMetadata {
    sessions: BTreeMap<String, SidebarSessionMetadata>,
    usage_lines: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SidebarSessionMetadata {
    pub branch: Option<String>,
    pub diff: Option<DiffStat>,
    pub attention: bool,
    pub status: Option<String>,
    pub progress: Option<u8>,
    pub process_cpu: Option<String>,
    pub agent_status: Option<String>,
    pub processes: Vec<ProcessStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarMetadataSession {
    id: String,
    name: String,
    cwd: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessStatus {
    pub name: String,
    pub cpu_pct: f32,
    pub mem_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffStat {
    pub added: u32,
    pub removed: u32,
}

impl SidebarMetadataSession {
    fn from_mux_session(session: &MuxSession) -> Self {
        Self {
            id: session.id.clone(),
            name: session.name.clone(),
            cwd: session.anchor.cwd.clone(),
        }
    }
}

impl SidebarMetadata {
    pub fn get(&self, session_name: &str) -> Option<&SidebarSessionMetadata> {
        self.sessions.get(session_name)
    }

    pub fn insert(&mut self, session_name: impl Into<String>, metadata: SidebarSessionMetadata) {
        self.sessions.insert(session_name.into(), metadata);
    }

    pub fn usage_lines(&self) -> &[String] {
        &self.usage_lines
    }

    pub fn set_usage_lines(&mut self, usage_lines: Vec<String>) {
        self.usage_lines = usage_lines;
    }
}

pub fn sidebar_metadata_sessions(sessions: &[MuxSession]) -> Vec<SidebarMetadataSession> {
    let mut metadata_sessions = Vec::with_capacity(sessions.len());
    for session in sessions {
        if !needs_sidebar_metadata_request(session) {
            continue;
        }
        metadata_sessions.push(SidebarMetadataSession::from_mux_session(session));
    }
    metadata_sessions
}

fn needs_sidebar_metadata_request(session: &MuxSession) -> bool {
    session.id.starts_with('$') || session.anchor.cwd.is_some()
}

pub fn collect_sidebar_metadata(sessions: &[SidebarMetadataSession]) -> SidebarMetadata {
    let mut metadata = SidebarMetadata {
        usage_lines: collect_usage_lines(32),
        ..SidebarMetadata::default()
    };
    let tmux_metadata = has_tmux_sessions(sessions).then(|| {
        let active_panes = tmux_active_panes();
        TmuxSidebarMetadata {
            process_status: tmux_active_process_status(&active_panes),
            agent_status: tmux_agent_status(&active_panes),
            session_options: tmux_session_options_by_id(),
        }
    });
    let mut repo_metadata = HashMap::<&str, (Option<String>, Option<DiffStat>)>::new();
    for session in sessions {
        let (branch, diff) = session
            .cwd
            .as_deref()
            .map(|cwd| {
                repo_metadata
                    .entry(cwd)
                    .or_insert_with(|| {
                        (
                            git_branch(cwd).filter(|branch| !branch.is_empty()),
                            git_diff_stat(cwd),
                        )
                    })
                    .clone()
            })
            .unwrap_or_default();
        let tmux = tmux_metadata
            .as_ref()
            .and_then(|metadata| metadata.session_options.get(&session.id))
            .cloned()
            .unwrap_or_default();
        let process = tmux_metadata
            .as_ref()
            .and_then(|metadata| metadata.process_status.get(&session.id));
        let session_meta = SidebarSessionMetadata {
            branch,
            diff,
            attention: tmux.attention,
            status: tmux.status,
            progress: tmux.progress,
            process_cpu: process.map(|status| format!("{:.1}%", status.cpu_pct)),
            agent_status: tmux_metadata
                .as_ref()
                .and_then(|metadata| metadata.agent_status.get(&session.id))
                .cloned(),
            processes: process.cloned().into_iter().collect(),
        };
        if !session_meta.is_empty() {
            metadata.insert(session.name.clone(), session_meta);
        }
    }
    metadata
}

struct TmuxSidebarMetadata {
    process_status: BTreeMap<String, ProcessStatus>,
    agent_status: BTreeMap<String, String>,
    session_options: BTreeMap<String, SidebarSessionMetadata>,
}

fn has_tmux_sessions(sessions: &[SidebarMetadataSession]) -> bool {
    sessions.iter().any(|session| session.id.starts_with('$'))
}

impl SidebarSessionMetadata {
    fn is_empty(&self) -> bool {
        self.branch.is_none()
            && self.diff.is_none()
            && !self.attention
            && self.status.is_none()
            && self.progress.is_none()
            && self.process_cpu.is_none()
            && self.agent_status.is_none()
            && self.processes.is_empty()
    }
}

fn tmux_session_options_by_id() -> BTreeMap<String, SidebarSessionMetadata> {
    let output = Command::new("tmux")
        .args([
            "list-sessions",
            "-F",
            "#{session_id}\t#{@attention}\t#{@sidebar_status}\t#{@sidebar_progress}",
        ])
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return BTreeMap::new();
    };
    if !output.status.success() {
        return BTreeMap::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_tmux_session_options_line)
        .collect()
}

fn parse_tmux_session_options_line(line: &str) -> Option<(String, SidebarSessionMetadata)> {
    let (session_id, options) = line.split_once('\t')?;
    (!session_id.is_empty()).then_some(())?;
    parse_tmux_session_options(options).map(|metadata| (session_id.to_owned(), metadata))
}

fn parse_tmux_session_options(output: &str) -> Option<SidebarSessionMetadata> {
    let line = output.lines().next().unwrap_or_default();
    let mut fields = line.split('\t');
    let attention = fields.next().is_some_and(|field| field == "1");
    let status = fields
        .next()
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(str::to_owned);
    let progress = fields
        .next()
        .and_then(|field| field.parse::<u8>().ok())
        .map(|progress| progress.min(100));
    let meta = SidebarSessionMetadata {
        attention,
        status,
        progress,
        ..SidebarSessionMetadata::default()
    };
    (!meta.is_empty()).then_some(meta)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TmuxActivePane {
    session_id: String,
    pane_id: String,
    pid: u32,
    command: String,
}

fn tmux_active_panes() -> Vec<TmuxActivePane> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{session_id}\t#{pane_active}\t#{pane_id}\t#{pane_pid}\t#{pane_current_command}",
        ])
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_tmux_active_pane)
        .collect()
}

fn tmux_active_process_status(panes: &[TmuxActivePane]) -> BTreeMap<String, ProcessStatus> {
    if panes.is_empty() {
        return BTreeMap::new();
    }

    let samples = build_process_info();
    let children = build_children(&samples);
    panes
        .iter()
        .map(|pane| {
            let mut memo = HashMap::new();
            let (cpu_pct, mem_bytes) = subtree_usage(pane.pid, &children, &samples, &mut memo);
            (
                pane.session_id.clone(),
                ProcessStatus {
                    name: pane.command.clone(),
                    cpu_pct,
                    mem_bytes,
                },
            )
        })
        .collect()
}

#[derive(Clone, Default)]
struct ProcSample {
    ppid: u32,
    cpu_pct: f32,
    rss_bytes: u64,
}

fn build_process_info() -> HashMap<u32, ProcSample> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,pcpu=,rss="])
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let mut samples = HashMap::new();
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(cpu), Some(rss)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(pid), Ok(ppid), Ok(rss)) =
            (pid.parse::<u32>(), ppid.parse::<u32>(), rss.parse::<u64>())
        else {
            continue;
        };
        samples.insert(
            pid,
            ProcSample {
                ppid,
                cpu_pct: cpu.parse::<f32>().unwrap_or(0.0).max(0.0),
                rss_bytes: rss.saturating_mul(1024),
            },
        );
    }
    samples
}

fn build_children(samples: &HashMap<u32, ProcSample>) -> HashMap<u32, Vec<u32>> {
    let mut children = HashMap::new();
    for (&pid, sample) in samples {
        children
            .entry(sample.ppid)
            .or_insert_with(Vec::new)
            .push(pid);
    }
    children
}

fn subtree_usage(
    pid: u32,
    children: &HashMap<u32, Vec<u32>>,
    samples: &HashMap<u32, ProcSample>,
    memo: &mut HashMap<u32, (f32, u64)>,
) -> (f32, u64) {
    if let Some(usage) = memo.get(&pid).copied() {
        return usage;
    }
    let mut cpu = samples
        .get(&pid)
        .map(|sample| sample.cpu_pct)
        .unwrap_or(0.0);
    let mut mem = samples
        .get(&pid)
        .map(|sample| sample.rss_bytes)
        .unwrap_or(0);
    if let Some(kids) = children.get(&pid) {
        for kid in kids {
            let (kid_cpu, kid_mem) = subtree_usage(*kid, children, samples, memo);
            cpu += kid_cpu;
            mem = mem.saturating_add(kid_mem);
        }
    }
    memo.insert(pid, (cpu, mem));
    (cpu, mem)
}

fn parse_tmux_active_pane(line: &str) -> Option<TmuxActivePane> {
    let mut fields = line.split('\t');
    let session_id = fields.next()?;
    let active = fields.next()?;
    let pane_id = fields.next()?;
    let pid = fields.next()?;
    let command = fields.next()?.trim();
    if active != "1" || session_id.is_empty() || pane_id.is_empty() || command.is_empty() {
        return None;
    }
    let pid = pid.parse::<u32>().ok()?;
    Some(TmuxActivePane {
        session_id: session_id.to_owned(),
        pane_id: pane_id.to_owned(),
        pid,
        command: command.rsplit('/').next().unwrap_or(command).to_owned(),
    })
}

fn tmux_agent_status(panes: &[TmuxActivePane]) -> BTreeMap<String, String> {
    panes
        .iter()
        .filter_map(|pane| {
            agent_command(&pane.command).and_then(|agent| {
                capture_agent_status(&pane.pane_id, agent)
                    .map(|status| (pane.session_id.clone(), status))
            })
        })
        .collect()
}

fn agent_command(command: &str) -> Option<&'static str> {
    if command.eq_ignore_ascii_case("claude") {
        Some("claude")
    } else if command.eq_ignore_ascii_case("codex") {
        Some("codex")
    } else if command.eq_ignore_ascii_case("opencode") {
        Some("opencode")
    } else {
        None
    }
}

fn capture_agent_status(pane_id: &str, agent: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", pane_id, "-p", "-S", "-30"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_agent_status(agent, &String::from_utf8_lossy(&output.stdout))
}

fn parse_agent_status(agent: &str, text: &str) -> Option<String> {
    let activity = match agent {
        "claude" => parse_star_activity(text),
        "codex" => parse_codex_activity(text),
        "opencode" => parse_opencode_activity(text),
        _ => None,
    };
    if let Some(activity) = activity {
        return Some(format!("{agent} {activity}"));
    }
    if is_agent_asking(agent, text) {
        return Some(format!("{agent} asking"));
    }
    None
}

fn parse_star_activity(text: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        let trimmed = line.trim();
        let mut chars = trimmed.chars();
        let first = chars.next()?;
        if !matches!(
            first,
            '\u{00B7}'
                | '\u{2022}'
                | '\u{273B}'
                | '\u{22C6}'
                | '\u{2726}'
                | '\u{2727}'
                | '\u{2736}'
                | '\u{2722}'
                | '\u{273D}'
                | '\u{2733}'
        ) || chars.next() != Some(' ')
        {
            return None;
        }
        let rest = chars.collect::<String>();
        rest.contains('\u{2026}').then(|| {
            rest.split_whitespace()
                .next()
                .unwrap_or("working")
                .trim_end_matches('\u{2026}')
                .to_owned()
                + "…"
        })
    })
}

fn parse_codex_activity(text: &str) -> Option<String> {
    text.lines()
        .any(|line| line.trim().starts_with("• Working"))
        .then(|| "Working…".to_owned())
}

fn parse_opencode_activity(text: &str) -> Option<String> {
    let bottom = text.lines().rev().take(10).collect::<Vec<_>>().join(" ");
    (bottom.contains("esc") && bottom.contains("interrupt")).then(|| "Working…".to_owned())
}

fn is_agent_asking(agent: &str, text: &str) -> bool {
    match agent {
        "claude" => text
            .lines()
            .any(|line| line.contains("Enter to select") || line.contains("enter to select")),
        "codex" => text.lines().any(|line| line.contains("to submit answer")),
        "opencode" => {
            let text = text.lines().collect::<String>();
            text.contains("select") && text.contains("submit") && text.contains("dismiss")
        }
        _ => false,
    }
}

fn collect_usage_lines(width: u16) -> Vec<String> {
    let output = Command::new(ct_bin())
        .args([
            "tui",
            "usage-bars",
            "--sidebar",
            "--width",
            &width.to_string(),
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .filter(|line| !line.trim().is_empty())
        .collect()
}

fn ct_bin() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("CT_BIN") {
        return path.into();
    }
    let cargo_ct = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/"))
        .join(".cargo/bin/ct");
    if cargo_ct.exists() {
        return cargo_ct;
    }
    "ct".into()
}

fn git_branch(dir: &str) -> Option<String> {
    git_output(dir, ["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|output| output.trim().to_owned())
        .filter(|branch| !branch.is_empty())
}

fn git_diff_stat(dir: &str) -> Option<DiffStat> {
    let output = git_output(dir, ["diff", "HEAD", "--numstat"])?;
    parse_git_numstat(&output)
}

fn git_output<const N: usize>(dir: &str, args: [&str; N]) -> Option<String> {
    if !Path::new(dir).exists() {
        return None;
    }
    let output = Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-C", dir])
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parse_git_numstat(output: &str) -> Option<DiffStat> {
    let mut diff = DiffStat::default();
    for line in output.lines() {
        let mut fields = line.split('\t');
        let added = fields.next().and_then(|field| field.parse::<u32>().ok());
        let removed = fields.next().and_then(|field| field.parse::<u32>().ok());
        if let (Some(added), Some(removed)) = (added, removed) {
            diff.added = diff.added.saturating_add(added);
            diff.removed = diff.removed.saturating_add(removed);
        }
    }
    (diff.added > 0 || diff.removed > 0).then_some(diff)
}

#[cfg(test)]
mod tests {
    use super::super::snapshot::{MuxPaneAnchor, MuxSession, MuxWindow};
    use super::*;

    #[test]
    fn parses_git_numstat_into_added_and_removed_totals() {
        assert_eq!(
            parse_git_numstat("7\t4\tsrc/lib.rs\n-\t-\timage.png\n3\t2\tREADME.md\n"),
            Some(DiffStat {
                added: 10,
                removed: 6
            })
        );
    }

    #[test]
    fn empty_git_numstat_has_no_diff() {
        assert_eq!(parse_git_numstat(""), None);
    }

    #[test]
    fn sidebar_metadata_sessions_keep_only_worker_inputs() {
        let tmux_anchor = MuxPaneAnchor {
            session_id: "$1".to_owned(),
            pane_id: Some("%1".to_owned()),
            cwd: Some("/tmp/project".to_owned()),
            process: Some("zsh".to_owned()),
        };
        let native_repo_anchor = MuxPaneAnchor {
            session_id: "local-repo".to_owned(),
            pane_id: None,
            cwd: Some("/tmp/native".to_owned()),
            process: Some("zsh".to_owned()),
        };
        let native_empty_anchor = MuxPaneAnchor {
            session_id: "local-empty".to_owned(),
            pane_id: None,
            cwd: None,
            process: Some("zsh".to_owned()),
        };
        let sessions = vec![
            MuxSession {
                id: "$1".to_owned(),
                name: "work/api".to_owned(),
                active: true,
                anchor: tmux_anchor.clone(),
                active_window_id: Some("@1".to_owned()),
                windows: vec![MuxWindow {
                    id: "@1".to_owned(),
                    index: 0,
                    name: "editor".to_owned(),
                    active: true,
                    anchor: tmux_anchor,
                }],
            },
            MuxSession {
                id: "local-repo".to_owned(),
                name: "native/repo".to_owned(),
                active: false,
                anchor: native_repo_anchor,
                active_window_id: None,
                windows: Vec::new(),
            },
            MuxSession {
                id: "local-empty".to_owned(),
                name: "native/empty".to_owned(),
                active: false,
                anchor: native_empty_anchor,
                active_window_id: None,
                windows: Vec::new(),
            },
        ];

        let metadata_sessions = sidebar_metadata_sessions(&sessions);

        assert_eq!(
            metadata_sessions,
            vec![
                SidebarMetadataSession {
                    id: "$1".to_owned(),
                    name: "work/api".to_owned(),
                    cwd: Some("/tmp/project".to_owned()),
                },
                SidebarMetadataSession {
                    id: "local-repo".to_owned(),
                    name: "native/repo".to_owned(),
                    cwd: Some("/tmp/native".to_owned()),
                }
            ]
        );
    }

    #[test]
    fn native_sidebar_metadata_sessions_do_not_need_tmux_polling() {
        let sessions = vec![SidebarMetadataSession {
            id: "local".to_owned(),
            name: "local".to_owned(),
            cwd: None,
        }];

        assert!(!has_tmux_sessions(&sessions));
    }

    #[test]
    fn tmux_sidebar_metadata_sessions_need_tmux_polling() {
        let sessions = vec![SidebarMetadataSession {
            id: "$1".to_owned(),
            name: "work".to_owned(),
            cwd: None,
        }];

        assert!(has_tmux_sessions(&sessions));
    }

    #[test]
    fn parses_tmux_status_and_progress_options() {
        let meta = parse_tmux_session_options("1\treview needed\t142\n").unwrap();

        assert!(meta.attention);
        assert_eq!(meta.status.as_deref(), Some("review needed"));
        assert_eq!(meta.progress, Some(100));
    }

    #[test]
    fn parses_tmux_session_options_line_with_session_id() {
        let (session_id, meta) = parse_tmux_session_options_line("$2\t0\tbuilding\t64").unwrap();

        assert_eq!(session_id, "$2");
        assert!(!meta.attention);
        assert_eq!(meta.status.as_deref(), Some("building"));
        assert_eq!(meta.progress, Some(64));
    }

    #[test]
    fn empty_tmux_session_options_line_is_ignored() {
        assert_eq!(parse_tmux_session_options_line("$2\t\t\t"), None);
    }

    #[test]
    fn parses_active_tmux_pane_once_for_process_and_agent_metadata() {
        assert_eq!(
            parse_tmux_active_pane("$2\t1\t%7\t1234\t/opt/homebrew/bin/codex"),
            Some(TmuxActivePane {
                session_id: "$2".to_owned(),
                pane_id: "%7".to_owned(),
                pid: 1234,
                command: "codex".to_owned(),
            })
        );
        assert_eq!(parse_tmux_active_pane("$2\t0\t%7\t1234\tcodex"), None);
        assert_eq!(parse_tmux_active_pane("$2\t1\t%7\tbad\tcodex"), None);
    }

    #[test]
    fn agent_command_matches_known_agents_case_insensitively() {
        assert_eq!(agent_command("Codex"), Some("codex"));
        assert_eq!(agent_command("claude"), Some("claude"));
        assert_eq!(agent_command("opencode"), Some("opencode"));
        assert_eq!(agent_command("zsh"), None);
    }
}
