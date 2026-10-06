# Source builds and packaging

Paku is a Pi-only fork of [Zeron](https://github.com/zeronsh/zeron). Packaging scripts are retained for maintainers; they do not imply prebuilt Paku releases or a hosted update service exist.

## Source build

Install the Rust toolchain in `rust-toolchain.toml` and an authenticated Pi CLI (0.85.1 or newer), then:

```sh
cargo run -p paku
cargo run -p paku -- headless
```

Linux (Debian/Ubuntu) native UI build dependencies:

```sh
sudo apt-get install libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libx11-dev libxcb1-dev libx11-xcb-dev libfontconfig1-dev libfreetype-dev \
  libasound2-dev libvulkan-dev pkg-config cmake libwebkit2gtk-4.1-dev libjson-glib-dev
```

The shared desktop/headless binary links system audio libraries; headless Linux still needs the ALSA runtime. macOS requires Xcode command-line tools and Metal. Windows requires the Rust MSVC toolchain, Visual Studio C++ build tools and Windows SDK (`fxc.exe` for shaders). See [Windows development](../docs/reference/windows-development.md).

## Linux packages

```sh
scripts/package-linux.sh
PROFILE=debug scripts/package-linux.sh
```

Produces `target/package/paku-<version>-linux-<arch>.tar.gz` with binary, desktop entry, icon and local `install.sh`. The local installer uses `~/.paku/app/<version>` with a `current` symlink and launcher paths under the XDG data directory. `scripts/test-linux-desktop-entry.sh` checks launcher quoting/idempotence offline.

The optional edge installer requires an explicit `PAKU_BASE_URL` operator-owned release origin. No public install endpoint is claimed. See [self-hosting](../docs/self-hosting.md).

## macOS packages

```sh
scripts/package-macos.sh
```

Assembles `Paku.app`, ad-hoc signs it by default, and creates a DMG plus app tarball. Run on macOS; this is not a Linux cross-build. Set `CODESIGN_IDENTITY` for your own Developer ID. The release workflow can import your certificate and notarize using your App Store Connect secrets. Do not reuse upstream signing identities. Inherited DMG layout/artwork is packaging infrastructure, not proof of a published Paku download.

## Windows packages

```powershell
./scripts/package-windows.ps1 -ReleasesUrl https://github.com/aarsh21/paku/releases/latest/download
```

Inno Setup 6 builds the per-user installer; the script also creates portable ZIP and updater EXE artifacts. The release-feed parameter is a maintainer configuration and may not yet resolve to published artifacts. Installation identity is distinct from upstream: `{d98c3134-ef43-4bdb-94b8-1d892301a381}`. Keep installer, updater and installer test registry keys consistent across future Paku upgrades.

## Publishing

`.github/workflows/release.yml` packages on version tags and publishes artifacts plus checksums into the current GitHub repository. Manual dispatch builds artifacts without publishing a release. No upstream cloud bucket or release host is used. Always validate packages on their native platforms before publishing. The workspace's release profile uses thin LTO and stripped symbols.
