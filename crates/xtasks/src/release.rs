use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use sha2::{Digest as _, Sha256};
use tempfile::NamedTempFile;
use toml_edit::{DocumentMut, value};

use crate::command;

const NOTES_ERROR: &str = "release notes must contain Features, Fixes, and Breaking Changes, in order, with at least one bullet each";

#[derive(Clone, Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Prepare and push a release commit directly on main.
    Prepare(PrepareArgs),
    /// Validate release notes from a file, or stdin when the path is `-`.
    ValidateNotes(ValidateNotesArgs),
    /// Check that a release tag matches the workspace version.
    VerifyTag(VerifyTagArgs),
    /// Rename one packaged asset to its public release name.
    RenameAsset(RenameAssetArgs),
    /// Tag the workspace version and dispatch the release workflow.
    TagAndDispatch,
    /// Create a GitHub release from an asset directory.
    Publish(PublishArgs),
}

#[derive(Clone, Debug, ClapArgs)]
pub struct PrepareArgs {
    pub release_notes_file: PathBuf,

    #[arg(long, value_enum, default_value_t = Bump::Minor)]
    pub bump: Bump,
}

#[derive(Clone, Debug, ClapArgs)]
pub struct ValidateNotesArgs {
    #[arg(default_value = "-")]
    pub release_notes_file: PathBuf,
}

#[derive(Clone, Debug, ClapArgs)]
pub struct VerifyTagArgs {
    pub tag: String,
}

#[derive(Clone, Debug, ClapArgs)]
pub struct PublishArgs {
    pub tag: String,
    pub asset_dir: PathBuf,
}

#[derive(Clone, Debug, ClapArgs)]
pub struct RenameAssetArgs {
    pub source: PathBuf,
    pub destination: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Bump {
    Major,
    Minor,
    Patch,
}

pub fn run(args: Args) -> Result<()> {
    match args.command {
        Command::Prepare(args) => prepare(&args),
        Command::ValidateNotes(args) => {
            let notes = read_notes(&args.release_notes_file)?;
            validate_notes(&notes)?;
            print!("{notes}");
            if !notes.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        Command::VerifyTag(args) => verify_tag(&args.tag),
        Command::RenameAsset(args) => {
            fs::rename(&args.source, &args.destination).with_context(|| {
                format!(
                    "failed to rename {} to {}",
                    args.source.display(),
                    args.destination.display()
                )
            })
        }
        Command::TagAndDispatch => tag_and_dispatch(),
        Command::Publish(args) => publish(&args.tag, &args.asset_dir),
    }
}

fn prepare(args: &PrepareArgs) -> Result<()> {
    let notes = read_notes(&args.release_notes_file)?;
    validate_notes(&notes)?;

    let manifest = fs::read_to_string("Cargo.toml").context("failed to read Cargo.toml")?;
    let current = workspace_version(&manifest)?;
    let next = bumped_version(&current, args.bump)?;
    let tag = format!("v{next}");

    ensure!(
        git_stdout(["branch", "--show-current"])? == "main",
        "run releases from main"
    );
    command::run(ProcessCommand::new("git").args(["fetch", "origin", "main", "--quiet"]))?;
    ensure!(
        git_stdout(["rev-parse", "HEAD"])? == git_stdout(["rev-parse", "origin/main"])?,
        "main is not synced with origin/main"
    );
    ensure!(!remote_tag_exists(&tag)?, "{tag} already exists");
    ensure_clean("working tree is not clean")?;

    for task in ["fmt", "clippy", "test"] {
        command::run(ProcessCommand::new("mise").args(["run", task]))?;
    }
    command::run(ProcessCommand::new("mise").args(["run", "bench", "--", "--ci-smoke"]))?;
    ensure_clean("the release gate changed the working tree")?;

    let mut document = manifest
        .parse::<DocumentMut>()
        .context("Cargo.toml is not valid TOML")?;
    document["workspace"]["package"]["version"] = value(&next);
    fs::write("Cargo.toml", document.to_string()).context("failed to update Cargo.toml")?;
    command::run(
        ProcessCommand::new("cargo")
            .args(["metadata", "--format-version", "1"])
            .stdout(Stdio::null()),
    )?;

    command::run(ProcessCommand::new("git").args(["add", "Cargo.toml", "Cargo.lock"]))?;
    command::run(ProcessCommand::new("git").args([
        "commit",
        "--cleanup=whitespace",
        "-m",
        &format!("chore(release): prepare {tag}"),
        "-m",
        notes.trim_end(),
    ]))?;
    command::run(ProcessCommand::new("git").args(["push", "origin", "main"]))?;
    println!("{tag} will publish from the prepare commit pushed to main.");
    Ok(())
}

pub fn validate_notes(notes: &str) -> Result<()> {
    let expected = ["## Features", "## Fixes", "## Breaking Changes"];
    let mut section = 0_usize;
    let mut has_bullet = false;

    for line in notes.lines() {
        if line.starts_with("## ") {
            if section > 0 && !has_bullet {
                bail!(NOTES_ERROR);
            }
            ensure!(
                section < expected.len() && line == expected[section],
                NOTES_ERROR
            );
            section += 1;
            has_bullet = false;
        } else if section == 0 {
            ensure!(line.trim().is_empty(), NOTES_ERROR);
        } else if line.starts_with("- ") {
            has_bullet = true;
        }
    }

    ensure!(section == expected.len() && has_bullet, NOTES_ERROR);
    Ok(())
}

pub fn bumped_version(version: &str, bump: Bump) -> Result<String> {
    let mut components = version.split('.');
    let major = parse_component(components.next(), version)?;
    let minor = parse_component(components.next(), version)?;
    let patch = parse_component(components.next(), version)?;
    ensure!(
        components.next().is_none(),
        "unsupported workspace version: {version}"
    );

    let next = match bump {
        Bump::Major => (
            major.checked_add(1).context("major version overflow")?,
            0,
            0,
        ),
        Bump::Minor => (
            major,
            minor.checked_add(1).context("minor version overflow")?,
            0,
        ),
        Bump::Patch => (
            major,
            minor,
            patch.checked_add(1).context("patch version overflow")?,
        ),
    };
    Ok(format!("{}.{}.{}", next.0, next.1, next.2))
}

fn parse_component(component: Option<&str>, version: &str) -> Result<u64> {
    component
        .filter(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        })
        .and_then(|component| component.parse().ok())
        .with_context(|| format!("unsupported workspace version: {version}"))
}

fn read_notes(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut notes = String::new();
        io::stdin()
            .read_to_string(&mut notes)
            .context("failed to read release notes from stdin")?;
        Ok(notes)
    } else {
        fs::read_to_string(path)
            .with_context(|| format!("failed to read release notes from {}", path.display()))
    }
}

fn workspace_version(manifest: &str) -> Result<String> {
    let document = manifest
        .parse::<DocumentMut>()
        .context("Cargo.toml is not valid TOML")?;
    document["workspace"]["package"]["version"]
        .as_str()
        .context("Cargo.toml has no workspace.package.version")
        .map(ToOwned::to_owned)
}

fn ensure_clean(message: &str) -> Result<()> {
    ensure!(
        git_stdout(["status", "--porcelain"])?.is_empty(),
        "{message}"
    );
    Ok(())
}

fn git_stdout<const N: usize>(args: [&str; N]) -> Result<String> {
    Ok(command::stdout(ProcessCommand::new("git").args(args))?
        .trim()
        .to_owned())
}

fn remote_tag_exists(tag: &str) -> Result<bool> {
    let status = ProcessCommand::new("git")
        .args([
            "ls-remote",
            "--exit-code",
            "--tags",
            "origin",
            &format!("refs/tags/{tag}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to query remote tags")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(2) => Ok(false),
        _ => bail!("git ls-remote exited with {status}"),
    }
}

fn verify_tag(tag: &str) -> Result<()> {
    let manifest = fs::read_to_string("Cargo.toml").context("failed to read Cargo.toml")?;
    let expected = format!("v{}", workspace_version(&manifest)?);
    ensure!(
        tag == expected,
        "release tag {tag} does not match workspace version {expected}"
    );
    Ok(())
}

fn tag_and_dispatch() -> Result<()> {
    let manifest = fs::read_to_string("Cargo.toml").context("failed to read Cargo.toml")?;
    let tag = format!("v{}", workspace_version(&manifest)?);
    let exists = ProcessCommand::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/tags/{tag}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to inspect release tags")?
        .success();
    let release_ref = if exists { tag.as_str() } else { "HEAD" };
    let subject = git_stdout(["show", "--no-patch", "--format=%s", release_ref])?;
    ensure!(
        subject == format!("chore(release): prepare {tag}"),
        "{release_ref} is not the prepare commit for {tag}"
    );
    let notes = git_stdout(["show", "--no-patch", "--format=%b", release_ref])?;
    validate_notes(&notes)?;

    if exists {
        println!("{tag} already exists; checking release dispatch state");
    } else {
        command::run(ProcessCommand::new("git").args(["tag", &tag]))?;
    }
    if !remote_tag_exists(&tag)? {
        command::run(ProcessCommand::new("git").args(["push", "origin", &tag]))?;
    }
    if ProcessCommand::new("gh")
        .args(["release", "view", &tag])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to inspect GitHub releases")?
        .success()
    {
        println!("GitHub release {tag} already exists");
        return Ok(());
    }
    command::run(ProcessCommand::new("gh").args([
        "workflow",
        "run",
        "release.yml",
        "--ref",
        &tag,
        "-f",
        &format!("tag={tag}"),
    ]))
}

fn publish(tag: &str, asset_dir: &Path) -> Result<()> {
    verify_tag(tag)?;
    let notes = git_stdout(["show", "--no-patch", "--format=%b", tag])?;
    validate_notes(&notes)?;

    let mut assets = fs::read_dir(asset_dir)
        .with_context(|| format!("failed to read asset directory {}", asset_dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    assets
        .retain(|path| path.is_file() && path.file_name().is_some_and(|name| name != "SHA256SUMS"));
    assets.sort();
    ensure!(
        !assets.is_empty(),
        "asset directory {} is empty",
        asset_dir.display()
    );

    let mut checksums = String::new();
    for asset in &assets {
        let bytes = fs::read(asset)
            .with_context(|| format!("failed to read release asset {}", asset.display()))?;
        let digest = Sha256::digest(bytes);
        let name = asset
            .file_name()
            .context("release asset has no file name")?
            .to_string_lossy();
        for byte in digest {
            write!(&mut checksums, "{byte:02x}")?;
        }
        writeln!(&mut checksums, "  {name}")?;
    }
    let checksum_path = asset_dir.join("SHA256SUMS");
    fs::write(&checksum_path, checksums)
        .with_context(|| format!("failed to write {}", checksum_path.display()))?;
    assets.push(checksum_path);

    let notes_file = NamedTempFile::new().context("failed to create release notes file")?;
    fs::write(notes_file.path(), notes).context("failed to write release notes file")?;
    let mut gh = ProcessCommand::new("gh");
    gh.args(["release", "create", tag]);
    gh.args(&assets);
    if let Ok(repository) = std::env::var("GITHUB_REPOSITORY") {
        gh.args(["--repo", &repository]);
    }
    gh.arg("--notes-file")
        .arg(notes_file.path())
        .args(["--title", tag]);
    command::run(&mut gh)
}
