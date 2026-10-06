# Paku's repository-owned GPUI core

## Provenance / licenses

This directory is copied from `crates/gpui` in
https://github.com/zeronsh/zui at commit
`0966d065b23e0b7c9b53c2e186705b8bce16fe2d` (GPUI 0.2.2).
The upstream Cargo.toml, examples, resources, documentation and core source
are retained; no platform/renderer sibling crate or agent driver is vendored.
`LICENSE-APACHE` and `LICENSE-GPL` are preserved from that revision. GPUI's
package declares Apache-2.0; the upstream repository also supplies the GPL
license. Paku additions in this directory are provided under Apache-2.0.

`../Cargo.toml` contains the core's inherited dependency/package/lint definitions
from the same upstream root workspace. Dependency normalization converts
unvendored sibling paths to that exact git URL and revision, preserving versions,
features and default-feature settings. This prevents duplicate Rust package
identities for shared strings, collections, macros, etc.
Paku's root patches the original zui source to this core, so gpui_platform,
gpui_tokio, gpui-base and Paku itself all use one GPUI. The vendor workspace is
excluded from Paku's workspace membership. Its matching source patch also
supports standalone core tests without introducing a second git GPUI.

## API / coordinates

- `Window::set_ui_zoom(zoom: f32, cx: &mut App)`; ordinary-window default
  **1.0**, finite values clamped to **0.5..=3.0**, nonfinite values ignored.
  Reset with 1.0. Anchored popups inherit their parent's zoom at creation.
- `Window::ui_zoom() -> f32` is independent of fonts/rem and native DPI.
- `Window::native_scale_factor() -> f32` is OS DPI only.
- `Window::scale_factor()` is **native DPI × UI zoom**.
- `Window::viewport_size()` is **native content size / UI zoom**. The native
  window size and renderer drawable are not resized when zoom changes.
- UI layout and internal/synthetic events stay in UI coordinates. Existing
  scale-factor paths scale fixed-px primitives, text rasterization, images,
  shadows, masks, layout rounding, and accessibility into device coordinates.
  This is not a root scene transform or rem-size approximation.
- Only `PlatformWindow::on_input` converts native positions and pixel scroll
  deltas by 1/zoom. Mouse pressure/exit, touch, file drop and pinch centers are
  covered. Line deltas, pinch magnitude, force, modifiers and keys are unchanged.
  Mouse readback during bounds refresh uses the same conversion.
- PlatformInputHandler scales outgoing caret/range/element rectangles by zoom
  and inversely transforms character lookup points. UTF-16 ranges are unchanged.
  Candidate calculation stays in UI space (including its line-change tolerance)
  and uses one context update, including Wayland's direct candidate queries.
  Native range/candidate/element bounds and character lookup reflow pending zoom
  before querying, unless a draw is already in progress. After reflow, a detached
  PlatformInputHandler acquires the newly painted handler: refreshing layout
  alone leaves ElementInputHandler's paint-time bounds stale. No recursive
  Window update is used. `invalidate_character_coordinates` continues to push
  the already-native rectangle once after refreshing the active handler.
- Window-local native setters (resize, input regions, client inset, traffic-light
  position, native window menu, positive exclusive-zone distance) multiply by
  zoom. Retained regions/insets are reapplied when zoom changes; layer-shell's
  negative sentinel is unchanged. Global placement, monitor bounds and raw
  window bounds remain in native desktop coordinates.
- `WindowKind::AnchoredPopup` converts parent-local anchor/offset and requested
  popup size/minimum size by the parent's zoom at creation; the new popup stores
  that zoom independently. A weak per-App snapshot index permits opening a menu
  from a borrowed parent action handler without recursively borrowing its Window.
  The platform owns final global placement. Existing popups have no reposition
  API in this pinned platform contract; recreate them if the parent zoom changes.
- Zoom forces a full refresh (no cached view prepaint/paint reuse), notifies
  window-bounds observers after the caller's entity borrow unwinds and invalidates
  the IME anchor. Input arriving before the next display tick rebuilds layout/
  hitboxes before dispatch. A zoom set during render schedules a consistent frame.

Apps embedding native views using raw handles must multiply GPUI
window-local bounds/masks by `ui_zoom()` before OS calls (or use the effective
`scale_factor()` for device-pixel children); do not use effective scale as native
DPI for global monitor placement. In particular, Paku's macOS browser canvas
passes bounds directly to native sync and needs this conversion in UI integration.

## Tests

`src/window/ui_zoom_tests.rs` has display-free GPUI tests for validation/default/
reset, independent windows, effective-scale device identity, fixed-px scene
geometry, layout viewport, resize/native DPI, observers (including entity-update
reentrancy), anchored popups created from borrowed parents, native vs synthetic
input, pixel vs line scroll, pinch/touch/file positions, outbound geometry, IME
pull/push, character lookup and unchanged UTF-16 ranges.

No builds or cargo tests were run by the implementation sub-agent, per parent
request. Offline Cargo metadata verified one local GPUI package and no vendor
workspace members in Paku. Parent owns compilation, test execution and native
GUI evidence; tests do not need the human's display.

The parent also added immediate fixed-px native hit-target coverage. The IME
reflow regression now queries within the setter's Window update, before effects
can auto-draw. Separate native pull tests hold an outer App update across setter
and query, exercising each geometry entry point first with a detached handler
whose bounds are a paint-time snapshot. They check reflow, handler replacement,
lookup point conversion and unchanged UTF-16 ranges; drawing-phase coverage
checks that queries leave pending zoom alone rather than recursively drawing.
These new regressions have not been executed by the implementation sub-agent.
Test-only SVG font includes point to `resources/test-fonts`,
copied byte-for-byte from that upstream revision with IBM Plex and Lilex font
licenses. No unrelated application assets or platform crates are needed.
Reproduce the zoom tests with:

```sh
cargo test --manifest-path vendor/Cargo.toml --locked -p gpui --features test-support --lib ui_zoom
```

`vendor/Cargo.lock` pins the standalone core test workspace; build artifacts in
`vendor/target` are ignored, not source/evidence inputs.
