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

Run one command from a clean, synced `main` branch:

```sh
mise run release
```

That publishes a minor release. Pass `-- patch` or `-- major` for the other two. The command creates the version-bump PR and enables auto-merge. Once CI merges it, GitHub tags that commit and dispatches the release workflow.

The tag job dispatches that workflow by name rather than letting the new tag speak for itself: GitHub starts no workflow from a push made with `GITHUB_TOKEN`, so a `push: tags` trigger alone leaves the tag sitting there unreleased.

The release workflow rejects a tag that does not match `Cargo.toml`. After packaging succeeds for every supported platform, it creates a GitHub Release, uploads target-named app and daemon assets, generates `SHA256SUMS`, and publishes generated release notes.

Release tags are immutable. Publish a corrected version instead of replacing an existing tag.
