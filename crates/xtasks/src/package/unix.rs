use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::{Args, Layout, Linkage};
use crate::daemon::{DaemonTarget, TARGETS};
use crate::{command, filesystem};

const BINARY: &str = "bootty";
const DAEMON: &str = "bootty-daemon";

pub(super) fn run(args: Args, layout: &Layout) -> Result<()> {
    let zig_path = ensure_project_zig(layout.app_name)?;
    let host_daemon = build_daemon(layout)?;
    fs::create_dir_all(&layout.dist_dir)?;
    build_application(args, layout, &zig_path)?;

    match env::consts::OS {
        "macos" => package_macos(layout, &host_daemon)?,
        "linux" => package_linux(layout, &host_daemon)?,
        os => bail!("unsupported OS: {os}"),
    }
    super::print_dist_files(layout)
}

fn build_application(args: Args, layout: &Layout, zig_path: &Path) -> Result<()> {
    let mut command = Command::new("cargo");
    command
        .arg("build")
        .args(layout.cargo_profile_args())
        .args(["-p", "bootty", "--bin", BINARY]);
    if args.dev {
        command.args(["--features", "bootty-dev"]);
    }
    let path = prepend_path(zig_path.parent().context("Zig executable has no parent")?)?;
    command.env("PATH", path);
    if let Some(flags) = rustflags(layout) {
        command.env("RUSTFLAGS", flags);
    }
    command::run(&mut command)
}

fn build_daemon(layout: &Layout) -> Result<PathBuf> {
    if layout.all_daemons {
        // The daemon xtask owns cross compilation. Keeping this as an in-process
        // call avoids a second Rust bootstrap and preserves its target policy.
        crate::daemon::build_all()?;
        crate::daemon::verify(&layout.daemon_output_dir)?;
        Ok(layout
            .daemon_output_dir
            .join(host_daemon_target()?.artifact_name()))
    } else {
        let mut command = Command::new("cargo");
        command
            .arg("build")
            .args(layout.daemon_profile_args())
            .args(["-p", DAEMON, "--bin", DAEMON])
            .env("RUSTFLAGS", "");
        command::run(&mut command)?;
        Ok(layout.target_root.join(layout.daemon_profile).join(DAEMON))
    }
}

fn ensure_project_zig(app_name: &str) -> Result<PathBuf> {
    let mise = fs::read_to_string("mise.toml").context("failed to read mise.toml")?;
    let required = mise
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == "zig").then(|| value.trim().trim_matches('"'))
        })
        .unwrap_or_default();
    let zig = if command::program_exists("mise") {
        let output = command::stdout(Command::new("mise").args(["which", "zig"]));
        output
            .ok()
            .map(|path| PathBuf::from(path.trim()))
            .filter(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from("zig"))
    } else {
        PathBuf::from("zig")
    };
    let version = command::stdout(Command::new(&zig).arg("version")).with_context(|| {
        format!("Zig {required} is required to package {app_name}; install it with mise")
    })?;
    let version = version.trim();
    if !required.is_empty() && version != required {
        bail!(
            "Zig {required} is required to package {app_name}; found {version} at {}",
            zig.display()
        );
    }
    Ok(zig)
}

fn rustflags(layout: &Layout) -> Option<OsString> {
    if layout.linkage == Linkage::Static {
        return None;
    }
    let suffix = match env::consts::OS {
        "macos" => "-C prefer-dynamic -C link-arg=-Wl,-rpath,@executable_path/../Frameworks",
        "linux" => "-C link-arg=-Wl,-rpath,$ORIGIN/../lib",
        _ => return None,
    };
    Some(command::append_env("RUSTFLAGS", suffix))
}

fn prepend_path(directory: &Path) -> Result<OsString> {
    let mut paths = vec![directory.to_path_buf()];
    paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
    env::join_paths(paths).context("failed to construct PATH")
}

fn host_daemon_target() -> Result<DaemonTarget> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Ok(DaemonTarget::Aarch64AppleDarwin),
        ("macos", "x86_64") => Ok(DaemonTarget::X86_64AppleDarwin),
        ("linux", "x86_64") => Ok(DaemonTarget::X86_64UnknownLinuxGnu),
        ("linux", "aarch64") => Ok(DaemonTarget::Aarch64UnknownLinuxGnu),
        (os, arch) => bail!("unsupported host daemon target: {os} {arch}"),
    }
}

fn dynamic_dependencies(binary: &Path) -> Result<Vec<String>> {
    let output = match env::consts::OS {
        "macos" => {
            command::stdout(Command::new("otool").args([OsStr::new("-L"), binary.as_os_str()]))?
        }
        "linux" => command::stdout(Command::new("ldd").arg(binary))?,
        os => bail!("dynamic packaging is unsupported on {os}; pass --static"),
    };
    let dependencies = output
        .lines()
        .filter_map(|line| {
            let name = line.split_whitespace().next()?;
            if env::consts::OS == "macos" {
                name.strip_prefix("@rpath/").filter(|name| {
                    Path::new(name)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("dylib"))
                })
            } else {
                name.starts_with("lib")
                    .then_some(name)
                    .filter(|name| name.contains(".so"))
            }
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if dependencies.is_empty() {
        bail!(
            "expected dynamic Rust libraries referenced by {}",
            binary.display()
        );
    }
    Ok(dependencies)
}

fn copy_dynamic_libraries(binary: &Path, destination: &Path, layout: &Layout) -> Result<()> {
    let target_libdir = command::stdout(Command::new("rustc").args(["--print", "target-libdir"]))?;
    let source_dirs = [
        layout.target_root.join(layout.profile).join("deps"),
        PathBuf::from(target_libdir.trim()),
    ];
    fs::create_dir_all(destination)?;
    for name in dynamic_dependencies(binary)? {
        let source = source_dirs
            .iter()
            .map(|directory| directory.join(&name))
            .find(|path| path.is_file())
            .with_context(|| {
                format!(
                    "could not find dynamic dependency {name} for {}",
                    binary.display()
                )
            })?;
        filesystem::copy_file(&source, &destination.join(name))?;
    }
    Ok(())
}

fn copy_bundled_daemons(layout: &Layout, destination: &Path) -> Result<()> {
    if !layout.all_daemons {
        return Ok(());
    }
    for target in TARGETS {
        let artifact = target.artifact_name();
        filesystem::copy_executable(
            &layout.daemon_output_dir.join(&artifact),
            &destination.join(artifact),
        )?;
    }
    Ok(())
}

fn package_macos(layout: &Layout, host_daemon: &Path) -> Result<()> {
    let bundle = layout.dist_dir.join(format!("{}.app", layout.app_name));
    filesystem::recreate_dir(&bundle)?;
    let contents = bundle.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(&resources)?;
    let binary = macos.join(BINARY);
    filesystem::copy_executable(
        &layout.target_root.join(layout.profile).join(BINARY),
        &binary,
    )?;
    filesystem::copy_executable(host_daemon, &macos.join(DAEMON))?;
    copy_bundled_daemons(layout, &resources.join("daemons"))?;
    if layout.linkage == Linkage::Dynamic {
        copy_dynamic_libraries(&binary, &contents.join("Frameworks"), layout)?;
    }
    compile_macos_icon(&contents, &resources)?;
    fs::write(
        contents.join("Info.plist"),
        info_plist(layout, &super::workspace_version()?),
    )?;
    sign_macos_bundle(layout, &bundle, &contents, &macos)?;
    let archive_name = format!("{}-macos-{}.app.zip", layout.app_name, macos_archive_arch());
    let archive = layout.dist_dir.join(&archive_name);
    remove_file_if_exists(&archive)?;
    command::run(
        Command::new("zip")
            .current_dir(&layout.dist_dir)
            .args(["-q", "-r", "-y"])
            .arg(archive_name)
            .arg(format!("{}.app", layout.app_name)),
    )
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn macos_archive_arch() -> &'static str {
    match env::consts::ARCH {
        "aarch64" => "arm64",
        arch => arch,
    }
}

fn compile_macos_icon(contents: &Path, resources: &Path) -> Result<()> {
    let contents = fs::canonicalize(contents)
        .with_context(|| format!("failed to resolve {}", contents.display()))?;
    let resources = fs::canonicalize(resources)
        .with_context(|| format!("failed to resolve {}", resources.display()))?;
    let icon = fs::canonicalize("crates/bootty/assets/bootty.icon")
        .context("failed to resolve the macOS app icon")?;
    let actool = command::stdout(Command::new("xcrun").args(["--find", "actool"]))
        .context("Xcode actool is required to package the macOS app icon")?;
    let actool = actool.trim();
    if actool_major_version(actool)? < 26 {
        eprintln!(
            "actool does not support Liquid Glass icon compilation; using legacy macOS .icns fallback"
        );
        return filesystem::copy_file(
            Path::new("crates/bootty-app/assets/bootty-icon-macos-fallback.icns"),
            &resources.join("bootty.icns"),
        );
    }
    let partial = contents.join("assetcatalog-info.plist");
    let output = Command::new(actool)
        .arg(icon)
        .args(["--compile", resources.to_string_lossy().as_ref()])
        .args(["--app-icon", "bootty", "--enable-on-demand-resources", "NO"])
        .args(["--development-region", "en", "--target-device", "mac"])
        .args(["--platform", "macosx", "--include-all-app-icons"])
        .args(["--minimum-deployment-target", "13.0"])
        .args([
            "--output-partial-info-plist",
            partial.to_string_lossy().as_ref(),
        ])
        .arg("--enable-icon-stack-fallback-generation=enabled")
        .output()
        .context("failed to run actool")?;
    let _ = fs::remove_file(partial);
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "actool failed compiling the app icon:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

fn actool_major_version(actool: &str) -> Result<u64> {
    let version = command::stdout(Command::new(actool).arg("--version"))?;
    let short_version = version
        .split_once("<key>short-bundle-version</key>")
        .and_then(|(_, suffix)| suffix.split_once("<string>"))
        .and_then(|(_, suffix)| suffix.split_once("</string>"))
        .map(|(version, _)| version.trim())
        .context("actool --version did not report its short bundle version")?;
    short_version
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
        .context("actool reported an invalid short bundle version")
}

fn info_plist(layout: &Layout, version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>{}</string>
  <key>CFBundleExecutable</key><string>bootty</string>
  <key>CFBundleIconFile</key><string>bootty</string>
  <key>CFBundleIconName</key><string>bootty</string>
  <key>CFBundleIdentifier</key><string>{}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>{}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>CFBundleVersion</key><string>{version}</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"#,
        layout.app_name, layout.bundle_identifier, layout.app_name
    )
}

fn sign_macos_bundle(layout: &Layout, bundle: &Path, contents: &Path, macos: &Path) -> Result<()> {
    if !executable_on_path("codesign") {
        return Ok(());
    }
    let frameworks = contents.join("Frameworks");
    if frameworks.is_dir() {
        for path in crate::filesystem::files_recursive(&frameworks)? {
            if path.extension() == Some(OsStr::new("dylib")) {
                command::run(
                    Command::new("codesign")
                        .args(["--force", "--sign", "-"])
                        .arg(path),
                )?;
            }
        }
    }
    command::run(
        Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(macos.join(DAEMON)),
    )?;
    command::run(
        Command::new("codesign")
            .args(["--force", "--sign", "-", "--requirements"])
            .arg(format!(
                "=designated => identifier \"{}\"",
                layout.bundle_identifier
            ))
            .arg(bundle),
    )
}

fn executable_on_path(program: &str) -> bool {
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .any(|directory| directory.join(program).is_file())
}

fn package_linux(layout: &Layout, host_daemon: &Path) -> Result<()> {
    let name = format!("{}-linux-{}", layout.app_name, env::consts::ARCH);
    let root = layout.dist_dir.join(&name);
    filesystem::recreate_dir(&root)?;
    let bin = root.join("bin");
    let applications = root.join("share/applications");
    let png = root.join("share/icons/hicolor/256x256/apps/bootty.png");
    let svg = root.join("share/icons/hicolor/scalable/apps/bootty.svg");
    fs::create_dir_all(&bin)?;
    fs::create_dir_all(&applications)?;
    filesystem::copy_executable(
        &layout.target_root.join(layout.profile).join(BINARY),
        &bin.join(layout.cli_name),
    )?;
    filesystem::copy_executable(host_daemon, &bin.join(DAEMON))?;
    copy_bundled_daemons(layout, &root.join("share/bootty/daemons"))?;
    if layout.linkage == Linkage::Dynamic {
        copy_dynamic_libraries(&bin.join(layout.cli_name), &root.join("lib"), layout)?;
    }
    filesystem::copy_file(
        Path::new("crates/bootty-app/assets/bootty-mascot.png"),
        &png,
    )?;
    filesystem::copy_file(
        Path::new("crates/bootty-app/assets/bootty-mascot.svg"),
        &svg,
    )?;
    fs::write(
        applications.join(format!("{}.desktop", layout.bundle_identifier)),
        desktop_entry(layout),
    )?;
    let archive = layout.dist_dir.join(format!("{name}.tar.gz"));
    remove_file_if_exists(&archive)?;
    command::run(
        Command::new("tar")
            .args(["-C", layout.dist_dir.to_string_lossy().as_ref(), "-czf"])
            .arg(archive)
            .arg(name),
    )
}

fn desktop_entry(layout: &Layout) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={}\nComment=Native GPU-rendered terminal\nExec={}\nIcon=bootty\nTerminal=false\nCategories=System;TerminalEmulator;\n",
        layout.app_name, layout.cli_name
    )
}
