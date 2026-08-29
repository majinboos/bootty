use std::env;
use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

use super::{Args, Layout, Linkage, print_dist_files};
use crate::command;
use crate::daemon::{DaemonTarget, TARGETS};
use crate::filesystem::{copy_file, files_recursive, recreate_dir};

pub(super) fn run(args: Args, layout: &Layout) -> Result<()> {
    if args.dev {
        bail!("Windows packaging does not support --dev");
    }
    if env::var_os("BOOTTY_DAEMON_OUTPUT_DIR").is_none_or(|value| value.is_empty()) {
        bail!("Windows packaging requires BOOTTY_DAEMON_OUTPUT_DIR with all daemon targets staged");
    }

    crate::daemon::verify(&layout.daemon_output_dir)?;

    let architecture = if env::var("PROCESSOR_ARCHITECTURE").is_ok_and(|value| value == "ARM64") {
        "arm64"
    } else {
        "x64"
    };
    let bundle_name = format!("{}-windows-{architecture}", layout.app_name);
    let bundle_root = layout.dist_dir.join(&bundle_name);

    recreate_dir(&layout.dist_dir)?;
    fs::create_dir_all(&bundle_root)
        .with_context(|| format!("failed to create {}", bundle_root.display()))?;

    build_bootty(layout)?;
    let profile_dir = layout.target_root.join(layout.profile);
    let binary = profile_dir.join("bootty.exe");
    if !binary.is_file() {
        bail!("expected built binary at {}", binary.display());
    }
    copy_file(&binary, &bundle_root.join("bootty.exe"))?;

    let host_daemon = layout
        .daemon_output_dir
        .join(DaemonTarget::X86_64PcWindowsMsvc.artifact_name());
    copy_file(&host_daemon, &bundle_root.join("bootty-daemon.exe"))?;
    let bundled_daemons = bundle_root.join("daemons");
    fs::create_dir_all(&bundled_daemons)
        .with_context(|| format!("failed to create {}", bundled_daemons.display()))?;
    for target in TARGETS {
        let name = target.artifact_name();
        copy_file(
            &layout.daemon_output_dir.join(&name),
            &bundled_daemons.join(name),
        )?;
    }

    if layout.linkage == Linkage::Dynamic {
        copy_dynamic_libraries(&profile_dir, &bundle_root)?;
    }
    copy_runtime_library(&profile_dir, &bundle_root, "ghostty-vt.dll")?;

    let archive = layout.dist_dir.join(format!("{bundle_name}.zip"));
    write_zip(&bundle_root, &archive, &bundle_name)?;
    print_dist_files(layout)
}

fn build_bootty(layout: &Layout) -> Result<()> {
    crate::build::run_with_features(
        &crate::build::BuildArgs {
            fast: layout.profile == "fast-release",
            static_linkage: layout.linkage == Linkage::Static,
        },
        false,
    )
}

fn copy_dynamic_libraries(profile_dir: &Path, bundle_root: &Path) -> Result<()> {
    let rustc_libdir =
        command::stdout(std::process::Command::new("rustc").args(["--print", "target-libdir"]))?;
    for directory in [profile_dir.join("deps"), PathBuf::from(rustc_libdir.trim())] {
        if !directory.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("failed to read {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
            {
                copy_file(&path, &bundle_root.join(entry.file_name()))?;
            }
        }
    }
    Ok(())
}

fn copy_runtime_library(profile_dir: &Path, bundle_root: &Path, name: &str) -> Result<()> {
    let library = newest_named_file(profile_dir, name)?.with_context(|| {
        format!(
            "expected runtime DLL {name} under {}",
            profile_dir.display()
        )
    })?;
    copy_file(&library, &bundle_root.join(name))
}

fn newest_named_file(root: &Path, name: &str) -> Result<Option<PathBuf>> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for file in files_recursive(root)? {
        if file.file_name().is_none_or(|file_name| file_name != name) {
            continue;
        }
        let modified = fs::metadata(&file)?
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if newest.as_ref().is_none_or(|(time, _)| modified > *time) {
            newest = Some((modified, file));
        }
    }
    Ok(newest.map(|(_, path)| path))
}

fn write_zip(bundle_root: &Path, archive: &Path, bundle_name: &str) -> Result<()> {
    let file =
        File::create(archive).with_context(|| format!("failed to create {}", archive.display()))?;
    let mut zip = ZipWriter::new(BufWriter::new(file));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    for source in files_recursive(bundle_root)? {
        let relative = source.strip_prefix(bundle_root)?;
        let archive_path = Path::new(bundle_name).join(relative);
        let archive_name = archive_path.to_string_lossy().replace('\\', "/");
        zip.start_file(archive_name, options)?;
        io::copy(&mut File::open(&source)?, &mut zip)?;
    }
    zip.finish()?;
    Ok(())
}
