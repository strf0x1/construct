# Releasing construct

A release is a tag. Pushing `vX.Y.Z` runs `.github/workflows/release.yml`,
which ends in a **draft** GitHub Release. Nothing is public until you publish
the draft, and the installer only ever sees published releases.

## What gets built

| | |
| --- | --- |
| Platform | Linux x86-64 (`x86_64-unknown-linux-gnu`), built on Ubuntu 22.04, so it needs glibc 2.35 or newer |
| Compiler | Pinned by `RUSTUP_TOOLCHAIN` in the workflow, with the committed `Cargo.lock` (`--locked`) |
| Archive | `construct-x86_64-unknown-linux-gnu.tar.gz`: the binary, `LICENSE`, `README.md`, `VERSION`, `THIRD-PARTY-LICENSES.txt` |
| Checksums | `<archive>.sha256` per archive, and `SHA256SUMS` for all of them |

Archive names carry no version, so
`https://github.com/strf0x1/construct/releases/latest/download/construct-x86_64-unknown-linux-gnu.tar.gz`
always points at the newest published build, and
`.../releases/download/vX.Y.Z/...` at a specific one.

macOS and Windows are not offered: the tool sandbox is Landlock, which is
Linux-only. Linux ARM64 comes once there's hardware to test it on.

## Cutting a release

1. Set `version` under `[workspace.package]` in `Cargo.toml`, run
   `cargo check` so `Cargo.lock` follows, and merge that to `main`. The
   workflow refuses a tag that doesn't match this version.
2. Tag the merge commit and push the tag:

   ```sh
   git tag -a v0.1.0 -m "construct v0.1.0"
   git push origin v0.1.0
   ```

   A tag with a suffix (`v0.2.0-rc.1`) becomes a pre-release, which the
   installer's "latest" skips. The crate version must carry the same suffix.
3. Watch the run: `check` (the full `cargo xtask check`), `build`, `smoke`
   (the archive unpacked and run on clean Ubuntu, Debian, Fedora and Arch
   images), then `release`, which uploads everything to a draft.
4. Try the draft before anyone else can. Download the archive from the draft
   page, check it, and run it on a machine that isn't your dev box:

   ```sh
   sha256sum -c construct-x86_64-unknown-linux-gnu.tar.gz.sha256
   tar -xzf construct-x86_64-unknown-linux-gnu.tar.gz
   ./construct-x86_64-unknown-linux-gnu/construct --version
   ```

5. Edit the generated notes if they need it, then **Publish release**. From
   that moment `install.sh` installs it.

A broken release: unpublish it (back to draft) or delete it, and the
installer falls back to the previous one. Fix forward with a new patch
version; don't move a tag that people may already have installed.

## The installer

`site/install.sh` is served at `https://construct.sprawlcommons.com/install.sh`
and is the only copy. It resolves one version, downloads that version's
archive and `.sha256`, verifies both and checks that the binary runs before it
replaces anything, and installs to `~/.local/bin`. No sudo, no profile edits,
no telemetry. Options:

```sh
curl -fsSL https://construct.sprawlcommons.com/install.sh | sh -s -- --version v0.1.0   # pin or roll back
curl -fsSL https://construct.sprawlcommons.com/install.sh | sh -s -- --dir ~/bin
```

When you change it, run `shellcheck site/install.sh` and try it under `dash`
as well as `bash`: `curl | sh` is `dash` on Debian and Ubuntu.

## The site

`site/` deploys as a Cloudflare Workers static-assets project named
`construct` (`wrangler.jsonc`), attached to `construct.sprawlcommons.com`.
`site/_headers` serves `install.sh` as plain text so "read the installer"
shows it in the browser instead of downloading it.
