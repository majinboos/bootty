# Releases and updates

Bootty publishes static native bundles through GitHub Releases. Every release has Linux x86_64, macOS arm64/x86_64, and Windows x64 assets, plus a `SHA256SUMS` file.

## Updates

Installed Bootty releases check for a newer GitHub Release before opening the app. If an update is available, Bootty installs it and restarts into the new version. Development binaries under a Cargo `target` directory never self-update.

Run an update explicitly with:

```sh
bootty update
```

The updater verifies the release asset through GitHub's release checksum before replacing the installed binary. Automatic updates currently support Linux and macOS. Windows continues to use the published ZIP because its bundled runtime DLL prevents safe in-process replacement.

## Publish a release

Run one command from a clean, synced `main` branch:

```sh
mise run release
```

That publishes a minor release. Pass `-- patch` or `-- major` for the other two. The command creates the version-bump PR and enables auto-merge. Once CI merges it, GitHub tags that commit and dispatches the release workflow.

The tag job dispatches that workflow by name rather than letting the new tag speak for itself: GitHub starts no workflow from a push made with `GITHUB_TOKEN`, so a `push: tags` trigger alone leaves the tag sitting there unreleased.

The release workflow rejects a tag that does not match `Cargo.toml`. After packaging succeeds for every supported platform, it creates a GitHub Release, uploads target-named bundles for the updater, generates `SHA256SUMS`, and publishes generated release notes.

Release tags are immutable. Publish a corrected version instead of replacing an existing tag.
