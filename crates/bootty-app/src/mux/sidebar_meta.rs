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
}

pub fn collect_sidebar_metadata(sessions: &[MuxSession]) -> SidebarMetadata {
    let mut metadata = SidebarMetadata {
        usage_lines: collect_usage_lines(32),
        ..SidebarMetadata::default()
    };
    let process_cpu = tmux_active_process_cpu();
    let agent_status = tmux_agent_status();
    let process_status = tmux_active_process_status();
    for session in sessions {
        let repo = session
            .anchor
            .cwd
            .as_deref()
            .map(|cwd| SidebarSessionMetadata {
                branch: git_branch(cwd).filter(|branch| !branch.is_empty()),
                diff: git_diff_stat(cwd),
                ..SidebarSessionMetadata::default()
            })
            .unwrap_or_default();
        let tmux = tmux_session_options(&session.id).unwrap_or_default();
        let session_meta = SidebarSessionMetadata {
            branch: repo.branch,
            diff: repo.diff,
            attention: tmux.attention,
            status: tmux.status,
            progress: tmux.progress,
            process_cpu: process_cpu.get(&session.id).cloned(),
            agent_status: agent_status.get(&session.id).cloned(),
            processes: process_status
                .get(&session.id)
                .cloned()
                .into_iter()
                .collect(),
        };
        if !session_meta.is_empty() {
            metadata.insert(session.name.clone(), session_meta);
        }
    }
    metadata
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

fn tmux_session_options(target: &str) -> Option<SidebarSessionMetadata> {
    let output = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            target,
            "#{@attention}\t#{@sidebar_status}\t#{@sidebar_progress}",
        ])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_tmux_session_options(&String::from_utf8_lossy(&output.stdout))
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

fn tmux_active_process_cpu() -> BTreeMap<String, String> {
    tmux_active_process_status()
        .into_iter()
        .map(|(session, status)| (session, format!("{:.1}%", status.cpu_pct)))
        .collect()
}

fn tmux_active_process_status() -> BTreeMap<String, ProcessStatus> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{session_id}\t#{pane_active}\t#{pane_pid}\t#{pane_current_command}",
        ])
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return BTreeMap::new();
    };
    if !output.status.success() {
        return BTreeMap::new();
    }

    let samples = build_process_info();
    let children = build_children(&samples);
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_tmux_active_pane_process)
        .filter_map(|(session_id, pid, command)| {
            let pid = pid.parse::<u32>().ok()?;
            let mut memo = HashMap::new();
            let (cpu_pct, mem_bytes) = subtree_usage(pid, &children, &samples, &mut memo);
            Some((
                session_id,
                ProcessStatus {
                    name: command,
                    cpu_pct,
                    mem_bytes,
                },
            ))
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

fn parse_tmux_active_pane_process(line: &str) -> Option<(String, String, String)> {
    let mut fields = line.split('\t');
    let session_id = fields.next()?;
    let active = fields.next()?;
    let pid = fields.next()?;
    let command = fields.next()?.trim();
    (active == "1" && !pid.is_empty() && !command.is_empty()).then(|| {
        (
            session_id.to_owned(),
            pid.to_owned(),
            command.rsplit('/').next().unwrap_or(command).to_owned(),
        )
    })
}

fn tmux_agent_status() -> BTreeMap<String, String> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{session_id}\t#{pane_active}\t#{pane_id}\t#{pane_current_command}",
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
        .filter_map(parse_agent_target)
        .filter_map(|(session_id, pane_id, agent)| {
            capture_agent_status(&pane_id, &agent).map(|status| (session_id, status))
        })
        .collect()
}

fn parse_agent_target(line: &str) -> Option<(String, String, String)> {
    let mut fields = line.split('\t');
    let session_id = fields.next()?;
    let active = fields.next()?;
    let pane_id = fields.next()?;
    let command = fields.next()?.to_ascii_lowercase();
    if active != "1" || pane_id.is_empty() {
        return None;
    }
    matches!(command.as_str(), "claude" | "codex" | "opencode")
        .then(|| (session_id.to_owned(), pane_id.to_owned(), command))
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
    fn parses_tmux_status_and_progress_options() {
        let meta = parse_tmux_session_options("1\treview needed\t142\n").unwrap();

        assert!(meta.attention);
        assert_eq!(meta.status.as_deref(), Some("review needed"));
        assert_eq!(meta.progress, Some(100));
    }
}
