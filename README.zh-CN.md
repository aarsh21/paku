# Paku

Paku 是面向 [Pi](https://github.com/badlogic/pi-mono) 的本地优先原生桌面及无界面应用，也是 [Zeron](https://github.com/zeronsh/zeron) 的 **Pi-only 分支**。原应用、架构与实现归功于 Zeron 贡献者。Paku 是独立项目，并非 Zeron 或 Pi 官方产品。

[English](README.md) | 简体中文 | [한국어](README.ko.md) | [日本語](README.ja.md)

## 从源码运行

请安装 `rust-toolchain.toml` 指定的 Rust 工具链、[平台构建依赖](dist/README.md)，以及已完成认证的 Pi CLI（0.85.1 或更新）。

```sh
git clone https://github.com/aarsh21/paku.git
cd paku
cargo run -p paku
# 无界面模式
cargo run -p paku -- headless
```

Paku 直接运行 `pi --mode rpc`。可用 `PI_EXECUTABLE` 指定可执行文件；模型与凭据由 Pi 管理。详见 [Pi 集成](docs/pi.md)。

本地会话无需 Paku 账户；记录与附件保存在本机。Pi 仍会连接您配置的模型提供商。此分支不提供托管的 Paku 服务。可选同步需要您自行配置 Cloudflare 资源与认证，参见[自托管说明](docs/self-hosting.md)。同一账户的设备是拥有工作区读写权限的可信节点。

目前请从源码构建。[Paku Releases](https://github.com/aarsh21/paku/releases) 仅供查询将来发布的构建，不保证已有下载、自动更新或公共安装服务。验证命令见 [English README](README.md)。

[架构](ARCHITECTURE.md) · [MIT 许可证](LICENSE) · [第三方声明](THIRD_PARTY_NOTICES.md)
