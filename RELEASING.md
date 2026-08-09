# Releasing

Releases publish prebuilt binaries for Intel and ARM macOS and Linux. Linux
artifacts use MUSL so one binary per architecture works across common
distributions.

## Prepare

1. Update the version in `Cargo.toml` and `herdr-plugin.toml`, then run a Cargo
   command and commit the resulting `Cargo.lock` update.
2. Run the complete local verification suite:

   ```bash
   cargo fmt --check
   cargo test --all-targets --locked
   cargo clippy --all-targets --all-features --locked -- -D warnings
   RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --locked
   tests/test-hooks.sh
   scripts/check-version.sh
   scripts/test-install.sh
   ```

3. Open and review the release pull request. Do not merge a manifest that
   references a release that does not exist yet.

## Publish Assets

From the reviewed release commit, create and push an annotated version tag:

```bash
version="$(scripts/check-version.sh)"
git tag -a "v$version" -m "v$version"
git push origin "v$version"
```

The release workflow validates the tag, creates a draft release, builds all four
targets, uploads their checksums, and publishes only after every build succeeds.
If any target fails, the release remains a draft and the pull request must not be
merged.

## Verify And Merge

1. Confirm the GitHub Release is published and contains four binaries plus
   `SHA256SUMS`.
2. Install the tag into a clean Herdr plugin registry:

   ```bash
   herdr plugin install Angel-O/herdr-labels --ref "v$version"
   ```

3. Verify startup reconciliation, automatic process naming, manual-name opt-out,
   reset/clear actions, and tab creation, deletion, reordering, and renaming.
4. Merge the release pull request only after installation succeeds.
