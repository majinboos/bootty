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

1. Update the workspace version in `Cargo.toml`.
2. Create and push the matching `v<version>` tag:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

The release workflow rejects a tag that does not match `Cargo.toml`. After packaging succeeds for every supported platform, it creates a GitHub Release, uploads target-named bundles for the updater, generates `SHA256SUMS`, and publishes generated release notes.

The tag is the release boundary. Push a corrected version instead of replacing an existing release tag.
