#!/usr/bin/env bash
# Deterministic core/Pi fixture coverage in one nextest build/run.
# Live Pi tests remain ignored unless explicitly requested.
# Extra arguments are passed to nextest (e.g. --release).
set -euo pipefail
cd "$(dirname "$0")/../.."

tests=()
for f in crates/harness/tests/*.rs crates/preview/tests/*.rs; do
  tests+=(--test "$(basename "$f" .rs)")
done
for t in session_publication restart_resume local_profiles message_queue pi_resume attachments_roundtrip e2e; do
  tests+=(--test "$t")
done

exec cargo nextest run --config-file scripts/ci/nextest.toml --locked --no-fail-fast \
  -p paku-harness -p paku-engine -p paku-sync -p paku-update -p paku-doc -p paku-preview \
  --features paku-harness/native-fixture \
  --lib --bins "${tests[@]}" "$@"
