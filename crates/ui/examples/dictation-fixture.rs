//! Native event-backend regression fixture; it never synthesizes keyboard input.
//! Usage: dictation-fixture OUTPUT_DIRECTORY
//! OUTPUT_DIRECTORY/data is isolated fixture-only storage (sparse fake weights).
//! events.jsonl records capture lifecycle; native.log records shortcut release reasons.
//! Hold the default dictation shortcut for >2s, allow native repeat, then release.
//! Expected: one start, no finish/drop while held, one finish/final/drop on release.
use gpui::{
    AppContext, Bounds, Context, Entity, Render, Subscription, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, size,
};
use paku_ui::{
    app_menus, appearance, composer, dictation_fixture, history, icons, settings, shell, state,
    theme, theme_library, typography,
};
use std::{fs::File, path::PathBuf};

struct Root {
    composer: Entity<composer::Composer>,
    _activation: Subscription,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::Theme::of(cx).clone();
        div()
            .id("dictation-fixture")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .p(px(24.0))
            .gap(px(16.0))
            .child("Native dictation keyboard fixture")
            .child("Hold Ctrl+D for two seconds; release to insert the fixed fixture text.")
            .child(
                "No microphone, audio, model inference, engine connection, or keyboard simulation.",
            )
            .child(div().flex_1())
            .child(div().w_full().flex_none().child(self.composer.clone()))
    }
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(std::env::args_os().nth(1).ok_or_else(|| {
        anyhow::anyhow!("usage: dictation-fixture OUTPUT_DIRECTORY (isolated fixture storage)")
    })?);
    std::fs::create_dir_all(&output)?;
    let data = output.join("data");
    std::fs::create_dir_all(&data)?;
    let trace = File::create(output.join("native.log"))?;
    tracing_subscriber::fmt()
        .with_ansi(false)
        // Restrict logs to metadata-only dictation instrumentation.
        .with_env_filter("paku_ui::dictation=debug,paku_ui::dictation_fixture=info")
        .with_writer(move || trace.try_clone().expect("clone fixture trace file"))
        .init();

    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let mut prefs = settings::UiSettings::default();
            prefs.ui_scale = std::env::var("PAKU_DICTATION_FIXTURE_ZOOM")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1.);
            prefs.ui_font_size = serde_json::from_str("18").unwrap();
            settings::init(prefs.clone(), data.clone(), cx);
            let fonts = typography::register_fonts(cx);
            typography::init(
                prefs.ui_font_family.clone(),
                prefs.ui_font_size,
                prefs.terminal_font_family.clone(),
                prefs.terminal_font_size,
                prefs.code_font_family.clone(),
                prefs.code_font_size,
                fonts,
                cx,
            );
            theme_library::init(data.clone(), cx);
            appearance::init(
                appearance::AppearanceMode::Dark,
                prefs.theme_selection,
                prefs.accent,
                prefs.surface,
                cx,
            );
            history::init(
                prefs.git_history_columns,
                prefs.git_history_column_widths,
                prefs.git_history_column_order,
                prefs.git_history_author_display,
                cx,
            );
            composer::init(cx, prefs.composer_send_behavior);
            app_menus::init(cx);
            shell::apply_keymap(cx, &prefs.keymap, prefs.composer_send_behavior);
            dictation_fixture::configure(&data, &output.join("events.jsonl"), cx)
                .expect("configure isolated fake dictation");

            paku_ui::terminal::panel::init(cx);
            let state = cx.new(|_| {
                let mut state = state::AppState::new();
                state.connection = state::ConnectionStatus::Ready;
                state.workspace_scope = Some(paku_proto::WorkspaceScope::Local);
                state.auth = Some(paku_proto::AuthState::SignedOut);
                state.local_device_id = Some("fixture-device".into());
                state.spaces.push(paku_proto::Space {
                    id: "fixture-project".into(),
                    device_id: "fixture-device".into(),
                    path: data.display().to_string(),
                    name: Some("Dictation fixture".into()),
                    git_detected: false,
                    git_checked_at: None,
                    checkout_id: None,
                    repository_id: None,
                    created_at: chrono::Utc::now(),
                });
                state.selected_space = Some("fixture-project".into());
                state
            });
            let boot = paku_ui::EngineBootConfig {
                data_dir: data.clone(),
                ipc_port: 0,
                edge_url: String::new(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: paku_proto::HarnessId::Pi,
            };
            if std::env::var_os("PAKU_DICTATION_FIXTURE_SHELL").is_some() {
                cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(40.), px(40.)),
                            size(px(1200.), px(760.)),
                        ))),
                        ..Default::default()
                    },
                    |window, cx| {
                        paku_ui::ui_scale::observe_window(window, cx).detach();
                        cx.new(|cx| shell::Shell::new(state, boot, cx))
                    },
                )
                .expect("open native shell fixture");
                cx.activate(true);
                return;
            }
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        gpui::point(px(40.0), px(40.0)),
                        size(px(1000.0), px(700.0)),
                    ))),
                    ..Default::default()
                },
                |window, cx| {
                    window.set_window_title("Paku native dictation fixture");
                    paku_ui::ui_scale::observe_window(window, cx).detach();
                    let composer = cx.new(|cx| composer::Composer::new(state, cx));
                    cx.new(|cx| {
                        let activation = cx.observe_window_activation(window, |_, window, _| {
                            tracing::info!(target: "paku_ui::dictation_fixture",
                                active = window.is_window_active(), "Native window activation");
                        });
                        Root {
                            composer,
                            _activation: activation,
                        }
                    })
                },
            )
            .expect("open native dictation fixture window");
            cx.activate(true);
        });
    Ok(())
}
