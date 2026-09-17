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

3. Open and review the release pull request, then merge it with a merge commit
   so the reviewed release commit remains in `main` history. Do not squash the
   release pull request.

## Publish Assets

Update local `main`, then create and push an annotated version tag from the
release pull request's merge commit:

```bash
set -euo pipefail
git switch main
git pull --ff-only origin main
merge_commit="$(gh pr view <release-pr-number> --json mergeCommit --jq '.mergeCommit.oid')"
test "$(git rev-parse HEAD)" = "$merge_commit"
version="$(scripts/check-version.sh)"
git tag -a "v$version" "$merge_commit" -m "v$version"
git push origin "v$version"
```

The release workflow validates the tag, creates a draft release, builds all four
targets, uploads their checksums, and publishes only after every build succeeds.
If any target fails, the release remains a draft.

## Verify

1. Confirm the GitHub Release is published and contains four binaries plus
   `SHA256SUMS`.
2. Install the tag into a clean Herdr plugin registry:

   ```bash
   herdr plugin install Angel-O/herdr-labels --ref "v$version"
   ```

3. Verify startup reconciliation, automatic process naming, manual-name opt-out,
   reset/clear actions, and tab creation, deletion, reordering, and renaming.
