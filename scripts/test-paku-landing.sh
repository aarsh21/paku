#!/usr/bin/env bash
# Browser-level checks of the local landing page, in a private Chromium session.
set -euo pipefail
cd "$(dirname "$0")/.."
output="$(realpath -m "${1:-/tmp/paku-landing-proof}")"
mkdir -p "$output"
browser="${AGENT_BROWSER_BIN:-agent-browser}"
session="paku-landing-$$"
port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"
python3 -m http.server "$port" --bind 127.0.0.1 --directory apps/landing/public > "$output/server.log" 2>&1 &
server=$!
cleanup() { "$browser" --session "$session" close >/dev/null 2>&1 || true; kill "$server" 2>/dev/null || true; }
trap cleanup EXIT
url="http://127.0.0.1:$port/"
for _ in {1..40}; do
  if curl --silent --fail "$url" >/dev/null; then break; fi
  sleep 0.1
done
"$browser" --session "$session" set viewport 1280 900
"$browser" --session "$session" open "$url"
"$browser" --session "$session" snapshot -i -u > "$output/desktop-snapshot.txt"
"$browser" --session "$session" screenshot --full "$output/desktop.png"
"$browser" --session "$session" set viewport 390 844
"$browser" --session "$session" snapshot -i -u > "$output/mobile-snapshot.txt"
"$browser" --session "$session" eval --stdin > "$output/browser-assertions.json" <<'JS'
(() => {
  const hero = document.querySelector('h1').innerText.replace(/\s+/g, ' ').trim();
  const credit = [...document.querySelectorAll('a')].find(a => a.href === 'https://github.com/zeronsh/zeron');
  if (!document.title.startsWith('Paku') || !hero.includes('home for Pi') || !credit) throw Error('Missing product or attribution');
  if (document.documentElement.scrollWidth > innerWidth) throw Error('Mobile horizontal overflow');
  if ([...document.links].some(a => new URL(a.href).hostname === 'zeron.sh')) throw Error('Upstream hosted service link');
  return {title: document.title, hero, viewport: innerWidth, documentWidth: document.documentElement.scrollWidth, credit: credit.href, pass: true};
})()
JS
"$browser" --session "$session" screenshot "$output/mobile.png"
"$browser" --session "$session" find role link click --name 'Build from source ↓'
"$browser" --session "$session" get url | tee "$output/build-url.txt" | grep -q '#build'
"$browser" --session "$session" snapshot -i -u > "$output/build-snapshot.txt"
"$browser" --session "$session" screenshot "$output/build-mobile.png"
echo "PASS: desktop/mobile browser branding, attribution, no overflow, source-build navigation"
