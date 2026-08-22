#![cfg(unix)]

use std::{cell::RefCell, fs, path::PathBuf, rc::Rc};

use anyhow::Result;
use assert_fs::prelude::*;
use bootty_mux::process::{CommandOutput, CommandRunner};
use bootty_mux_model::SshTarget;
use bootty_remote::ssh::SshRemote;
use pretty_assertions::assert_eq;

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
            output(true, platform_probe())
        } else if command.ends_with("remote-ping") && command.contains(".upload") {
            if self.candidate_succeeds {
                compatible_ping()
            } else {
                output(false, "candidate is incompatible")
            }
        } else if command.ends_with("remote-ping") {
            if self.state.borrow().installed {
                compatible_ping()
            } else {
                output(false, "installed daemon is incompatible")
            }
        } else if command.contains("mv -f") {
            self.state.borrow_mut().installed = true;
            output(true, "")
        } else {
            output(true, "")
        };
        Ok(output)
    }
}

fn output(success: bool, message: &str) -> CommandOutput {
    CommandOutput {
        success,
        stdout: if success {
            message.to_owned()
        } else {
            String::new()
        },
        stderr: if success {
            String::new()
        } else {
            message.to_owned()
        },
    }
}

fn compatible_ping() -> CommandOutput {
    output(true, &format!("2:{}", env!("CARGO_PKG_VERSION")))
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
            assert_fs::fixture::ChildPath::new(&path).write_binary(b"packaged daemon fixture")?;
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
fn candidate_is_verified_before_publication() -> Result<()> {
    let _artifact = PackagedDaemon::install()?;
    let successful = Rc::new(RefCell::new(InstallerState::default()));
    remote().ensure_daemon_with(&InstallerRunner {
        state: Rc::clone(&successful),
        candidate_succeeds: true,
    })?;
    let successful = successful.borrow();
    let position = |needle| {
        successful
            .commands
            .iter()
            .position(|command| command.contains(needle))
            .expect("observed install step")
    };
    let candidate_ping = position(".upload");
    let publication = position("mv -f");
    let final_ping = successful
        .commands
        .iter()
        .rposition(|command| command.ends_with("remote-ping"))
        .expect("final ping");
    let observed = (
        successful.installed,
        candidate_ping < publication,
        publication < final_ping,
    );
    assert_eq!(observed, (true, true, true));

    let failed = Rc::new(RefCell::new(InstallerState::default()));
    let error = remote()
        .ensure_daemon_with(&InstallerRunner {
            state: Rc::clone(&failed),
            candidate_succeeds: false,
        })
        .expect_err("an incompatible candidate must not publish");
    let failed = failed.borrow();
    let has = |needle| {
        failed
            .commands
            .iter()
            .any(|command| command.contains(needle))
    };
    let observed = (
        failed.installed,
        has("mv -f"),
        has("rm -f") && has(".upload"),
    );
    assert_eq!(observed, (false, false, true));
    assert_eq!(
        error.to_string(),
        "uploaded Bootty daemon on devbox did not start with protocol 2: candidate is incompatible"
    );
    Ok(())
}
