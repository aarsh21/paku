# Whole-interface scale

Open **Settings → Appearance** (**Ctrl+,** on Linux/Windows, **Cmd+,** on macOS), then choose **Interface scale**: **75%, 100%, 125%, 150%, 175%, or 200%**. The default is **100%**.

This is whole-window zoom, not a font-only setting. Icons, buttons, padding, panels, fixed-pixel controls, and text are enlarged together. The logical viewport becomes smaller when zooming in, so content reflows rather than being cropped from an unchanged layout. The native window size does not change.

## Keyboard controls

- **Ctrl+Plus**: increase by 25 percentage points.
- **Ctrl+Minus**: decrease by 25 percentage points.
- **Ctrl+0**: reset to 100%.
- On macOS, use **Cmd** instead of **Ctrl**.

The default Plus shortcut also accepts **Ctrl+=** (and shifted equals/plus variants), or **Cmd+=** on macOS. These aliases only apply while the Plus binding is at its default and do not take a chord assigned to another app action. Rebind the three actions in **Settings → Shortcuts → Appearance**, or find them by searching “interface scale” in the command palette. Zoom works while Appearance settings are open too.

Scale is device-local and saved automatically as `uiScale` in `ui-settings.json`, through the central settings writer. Changes apply to open windows; newly opened/reopened main windows restore the saved value. Finite hand-edited values are limited to 75–200%; invalid/non-finite values recover to 100%. Reset changes only scale, not fonts or panel preferences.

## Independent font preferences

**Interface font**, **Terminal font**, and **Code font** remain independent size/family controls. The interface font still defaults to **16 px**, with the existing **24, 28, and 32 px** options available. A 24 px interface font with 150% interface scale is visually larger than either setting alone; whole-interface zoom also enlarges terminal and code text without rewriting their stored font sizes. Choose 100% scale if you only want to adjust text sizes.

## Native Linux display scaling

Keep native Wayland enabled on Hyprland and other Wayland desktops. This feature does not remove `WAYLAND_DISPLAY`, require XWayland, use `GPUI_X11_SCALE_FACTOR`, or change the compositor's monitor scale. Native monitor DPI remains the platform/compositor setting; Paku's application zoom multiplies it for GPUI painting and layout only. `GDK_SCALE` and browser zoom do not control the main GPUI interface.

The implementation uses GPUI's `Window::set_ui_zoom`: effective paint scale is native DPI × interface zoom, while the logical viewport is native content size ÷ zoom. GPUI owns the corresponding pointer, scrolling, and IME coordinate conversions. This also scales Paku's fixed-pixel UI, unlike changing only `set_rem_size`. [Zed's source](https://github.com/zed-industries/zed) (UI font adjustment handlers in `crates/zed/src/zed.rs`, `crates/theme_settings/src/settings.rs`, and rem-sized icons in `crates/ui/src/components/icon.rs`) informed the investigation. Its rem-based approach is not represented here as a general whole-window zoom API: Paku needs whole-window scaling because much of its geometry uses fixed pixels. The repository-owned GPUI core patch and preserved upstream licenses/provenance are documented in `vendor/gpui/PAKU-ZOOM.md`.

## Verification status

The scoped Linux run in `/tmp/paku-full-scale-evidence` passed **12 GPUI coordinate/layout/input/IME tests** and **1,613 Paku UI tests**, and built the application. Private nested Wayland runs at **150% and 200%** exercised actual keyboard input through the desktop, engine and genuine installed Pi with an isolated local model fixture (no paid API). Ctrl+Plus, Minus, 0 and equals changed/persisted scale without altering font preferences. Screenshots measure an unchanged 1200×760 native window, sidebar widths **255 / 383 / 510 px** at 100 / 150 / 200%, and the fixed-px Plus icon **10 / 16 / 20 px**: this is not font-only enlargement.

Limits: the nested Weston/Vulkan setup has an **inherited passive-presentation issue**, also reproduced with the pre-zoom 100% baseline: `02-pi-reply.png` retains stale partial composer text even though Pi replies arrive. Zoom/full refresh presents the completed conversation in later screenshots. The initial reply screenshot must not be called successful visible rendering; this run does not certify general Wayland frame delivery or the user's Hyprland desktop. No monitor or human desktop was changed.

The optional Linux browser runtime attempt stopped because the isolated WebKit library tried to launch a missing system `WebKitNetworkProcess`; only its effective-scale protocol/unit checks passed. Native embedded browser runtime, live IME candidate windows, macOS chrome/children, Windows and platform-specific accessibility are not certified. Earlier full-workspace and font-only test totals apply to earlier builds, not this scoped zoom run.
