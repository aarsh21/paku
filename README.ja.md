# Paku

Paku は [Pi](https://github.com/badlogic/pi-mono) 向けのローカルファーストなネイティブデスクトップ／ヘッドレスアプリです。[Zeron](https://github.com/zeronsh/zeron) の **Pi 専用フォーク**であり、元のアプリ・設計・実装の功績は Zeron の貢献者に帰属します。Zeron や Pi の公式製品ではない独立したプロジェクトです。

[English](README.md) | [简体中文](README.zh-CN.md) | [한국어](README.ko.md) | 日本語

## ソースから実行

`rust-toolchain.toml` で指定された Rust、[各プラットフォームのビルド依存関係](dist/README.md)、認証済みの Pi CLI（0.85.1 以降）が必要です。

```sh
git clone https://github.com/aarsh21/paku.git
cd paku
cargo run -p paku
# ヘッドレスモード
cargo run -p paku -- headless
```

Paku は `pi --mode rpc` を直接起動します。`PI_EXECUTABLE` で実行ファイルを選択でき、モデルと認証情報は Pi 側で管理します。[Pi 統合](docs/pi.md)を参照してください。

ローカルセッションには Paku アカウントは不要で、履歴と添付ファイルはローカルに保存されます。Pi は設定したモデルプロバイダーに接続します。このフォークはホストされた Paku サービスを提供しません。任意の同期機能には自分の Cloudflare リソースと認証設定が必要です。[セルフホスト](docs/self-hosting.md)を参照してください。同じアカウントのデバイスは、ワークスペースの読み書き権限を持つ信頼されたピアです。

当面はソースからビルドしてください。[Paku Releases](https://github.com/aarsh21/paku/releases) は今後公開される成果物の確認先であり、既存のダウンロード、自動更新、公開インストールサービスを保証しません。検証コマンドは [English README](README.md) にあります。

[アーキテクチャ](ARCHITECTURE.md) · [MIT ライセンス](LICENSE) · [サードパーティ通知](THIRD_PARTY_NOTICES.md)
