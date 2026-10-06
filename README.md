# Paku

A local-first native desktop and headless interface for [Pi](https://github.com/badlogic/pi-mono). Paku is a Pi-only fork of [Zeron](https://github.com/zeronsh/zeron); credit for the original application, architecture, and implementation belongs to the Zeron contributors. Paku is independent and is not an official Zeron or Pi product.

*English | [简体中文](README.zh-CN.md) | [한국어](README.ko.md) | [日本語](README.ja.md)*

## Build from source

Install Rust using the version in `rust-toolchain.toml`, platform build dependencies, and an authenticated Pi CLI (0.85.1 or newer). See [packaging/build notes](dist/README.md) and [Pi integration](docs/pi.md).

```sh
git clone https://github.com/aarsh21/paku.git
cd paku
cargo run -p paku
```

For a machine without a display:

```sh
cargo run -p paku -- headless
```

Paku uses `pi --mode rpc`; `PI_EXECUTABLE` can select a specific executable. Model providers and credentials are configured in Pi, not separate Paku harnesses.

## Local by default

No Paku account or hosted service is required for local sessions. Transcripts and attachments stay in your local profile; Pi still connects to whichever model provider you configure. The native UI includes conversations, streaming tool activity, queues/steering, workspace files, terminals, diffs, and themes.

Optional edge/sync infrastructure is retained for self-hosting experiments. **This fork provides no hosted Paku service.** You must configure your own Cloudflare resources and authentication; the checked-in templates contain no upstream deployment targets. See [self-hosting](docs/self-hosting.md). Devices sharing an authenticated account are trusted peers with workspace read/write access. Local sessions are not automatically uploaded by signing in.

## Releases and validation

Build from source for now. [Paku Releases](https://github.com/aarsh21/paku/releases) is the place to check for future published artifacts; this README does not promise existing binaries, automatic updates, or a public install endpoint. Packaging and release workflows are retained for maintainers.

```sh
npm --prefix edge ci
scripts/verify-paku.sh /tmp/paku-evidence
```

The verification script runs the complete workspace test suite, compiles all features/targets, checks Swift binding generation, and exercises genuine installed Pi lifecycle/session resume, native Linux keyboard-to-reply, and two-device Mock-based sync. It saves commands, exit codes, raw logs, screenshots/video, source hashes, a reconstructable source patch/archive, and an artifact checksum manifest. Linux UI verification needs Xvfb, xdotool, FFmpeg, and ImageMagick; `agent-browser` adds desktop/mobile landing-page checks. Platform build dependencies must also be installed.

Pi verification uses isolated settings and a local model fixture, **not a paid provider API**. Mock-based sync proves the transport, not model quality. Paid-model/private-snapshot tests are deliberately not enabled wholesale. See [verification scope and evidence](docs/verification.md) and [Pi validation](docs/pi.md). macOS, Windows, and physical iOS runtime validation require their respective platforms.

[Architecture](ARCHITECTURE.md) · [MIT License](LICENSE) · [Third-party notices](THIRD_PARTY_NOTICES.md)
