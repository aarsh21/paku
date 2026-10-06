//! Whole-window UI zoom, independent of interface, code and terminal typography.
//!
//! Unlike changing the rem size alone, GPUI zoom scales fixed-pixel icons,
//! padding and controls too, and reflows against a smaller logical viewport.

use gpui::{App, KeyBinding, Keystroke, Subscription, Window};
use serde::{Deserialize, Deserializer, Serializer};

use crate::settings::{self, SavePolicy};

pub const DEFAULT: f32 = 1.0;
pub const MIN: f32 = 0.75;
pub const MAX: f32 = 2.0;
pub const STEP: f32 = 0.25;
pub const PRESETS: [f32; 6] = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

/// Non-finite values recover to 100%; finite values clamp without changing
/// independently configured font sizes or snapping hand-edited percentages.
pub fn normalize(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(MIN, MAX)
    } else {
        DEFAULT
    }
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f32, D::Error> {
    // A malformed zoom preference must not discard unrelated UI settings.
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(normalize(
        value.as_f64().map(|n| n as f32).unwrap_or(DEFAULT),
    ))
}

pub fn serialize<S: Serializer>(scale: &f32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_f32(normalize(*scale))
}

pub fn current(cx: &App) -> f32 {
    settings::ui_scale(cx)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Increase,
    Decrease,
    Reset,
}

pub fn changed(scale: f32, change: Change) -> f32 {
    let scale = normalize(scale);
    normalize(match change {
        Change::Increase => scale + STEP,
        Change::Decrease => scale - STEP,
        Change::Reset => DEFAULT,
    })
}

/// Save only this preference through the central writer. Apply the active
/// window immediately; defer the all-window pass until that window is back
/// inside App (it is temporarily borrowed while handling a settings click).
pub fn set(scale: f32, window: &mut Window, cx: &mut App) -> bool {
    let scale = normalize(scale);
    let changed = settings::update(SavePolicy::Immediate, cx, |settings| {
        settings.ui_scale = scale;
    });
    if !changed && window.ui_zoom() == scale {
        return false;
    }
    window.set_ui_zoom(scale, cx);
    cx.defer(|cx| {
        let scale = current(cx);
        for handle in cx.windows() {
            if let Err(error) = handle.update(cx, |_, window, cx| {
                window.set_ui_zoom(scale, cx);
            }) {
                tracing::warn!(%error, "interface scale: window zoom not applied");
            }
        }
        cx.refresh_windows();
    });
    changed
}

pub fn change(change: Change, window: &mut Window, cx: &mut App) -> bool {
    set(changed(current(cx), change), window, cx)
}

/// Extra spellings for the default Plus chord on keyboard layouts that
/// report the physical equals key and/or retain Shift. Customized bindings
/// do not acquire hidden aliases, and aliases never steal another app chord.
pub const INCREASE_ALIASES: [&str; 3] = ["mod-=", "mod-shift-=", "mod-shift-+"];

pub fn increase_aliases(
    keymap: &settings::KeymapConfig,
) -> impl Iterator<Item = &'static str> + '_ {
    use settings::ShortcutId;
    INCREASE_ALIASES.into_iter().filter(move |alias| {
        keymap.increase_interface_scale == ShortcutId::IncreaseInterfaceScale.default_combo()
            && !ShortcutId::ALL.iter().any(|id| {
                *id != ShortcutId::IncreaseInterfaceScale
                    && settings::platform_combo(keymap.get(*id)) == settings::platform_combo(alias)
            })
    })
}

pub fn bind_keys(cx: &mut App, keymap: &settings::KeymapConfig) {
    use crate::shell::{DecreaseInterfaceScale, IncreaseInterfaceScale, ResetInterfaceScale};
    use settings::ShortcutId;

    let mut bindings = Vec::new();
    for id in [
        ShortcutId::IncreaseInterfaceScale,
        ShortcutId::DecreaseInterfaceScale,
        ShortcutId::ResetInterfaceScale,
    ] {
        let combo = keymap.get(id);
        if combo.is_empty() {
            continue;
        }
        let combo = settings::platform_combo(combo);
        let combo = if Keystroke::parse(&combo).is_ok() {
            combo
        } else {
            settings::platform_combo(id.default_combo())
        };
        bindings.push(match id {
            ShortcutId::IncreaseInterfaceScale => {
                KeyBinding::new(&combo, IncreaseInterfaceScale, None)
            }
            ShortcutId::DecreaseInterfaceScale => {
                KeyBinding::new(&combo, DecreaseInterfaceScale, None)
            }
            ShortcutId::ResetInterfaceScale => KeyBinding::new(&combo, ResetInterfaceScale, None),
            _ => unreachable!(),
        });
    }
    for alias in increase_aliases(keymap) {
        bindings.push(KeyBinding::new(
            &settings::platform_combo(alias),
            IncreaseInterfaceScale,
            None,
        ));
    }
    cx.bind_keys(bindings);
}

/// Install persisted zoom before the first frame of a new/reopened window,
/// and follow later centralized settings mutations as well as our controls.
pub fn observe_window(window: &mut Window, cx: &mut App) -> Subscription {
    window.set_ui_zoom(current(cx), cx);
    window.observe_global::<settings::SettingsStore>(cx, |window, cx| {
        window.set_ui_zoom(current(cx), cx);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_finite_values_recover_and_finite_values_clamp() {
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(normalize(invalid), DEFAULT);
            assert_eq!(changed(invalid, Change::Reset), DEFAULT);
        }
        assert_eq!(normalize(-1.0), MIN);
        assert_eq!(normalize(999.0), MAX);
        assert_eq!(normalize(1.37), 1.37);
    }

    #[test]
    fn plus_aliases_are_parseable_and_respect_custom_or_unbound_shortcuts() {
        let mut keymap = settings::KeymapConfig::default();
        assert_eq!(
            increase_aliases(&keymap).collect::<Vec<_>>(),
            INCREASE_ALIASES
        );
        for mac in [false, true] {
            for alias in INCREASE_ALIASES {
                assert!(Keystroke::parse(&settings::platform_combo_on(mac, alias)).is_ok());
            }
        }
        keymap.save_file = "mod-=".into();
        assert!(!increase_aliases(&keymap).any(|alias| alias == "mod-="));
        keymap.increase_interface_scale = "mod-alt-z".into();
        assert_eq!(increase_aliases(&keymap).count(), 0);
        keymap.increase_interface_scale.clear();
        assert_eq!(increase_aliases(&keymap).count(), 0);
    }

    #[test]
    fn actions_step_by_twenty_five_points_and_reset_independently() {
        assert_eq!(changed(DEFAULT, Change::Increase), 1.25);
        assert_eq!(changed(1.25, Change::Decrease), DEFAULT);
        assert_eq!(changed(MAX, Change::Increase), MAX);
        assert_eq!(changed(MIN, Change::Decrease), MIN);
        assert_eq!(changed(1.37, Change::Increase), 1.62);
        for scale in PRESETS {
            assert_eq!(changed(scale, Change::Reset), DEFAULT);
        }
    }
}
