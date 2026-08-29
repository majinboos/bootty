# Releases and updates

Bootty publishes static native bundles through GitHub Releases. Every release has Linux x86_64, macOS arm64/x86_64, and Windows x64 app assets. It also has small headless daemon binaries for those targets and Linux arm64, plus a `SHA256SUMS` file.

## Updates

Installed Bootty releases check for a newer GitHub Release before opening the app. If an update is available, Bootty installs it and restarts into the new version. Development binaries under a Cargo `target` directory never self-update.

Run an update explicitly with:

```sh
bootty update
```

The updater verifies the release asset through GitHub's release checksum before replacing the installed binary. Automatic updates currently support Linux and macOS. Windows continues to use the published ZIP because its bundled runtime DLL prevents safe in-process replacement.

Remote Spaces first use a matching target daemon bundled with the installed
app. Local macOS installs cross-build all supported targets so development
does not depend on a published release. When no bundled target exists, Bootty
downloads the release daemon, verifies it against `SHA256SUMS`, and installs it
under a versioned `.bootty/bin/bootty-daemon-<protocol>-<version>.exe` path in
the remote user's home directory. The `.exe` suffix is intentional on every
platform. It gives Bootty one shell-neutral remote path while Unix still
executes the file normally.

On Unix, Bootty uploads a unique candidate beside the installed path. It makes
the candidate executable and verifies its exact protocol and package version.
It then atomically replaces the installed path and verifies that path again.
A failure before replacement leaves the prior installed daemon unchanged.
Windows keeps first-writer publication for its versioned executable path.

## Publish a release

When Luan asks you to make a release, perform the whole preparation locally on
`main`:

1. Fetch `origin/main` and verify that local `main` is synced and clean.
2. Find the latest reachable Bootty version tag with
   `git describe --tags --match 'v[0-9]*' --abbrev=0 HEAD`.
3. Inspect every commit in `<previous-tag>..HEAD`, including each commit's body
   and relevant diff. Commits are the source of the release notes. Do not use
   pull requests or GitHub-generated notes.
4. Determine the target version from the requested major, minor, or patch bump;
   an unspecified release is minor.
5. Write the release notes yourself in a temporary local file outside the
   repository. Use exactly these sections, in order: `## Features`,
   `## Fixes`, and `## Breaking Changes`. Group related commits into clear
   user-facing bullets. Omit internal-only changes. A breaking change appears
   only under `Breaking Changes`, even when its commit type is `feat` or `fix`.
   Write `- None.` when a section is empty.
6. Pass that file to the release command:

```sh
mise run release -- <release-notes-file>
```

That prepares a minor release. Pass `--bump patch` or `--bump major` after the
notes file for the other two, for example
`mise run release -- <release-notes-file> --bump patch`.
The command validates the notes, runs the full local release gate, updates
`Cargo.toml` and `Cargo.lock`, writes the notes into the body of
`chore(release): prepare v<version>` directly on `main`, and pushes it. The
local notes file is only an input; it is not committed. Do not create a release
branch or pull request. The preparation is complete only when the commit is on
`origin/main`.

The tag job dispatches that workflow by name rather than letting the new tag speak for itself: GitHub starts no workflow from a push made with `GITHUB_TOKEN`, so a `push: tags` trigger alone leaves the tag sitting there unreleased.

The release workflow rejects a tag that does not match `Cargo.toml`. After
packaging succeeds for every supported platform, it creates a GitHub Release,
uploads target-named app and daemon assets, generates `SHA256SUMS`, and
publishes the tagged prepare commit's message body as its release notes.

Release tags are immutable. Publish a corrected version instead of replacing an existing tag.
