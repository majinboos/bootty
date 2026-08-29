#[cfg(unix)]
use std::process::Stdio;
use std::process::{Child, Command};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
pub fn configure_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
pub fn configure_group(_command: &mut Command) {}

pub fn terminate_group(child: &mut Child) {
    #[cfg(unix)]
    {
        signal_group(child.id(), "-TERM");
        for _ in 0..20 {
            if child.try_wait().ok().flatten().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        signal_group(child.id(), "-KILL");
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn signal_group(process_group: u32, signal: &str) {
    let _ = Command::new("/bin/kill")
        .args([signal, "--", &format!("-{process_group}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
