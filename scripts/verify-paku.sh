#!/usr/bin/env bash
# Reproducible verification with raw logs and exit codes; no paid model requests.
# The native Pi tests launch the real CLI with an isolated local model fixture.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
output="${1:-/tmp/paku-evidence-$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$output"
output="$(realpath "$output")"
failed=0
printf 'suite\texit_code\tlog\n' > "$output/results.tsv"
run() {
  local name="$1"; shift
  printf '\n== %s ==\n' "$name"
  printf '%q ' "$@" > "$output/$name.command"
  printf '\n' >> "$output/$name.command"
  "$@" 2>&1 | tee "$output/$name.log"
  local code=${PIPESTATUS[0]}
  printf '%s\t%s\t%s.log\n' "$name" "$code" "$name" >> "$output/results.tsv"
  if [[ "$code" != 0 ]]; then failed=1; fi
  return "$code"
}
{
  date -u +%FT%TZ
  uname -a
  git rev-parse HEAD
  cargo --version
  rustc --version
  node --version
  pi --version
} > "$output/environment.txt" 2>&1
run surface python3 scripts/test-paku-surface.py
run formatting cargo fmt --all -- --check
run gpui-formatting cargo fmt --manifest-path vendor/Cargo.toml --all -- --check
run gpui-zoom cargo test --manifest-path vendor/Cargo.toml --locked -p gpui --features test-support --lib ui_zoom
run whitespace git diff --check
if run rust env RUST_TEST_THREADS=4 cargo test --locked --workspace --all-targets --no-fail-fast --features paku-harness/native-fixture; then
  run rust-feature-build cargo test --locked --workspace --all-targets --all-features --no-run
  run build cargo build --locked -p paku
  run binary-sha256 sha256sum target/debug/paku
  run pi-live cargo test --locked -p paku-harness --test pi_live -- --ignored --nocapture --test-threads=1
  run pi-engine env PAKU_TEST_APP_BIN="$(realpath target/debug/paku)" PAKU_EVIDENCE_DIR="$output" cargo test --locked -p paku-engine --test pi_ipc_live -- --ignored --nocapture --test-threads=1
  run mobile-bindgen cargo build --locked -p paku-mobile --lib --bin uniffi-bindgen --features bindgen
  case "$(uname -s)" in
    Darwin) mobile_library=target/debug/libpaku_mobile.dylib ;;
    MINGW*|MSYS*|CYGWIN*) mobile_library=target/debug/paku_mobile.dll ;;
    *) mobile_library=target/debug/libpaku_mobile.so ;;
  esac
  run swift-generate target/debug/uniffi-bindgen generate --library "$mobile_library" --language swift --out-dir "$output/swift-bindings"
  run swift-match cmp apps/ios/Paku/Core/Generated/paku_core.swift "$output/swift-bindings/paku_core.swift"
  run cli-version target/debug/paku --version
  run cli-help target/debug/paku --help
  if [[ "$(uname -s)" == Linux ]]; then
    run native-ui node scripts/test-paku-native-ui.mjs "$output/native-ui"
  fi
  run sync-e2e env PAKU_E2E_KEEP_LOGS=1 PAKU_E2E_LOG_ROOT="$output" bash scripts/e2e-smoke.sh
fi
run landing node --test apps/landing/*.test.mjs apps/www-redirect/*.test.mjs
if command -v "${AGENT_BROWSER_BIN:-agent-browser}" >/dev/null; then
  run landing-browser bash scripts/test-paku-landing.sh "$output/landing-browser"
else
  printf 'landing-browser\tSKIPPED\tagent-browser not installed\n' >> "$output/results.tsv"
fi
run edge-typecheck npm --prefix edge run typecheck
run edge-unit npm --prefix edge run test:unit
run edge-workerd npm --prefix edge run test:workerd
run linux-desktop bash scripts/test-linux-desktop-entry.sh
# Bind manifests, docs, tracked changes and untracked source to the evidence.
python3 scripts/collect-paku-evidence.py "$output" || failed=1
printf '\nEvidence: %s\n' "$output"
exit "$failed"
