//! No native display or renderer is required by these coordinate-layer tests.
use super::*;
use crate::{TestAppContext, canvas, div, rgb};

#[derive(Clone, Default)]
struct ImeProbe {
    queried_ranges: Rc<RefCell<Vec<Range<usize>>>>,
    selected_range: Rc<RefCell<Option<Range<usize>>>>,
    queried_point: Rc<Cell<Point<Pixels>>>,
    layout_dependent: bool,
    // A paint-time snapshot, like ElementInputHandler::element_bounds. Sharing
    // live bounds here would mask a detached handler surviving the redraw.
    painted_bounds: Option<Bounds<Pixels>>,
}

impl InputHandler for ImeProbe {
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut App,
    ) -> Option<crate::UTF16Selection> {
        Some(crate::UTF16Selection {
            range: self.selected_range.borrow().clone().unwrap_or(3..7),
            reversed: false,
        })
    }
    fn marked_text_range(&mut self, _: &mut Window, _: &mut App) -> Option<Range<usize>> {
        None
    }
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<String> {
        *adjusted = Some(range);
        Some("text".into())
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        _: &str,
        _: &mut Window,
        _: &mut App,
    ) {
        *self.selected_range.borrow_mut() = range;
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        _: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut App,
    ) {
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut App) {}
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<Bounds<Pixels>> {
        self.queried_ranges.borrow_mut().push(range.clone());
        if let Some(bounds) = self.painted_bounds {
            return Some(Bounds::new(
                point(bounds.right() - px(30.), bounds.bottom() - px(40.)),
                size(px(2.), px(16.)),
            ));
        }
        Some(Bounds::new(
            point(px(range.start as f32 * 10.), px(20.)),
            size(px(2.), px(16.)),
        ))
    }
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<usize> {
        self.queried_point.set(point);
        if let Some(bounds) = self.painted_bounds {
            return (point == bounds.bottom_right() - crate::point(px(30.), px(40.))).then_some(7);
        }
        Some(7)
    }
    fn set_selected_text_range(&mut self, range: Range<usize>, _: &mut Window, _: &mut App) {
        *self.selected_range.borrow_mut() = Some(range);
    }
    fn element_bounds(&mut self, _: &mut Window, _: &mut App) -> Option<Bounds<Pixels>> {
        Some(
            self.painted_bounds
                .unwrap_or_else(|| Bounds::new(point(px(10.), px(20.)), size(px(100.), px(30.)))),
        )
    }
}

struct ZoomView {
    bounds: Rc<Cell<Bounds<Pixels>>>,
    focus: FocusHandle,
    ime: Option<ImeProbe>,
}

impl Render for ZoomView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let bounds = self.bounds.clone();
        let focus = self.focus.clone();
        let ime = self.ime.clone();
        div().size_full().child(
            canvas(
                move |value, _, _| bounds.set(value),
                move |value, _, window, cx| {
                    // Fixed px geometry deliberately does not depend on rem.
                    window.paint_quad(fill(
                        Bounds::new(point(px(10.), px(20.)), size(px(24.), px(18.))),
                        rgb(0xff0000),
                    ));
                    if let Some(mut ime) = ime {
                        ime.painted_bounds = ime.layout_dependent.then_some(value);
                        window.handle_input(&focus, ime, cx);
                    }
                },
            )
            .size_full(),
        )
    }
}

fn add_window(cx: &mut TestAppContext, ime: Option<ImeProbe>) -> WindowHandle<ZoomView> {
    cx.open_window(size(px(800.), px(600.)), |_, cx| ZoomView {
        bounds: Rc::new(Cell::new(Bounds::default())),
        focus: cx.focus_handle(),
        ime,
    })
}

#[test]
fn ui_zoom_ime_reflows_within_setter_update_before_effects() {
    let mut cx = TestAppContext::single();
    let probe = ImeProbe {
        layout_dependent: true,
        ..Default::default()
    };
    let handle = add_window(&mut cx, Some(probe.clone()));
    let focus = handle
        .update(&mut cx, |view, _, _| view.focus.clone())
        .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.focus(&focus, cx);
        window.draw(cx).clear();
        let mut input = window.platform_window.take_input_handler().unwrap();
        assert_eq!(
            input.selected_bounds(window, cx),
            Some(Bounds::new(
                point(px(770.), px(560.)),
                size(px(2.), px(16.))
            ))
        );
        window.set_ui_zoom(2., cx);
        assert!(window.ui_zoom_pending);
        // The same Window update has not ended, so flush_effects cannot have
        // auto-drawn. selected_bounds is also the direct native pull's body;
        // do not recursively update this already-borrowed Window.
        assert_eq!(
            input.selected_bounds(window, cx),
            Some(Bounds::new(
                point(px(740.), px(520.)),
                size(px(4.), px(32.))
            ))
        );
        assert!(!window.ui_zoom_pending);
        assert_eq!(*probe.queried_ranges.borrow(), vec![7..7, 7..7]);
        window.platform_window.set_input_handler(input);
    })
    .unwrap();
}

#[test]
fn ui_zoom_direct_native_geometry_queries_refresh_detached_handler_before_effects() {
    // Each entry point must be the first query after the setter. An earlier
    // candidate query or TestAppContext's automatic draw would mask missing
    // refreshes in element bounds, range bounds or character lookup.
    for query in 0..4 {
        let mut cx = TestAppContext::single();
        let probe = ImeProbe {
            layout_dependent: true,
            ..Default::default()
        };
        let handle = add_window(&mut cx, Some(probe.clone()));
        let (focus, bounds) = handle
            .update(&mut cx, |view, _, _| {
                (view.focus.clone(), view.bounds.clone())
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.focus(&focus, cx);
            window.draw(cx).clear();
        })
        .unwrap();
        assert_eq!(bounds.get().size, size(px(800.), px(600.)));

        // Hold an outer App update, but release its borrow so native methods
        // can perform their single AsyncWindowContext update. No effects or
        // test-only automatic draw may run between setter and native pull.
        cx.app.borrow_mut().start_update();
        let mut input = cx
            .update_window(handle.into(), |_, window, cx| {
                let input = window.platform_window.take_input_handler().unwrap();
                window.set_ui_zoom(2., cx);
                assert!(window.ui_zoom_pending);
                input
            })
            .unwrap();
        assert_eq!(bounds.get().size, size(px(800.), px(600.)));
        let caret = Bounds::new(point(px(740.), px(520.)), size(px(4.), px(32.)));
        match query {
            0 => assert_eq!(input.ime_candidate_bounds(), Some(caret)),
            1 => {
                assert_eq!(input.bounds_for_range(5..6), Some(caret));
                assert_eq!(probe.queried_ranges.borrow().last(), Some(&(5..6)));
            }
            2 => assert_eq!(
                input.element_bounds(),
                Some(Bounds::new(Point::default(), size(px(800.), px(600.))))
            ),
            3 => {
                assert_eq!(
                    input.character_index_for_point(point(px(740.), px(520.))),
                    Some(7)
                );
                assert_eq!(probe.queried_point.get(), point(px(370.), px(260.)));
            }
            _ => unreachable!(),
        }
        assert_eq!(bounds.get().size, size(px(400.), px(300.)));
        // Check replacement even when candidate/range/index was the first
        // query: drawing without acquiring the fresh handler is insufficient.
        assert_eq!(
            input.element_bounds(),
            Some(Bounds::new(Point::default(), size(px(800.), px(600.))))
        );
        assert_eq!(input.selected_text_range(true).unwrap().range, 3..7);
        cx.update_window(handle.into(), |_, window, _| {
            assert!(!window.ui_zoom_pending);
            window.platform_window.set_input_handler(input);
        })
        .unwrap();
        cx.app.borrow_mut().finish_update();
    }
}

#[test]
fn ui_zoom_ime_query_during_draw_does_not_reflow_recursively() {
    let mut cx = TestAppContext::single();
    let handle = add_window(
        &mut cx,
        Some(ImeProbe {
            layout_dependent: true,
            ..Default::default()
        }),
    );
    let focus = handle
        .update(&mut cx, |view, _, _| view.focus.clone())
        .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.focus(&focus, cx);
        window.draw(cx).clear();
        let mut input = window.platform_window.take_input_handler().unwrap();
        window.set_ui_zoom(2., cx);
        window.invalidator.set_phase(DrawPhase::Paint);
        assert_eq!(
            input.selected_bounds(window, cx),
            Some(Bounds::new(
                point(px(1540.), px(1120.)),
                size(px(4.), px(32.))
            ))
        );
        assert!(window.ui_zoom_pending);
        window.invalidator.set_phase(DrawPhase::None);
        assert_eq!(
            input.selected_bounds(window, cx),
            Some(Bounds::new(
                point(px(740.), px(520.)),
                size(px(4.), px(32.))
            ))
        );
        assert!(!window.ui_zoom_pending);
        window.platform_window.set_input_handler(input);
    })
    .unwrap();
}

#[test]
fn ui_zoom_native_click_hits_rebuilt_fixed_px_target_immediately() {
    struct HitView(Rc<Cell<usize>>);
    impl Render for HitView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let hits = self.0.clone();
            div().size_full().child(
                div()
                    .absolute()
                    .left(px(10.))
                    .top(px(20.))
                    .w(px(24.))
                    .h(px(18.))
                    .on_mouse_down(MouseButton::Left, move |_, _, _| hits.set(hits.get() + 1)),
            )
        }
    }
    let mut cx = TestAppContext::single();
    let hits = Rc::new(Cell::new(0));
    let handle = cx.open_window(size(px(800.), px(600.)), |_, _| HitView(hits.clone()));
    let mut platform = cx
        .update_window(handle.into(), |_, window, cx| {
            window.draw(cx);
            window.set_ui_zoom(2., cx);
            window.platform_window.as_test().unwrap().clone()
        })
        .unwrap();
    // At 200%, native (60, 60) is UI (30, 30), inside the target. The old
    // unzoomed hitbox would miss; no display tick or explicit redraw intervenes.
    platform.simulate_input(PlatformInput::MouseMove(crate::MouseMoveEvent {
        position: point(px(60.), px(60.)),
        ..Default::default()
    }));
    platform.simulate_input(PlatformInput::MouseDown(crate::MouseDownEvent {
        button: MouseButton::Left,
        position: point(px(60.), px(60.)),
        modifiers: Default::default(),
        click_count: 1,
        first_mouse: false,
    }));
    assert_eq!(hits.get(), 1);
}

#[test]
fn ui_zoom_default_validation_reset_and_independent_windows() {
    let mut cx = TestAppContext::single();
    let first = add_window(&mut cx, None);
    let second = add_window(&mut cx, None);
    cx.update_window(first.into(), |_, window, cx| {
        let native_bounds = window.bounds();
        let rem = window.rem_size();
        assert_eq!(window.ui_zoom(), 1.);
        assert_eq!(window.native_scale_factor(), 2.);
        for (input, expected) in [(0., 0.5), (-1., 0.5), (9., 3.), (1.25, 1.25)] {
            window.set_ui_zoom(input, cx);
            assert_eq!(window.ui_zoom(), expected);
        }
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            window.set_ui_zoom(invalid, cx);
            assert_eq!(window.ui_zoom(), 1.25);
        }
        assert_eq!(window.rem_size(), rem);
        assert_eq!(window.bounds(), native_bounds);
        window.set_ui_zoom(1., cx);
        assert_eq!(window.viewport_size(), size(px(800.), px(600.)));
        assert_eq!(window.scale_factor(), window.native_scale_factor());
    })
    .unwrap();
    cx.update_window(second.into(), |_, window, _| {
        assert_eq!(window.ui_zoom(), 1.)
    })
    .unwrap();
}

#[test]
fn ui_zoom_entity_update_retains_bounds_observers() {
    let mut cx = TestAppContext::single();
    let handle = add_window(&mut cx, None);
    let calls = Rc::new(Cell::new(0));
    let _subscription = handle
        .update(&mut cx, |_, window, cx| {
            let calls = calls.clone();
            cx.observe_window_bounds(window, move |_, _, _| calls.set(calls.get() + 1))
        })
        .unwrap();
    handle
        .update(&mut cx, |_, window, cx| window.set_ui_zoom(1.25, cx))
        .unwrap();
    handle
        .update(&mut cx, |_, window, cx| window.set_ui_zoom(1.5, cx))
        .unwrap();
    assert_eq!(calls.get(), 2);
}

#[test]
fn anchored_popup_uses_parent_zoom_without_reborrowing_parent() {
    use crate::popup::{PopupAnchor, PopupConstraintAdjustment, PopupGravity, PopupOptions};
    let mut cx = TestAppContext::single();
    let parent = add_window(&mut cx, None);
    let anchor = Bounds::new(point(px(10.), px(20.)), size(px(30.), px(40.)));
    let popup = parent
        .update(&mut cx, |_, window, cx| {
            window.set_ui_zoom(2., cx);
            cx.open_window(
                WindowOptions {
                    kind: crate::WindowKind::AnchoredPopup(PopupOptions {
                        parent: parent.into(),
                        anchor_rect: anchor,
                        anchor: PopupAnchor::BottomLeft,
                        gravity: PopupGravity::BottomRight,
                        constraint_adjustment: PopupConstraintAdjustment::empty(),
                        offset: point(px(3.), px(4.)),
                        grab: false,
                    }),
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Point::default(),
                        size(px(100.), px(80.)),
                    ))),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|cx| ZoomView {
                        bounds: Rc::new(Cell::new(Bounds::default())),
                        focus: cx.focus_handle(),
                        ime: None,
                    })
                },
            )
            .unwrap()
        })
        .unwrap();
    cx.update_window(popup.into(), |_, window, _| {
        assert_eq!(window.ui_zoom(), 2.);
        assert_eq!(window.viewport_size(), size(px(100.), px(80.)));
        assert_eq!(window.bounds().size, size(px(200.), px(160.)));
        assert_eq!(window.scale_factor(), 4.);
        let platform = window.platform_window.as_test().unwrap().clone();
        let state = platform.0.lock();
        let crate::WindowKind::AnchoredPopup(popup) = &state.kind else {
            panic!()
        };
        assert_eq!(popup.anchor_rect, anchor.map(|value| value * 2.));
        assert_eq!(popup.offset, point(px(6.), px(8.)));
    })
    .unwrap();
}

#[test]
fn ui_zoom_layout_scene_identity_resize_dpi_and_observers() {
    let mut cx = TestAppContext::single();
    let handle = add_window(&mut cx, None);
    let notifications = Rc::new(Cell::new(0));
    let _subscription = handle
        .update(&mut cx, |_, window, cx| {
            let notifications = notifications.clone();
            cx.observe_window_bounds(window, move |_, _, _| {
                notifications.set(notifications.get() + 1);
            })
        })
        .unwrap();
    let mut platform = cx
        .update_window(handle.into(), |_, window, _| {
            window.platform_window.as_test().unwrap().clone()
        })
        .unwrap();
    cx.update_window(handle.into(), |view, window, cx| {
        window.set_ui_zoom(2., cx);
        window.set_ui_zoom(2., cx); // Unchanged values do not notify twice.
        window.draw(cx).clear();
        assert_eq!(
            view.downcast::<ZoomView>()
                .unwrap()
                .read(cx)
                .bounds
                .get()
                .size,
            size(px(400.), px(300.))
        );
        assert_eq!(window.scale_factor(), 4.);
        assert_eq!(
            window.viewport_size().scale(window.scale_factor()),
            size(px(800.), px(600.)).scale(2.)
        );
        let quad = &window.rendered_frame.scene.quads[0];
        assert_eq!(
            quad.bounds,
            Bounds::new(point(px(10.), px(20.)), size(px(24.), px(18.))).scale(4.)
        );
        assert_eq!(window.bounds().size, size(px(800.), px(600.)));
    })
    .unwrap();
    assert_eq!(notifications.get(), 1);
    platform.simulate_resize(size(px(1000.), px(700.)));
    platform.simulate_scale_factor_change(1.5);
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear();
        assert_eq!(window.ui_zoom(), 2.);
        assert_eq!(window.native_scale_factor(), 1.5);
        assert_eq!(window.scale_factor(), 3.);
        assert_eq!(window.viewport_size(), size(px(500.), px(350.)));
        assert_eq!(
            window.viewport_size().scale(window.scale_factor()),
            size(px(1000.), px(700.)).scale(1.5)
        );
    })
    .unwrap();
    assert_eq!(notifications.get(), 3);
}

#[test]
fn native_input_boundary_is_not_applied_to_synthetic_events() {
    let mut cx = TestAppContext::single();
    let handle = add_window(&mut cx, None);
    let mut platform = cx
        .update_window(handle.into(), |_, window, cx| {
            window
                .platform_window
                .as_test()
                .unwrap()
                .0
                .lock()
                .mouse_position = point(px(120.), px(80.));
            window.set_ui_zoom(2., cx);
            assert_eq!(window.mouse_position(), point(px(60.), px(40.)));
            window.platform_window.as_test().unwrap().clone()
        })
        .unwrap();
    platform.simulate_input(PlatformInput::MouseMove(crate::MouseMoveEvent {
        position: point(px(100.), px(60.)),
        ..Default::default()
    }));
    cx.update_window(handle.into(), |_, window, cx| {
        assert_eq!(window.mouse_position(), point(px(50.), px(30.)));
        assert!(
            !window.ui_zoom_pending,
            "input must refresh zoomed hitboxes before dispatch"
        );
        window.dispatch_event(
            PlatformInput::MouseMove(crate::MouseMoveEvent {
                position: point(px(100.), px(60.)),
                ..Default::default()
            }),
            cx,
        );
        assert_eq!(window.mouse_position(), point(px(100.), px(60.)));
    })
    .unwrap();
}

#[test]
fn native_pixel_scroll_and_positions_but_not_lines_pinch_delta_or_keys() {
    use crate::{
        FileDropEvent, MouseMoveEvent, PinchEvent, ScrollDelta, ScrollWheelEvent, TouchEvent,
    };
    let position = point(px(60.), px(90.));
    let expected = position * 0.5;
    for input in [
        PlatformInput::MouseDown(crate::MouseDownEvent {
            position,
            ..Default::default()
        }),
        PlatformInput::MouseUp(crate::MouseUpEvent {
            position,
            ..Default::default()
        }),
        PlatformInput::MousePressure(crate::MousePressureEvent {
            position,
            ..Default::default()
        }),
        PlatformInput::MouseExited(crate::MouseExitEvent {
            position,
            ..Default::default()
        }),
        PlatformInput::FileDrop(FileDropEvent::Entered {
            position,
            paths: Default::default(),
        }),
        PlatformInput::FileDrop(FileDropEvent::Pending { position }),
    ] {
        let converted = match input.into_ui_coordinates(2.) {
            PlatformInput::MouseDown(event) => event.position,
            PlatformInput::MouseUp(event) => event.position,
            PlatformInput::MousePressure(event) => event.position,
            PlatformInput::MouseExited(event) => event.position,
            PlatformInput::FileDrop(FileDropEvent::Entered { position, .. })
            | PlatformInput::FileDrop(FileDropEvent::Pending { position }) => position,
            _ => unreachable!(),
        };
        assert_eq!(converted, expected);
    }
    let input = PlatformInput::ScrollWheel(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(6.), px(-12.))),
        ..Default::default()
    })
    .into_ui_coordinates(2.);
    let PlatformInput::ScrollWheel(event) = input else {
        panic!()
    };
    assert_eq!(event.position, expected);
    let ScrollDelta::Pixels(delta) = event.delta else {
        panic!()
    };
    assert_eq!(delta, point(px(3.), px(-6.)));
    let input = PlatformInput::ScrollWheel(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Lines(point(3., -6.)),
        ..Default::default()
    })
    .into_ui_coordinates(2.);
    let PlatformInput::ScrollWheel(event) = input else {
        panic!()
    };
    let ScrollDelta::Lines(delta) = event.delta else {
        panic!()
    };
    assert_eq!(event.position, expected);
    assert_eq!(delta, point(3., -6.));
    let PlatformInput::Pinch(event) = PlatformInput::Pinch(PinchEvent {
        position,
        delta: 0.125,
        ..Default::default()
    })
    .into_ui_coordinates(2.) else {
        panic!()
    };
    assert_eq!(event.position, expected);
    assert_eq!(event.delta, 0.125);
    let PlatformInput::Touch(event) = PlatformInput::Touch(TouchEvent {
        position,
        force: Some(0.7),
        ..Default::default()
    })
    .into_ui_coordinates(2.) else {
        panic!()
    };
    assert_eq!(event.position, expected);
    assert_eq!(event.force, Some(0.7));
    let PlatformInput::FileDrop(FileDropEvent::Submit { position: value }) =
        PlatformInput::FileDrop(FileDropEvent::Submit { position }).into_ui_coordinates(2.)
    else {
        panic!()
    };
    assert_eq!(value, expected);
    let PlatformInput::MouseMove(event) = PlatformInput::MouseMove(MouseMoveEvent {
        position,
        ..Default::default()
    })
    .into_ui_coordinates(1.) else {
        panic!()
    };
    assert_eq!(event.position, position);
    let PlatformInput::KeyDown(event) = PlatformInput::KeyDown(KeyDownEvent {
        keystroke: Keystroke::parse("ctrl-a").unwrap(),
        is_held: true,
        prefer_character_input: true,
    })
    .into_ui_coordinates(2.) else {
        panic!()
    };
    assert_eq!(event.keystroke, Keystroke::parse("ctrl-a").unwrap());
    assert!(event.is_held && event.prefer_character_input);
}

#[test]
fn outbound_window_local_geometry_is_zoomed_and_global_bounds_are_native() {
    let mut cx = TestAppContext::single();
    let handle = add_window(&mut cx, None);
    cx.update_window(handle.into(), |_, window, cx| {
        let global_bounds = window.bounds();
        let region = Bounds::new(point(px(10.), px(20.)), size(px(30.), px(40.)));
        window.set_client_inset(px(8.));
        window.set_input_region(Some(&[region]));
        window.set_exclusive_zone(px(20.));
        window.set_ui_zoom(2., cx);
        window.show_window_menu(point(px(12.), px(18.)));
        assert_eq!(window.bounds(), global_bounds);
        assert_eq!(window.window_bounds().get_bounds(), global_bounds);
        assert_eq!(window.client_inset(), Some(px(8.)));
        let platform = window.platform_window.as_test().unwrap().clone();
        {
            let state = platform.0.lock();
            assert_eq!(state.client_inset, Some(px(16.)));
            assert_eq!(
                state.input_region,
                Some(vec![region.map(|value| value * 2.)])
            );
            assert_eq!(state.exclusive_zone, Some(px(40.)));
            assert_eq!(state.window_menu_position, Some(point(px(24.), px(36.))));
        }
        window.resize(size(px(300.), px(200.)));
        window.bounds_changed(cx);
        assert_eq!(window.bounds().size, size(px(600.), px(400.)));
        assert_eq!(window.viewport_size(), size(px(300.), px(200.)));
        window.set_ui_zoom(1., cx);
        let state = platform.0.lock();
        assert_eq!(state.input_region, Some(vec![region]));
        assert_eq!(state.client_inset, Some(px(8.)));
    })
    .unwrap();
}

#[test]
fn ime_pull_push_character_lookup_and_utf16_ranges() {
    let mut cx = TestAppContext::single();
    let probe = ImeProbe::default();
    let handle = add_window(&mut cx, Some(probe.clone()));
    handle
        .update(&mut cx, |view, window, cx| {
            window.focus(&view.focus, cx);
            window.set_ui_zoom(2., cx);
        })
        .unwrap();
    let mut platform = cx
        .update_window(handle.into(), |_, window, cx| {
            window.draw(cx).clear();
            window.platform_window.as_test().unwrap().clone()
        })
        .unwrap();
    let mut handler = platform.take_input_handler().unwrap();
    let caret = Bounds::new(point(px(70.), px(20.)), size(px(2.), px(16.))).map(|value| value * 2.);
    assert_eq!(handler.ime_candidate_bounds(), Some(caret));
    assert_eq!(*probe.queried_ranges.borrow(), vec![7..7]);
    assert_eq!(handler.selected_text_range(true).unwrap().range, 3..7);
    assert_eq!(
        handler.bounds_for_range(5..6),
        Some(Bounds::new(point(px(50.), px(20.)), size(px(2.), px(16.))).map(|value| value * 2.))
    );
    assert_eq!(probe.queried_ranges.borrow().last(), Some(&(5..6)));
    assert_eq!(
        handler.element_bounds(),
        Some(Bounds::new(point(px(10.), px(20.)), size(px(100.), px(30.))).map(|value| value * 2.))
    );
    assert_eq!(
        handler.character_index_for_point(point(px(140.), px(40.))),
        Some(7)
    );
    assert_eq!(probe.queried_point.get(), point(px(70.), px(20.)));
    handler.set_selected_text_range(4..9);
    assert_eq!(handler.selected_text_range(true).unwrap().range, 4..9);
    let mut adjusted = None;
    assert_eq!(
        handler.text_for_range(1..3, &mut adjusted),
        Some("text".into())
    );
    assert_eq!(adjusted, Some(1..3));
    platform.set_input_handler(handler);
    cx.update_window(handle.into(), |_, window, _| {
        window.invalidate_character_coordinates()
    })
    .unwrap();
    assert!(platform.simulate_display_tick());
    assert_eq!(
        platform.0.lock().ime_position,
        Some(Bounds::new(point(px(90.), px(20.)), size(px(2.), px(16.))).map(|value| value * 2.))
    );
}
