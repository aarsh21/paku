# Paku

Paku는 [Pi](https://github.com/badlogic/pi-mono)를 위한 로컬 우선 네이티브 데스크톱 및 헤드리스 앱입니다. [Zeron](https://github.com/zeronsh/zeron)의 **Pi 전용 포크**이며, 원본 앱·아키텍처·구현의 공로는 Zeron 기여자에게 있습니다. Zeron이나 Pi의 공식 제품이 아닌 독립 프로젝트입니다.

[English](README.md) | [简体中文](README.zh-CN.md) | 한국어 | [日本語](README.ja.md)

## 소스에서 실행

`rust-toolchain.toml`의 Rust 도구 모음, [플랫폼 빌드 의존성](dist/README.md), 인증된 Pi CLI(0.85.1 이상)가 필요합니다.

```sh
git clone https://github.com/aarsh21/paku.git
cd paku
cargo run -p paku
# 헤드리스 모드
cargo run -p paku -- headless
```

Paku는 `pi --mode rpc`를 직접 실행합니다. `PI_EXECUTABLE`로 실행 파일을 선택하며, 모델과 자격 증명은 Pi에서 관리합니다. [Pi 통합](docs/pi.md)을 참고하세요.

로컬 세션에는 Paku 계정이 필요 없고 기록과 첨부 파일은 로컬에 저장됩니다. Pi는 설정한 모델 제공자에 연결합니다. 이 포크는 호스팅된 Paku 서비스를 제공하지 않습니다. 선택적 동기화에는 직접 구성한 Cloudflare 리소스와 인증이 필요합니다. [셀프 호스팅](docs/self-hosting.md)을 참고하세요. 같은 계정의 기기는 작업 공간 읽기·쓰기 권한을 가진 신뢰할 수 있는 피어입니다.

현재는 소스 빌드를 사용하세요. [Paku Releases](https://github.com/aarsh21/paku/releases)는 향후 게시된 빌드를 확인하는 곳이며, 기존 다운로드·자동 업데이트·공개 설치 서비스를 보장하지 않습니다. 검증 명령은 [English README](README.md)에 있습니다.

[아키텍처](ARCHITECTURE.md) · [MIT 라이선스](LICENSE) · [서드 파티 고지](THIRD_PARTY_NOTICES.md)
