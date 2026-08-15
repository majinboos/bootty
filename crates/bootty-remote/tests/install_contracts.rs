#![cfg(unix)]

use std::{cell::RefCell, fs, path::PathBuf, rc::Rc};

use anyhow::Result;
use bootty_mux::process::{CommandOutput, CommandRunner};
use bootty_mux_model::SshTarget;
use bootty_remote::ssh::SshRemote;

#[derive(Clone)]
struct InstallerRunner {
    state: Rc<RefCell<InstallerState>>,
    candidate_succeeds: bool,
}

#[derive(Default)]
struct InstallerState {
    installed: bool,
    commands: Vec<String>,
}

impl CommandRunner for InstallerRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
        let command = args.last().cloned().unwrap_or_default();
        self.state
            .borrow_mut()
            .commands
            .push(format!("{program} {command}"));

        let output = if command == "uname -s && uname -m" {
            success(platform_probe())
        } else if command.ends_with("remote-ping") && command.contains(".upload") {
            if self.candidate_succeeds {
                compatible_ping()
            } else {
                failure("candidate is incompatible")
            }
        } else if command.ends_with("remote-ping") {
            if self.state.borrow().installed {
                compatible_ping()
            } else {
                failure("installed daemon is incompatible")
            }
        } else if command.contains("mv -f") {
            self.state.borrow_mut().installed = true;
            success("")
        } else {
            success("")
        };
        Ok(output)
    }
}

fn success(stdout: &str) -> CommandOutput {
    CommandOutput {
        success: true,
        stdout: stdout.to_owned(),
        stderr: String::new(),
    }
}

fn failure(stderr: &str) -> CommandOutput {
    CommandOutput {
        success: false,
        stdout: String::new(),
        stderr: stderr.to_owned(),
    }
}

fn compatible_ping() -> CommandOutput {
    success(&format!("2:{}", env!("CARGO_PKG_VERSION")))
}

fn platform_probe() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return "Darwin\narm64\n";
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return "Darwin\nx86_64\n";
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return "Linux\naarch64\n";
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return "Linux\nx86_64\n";
    #[allow(unreachable_code)]
    "unsupported\nunsupported\n"
}

struct PackagedDaemon {
    path: PathBuf,
    created: bool,
}

impl PackagedDaemon {
    fn install() -> Result<Self> {
        let path = std::env::current_exe()?.with_file_name("bootty-daemon");
        let created = !path.exists();
        if created {
            fs::write(&path, b"packaged daemon fixture")?;
        }
        Ok(Self { path, created })
    }
}

impl Drop for PackagedDaemon {
    fn drop(&mut self) {
        if self.created {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn remote() -> SshRemote {
    SshRemote::new(SshTarget::for_host("devbox"))
}

#[test]
fn remote_install_verifies_a_candidate_before_atomic_publication() -> Result<()> {
    let _artifact = PackagedDaemon::install()?;
    let successful_state = Rc::new(RefCell::new(InstallerState::default()));
    remote().ensure_daemon_with(&InstallerRunner {
        state: Rc::clone(&successful_state),
        candidate_succeeds: true,
    })?;

    let commands = &successful_state.borrow().commands;
    let candidate_ping = commands
        .iter()
        .position(|command| command.contains(".upload") && command.ends_with("remote-ping"))
        .expect("candidate ping");
    let publication = commands
        .iter()
        .position(|command| command.contains("mv -f"))
        .expect("atomic publication");
    let final_ping = commands
        .iter()
        .rposition(|command| command.ends_with("remote-ping"))
        .expect("installed ping");
    assert!(candidate_ping < publication);
    assert!(publication < final_ping);
    assert!(successful_state.borrow().installed);

    let failed_state = Rc::new(RefCell::new(InstallerState::default()));
    let error = remote()
        .ensure_daemon_with(&InstallerRunner {
            state: Rc::clone(&failed_state),
            candidate_succeeds: false,
        })
        .expect_err("an incompatible candidate must not publish");
    assert!(error.to_string().contains("uploaded Bootty daemon"));
    let failed_commands = &failed_state.borrow().commands;
    assert!(
        !failed_commands
            .iter()
            .any(|command| command.contains("mv -f"))
    );
    assert!(
        failed_commands
            .iter()
            .any(|command| command.contains("rm -f") && command.contains(".upload"))
    );
    assert!(!failed_state.borrow().installed);
    Ok(())
}
