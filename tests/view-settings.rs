// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! Zoom, desktop mode and the script switch reach a renderer twice over: their values when the
//! view is built ride in `WebProps::settings` (Day drops a patch sent before the node is
//! realized), and each later change is exactly one patch. A value that does not change, or that
//! clamps to the zoom already applied, sends nothing.
use day_core::{BuildCx, Piece, RNode};
use day_piece_webview::{WebPatch, WebProps, WebRenderer, WebSettings, renderer, web_view};
use day_pieces::Decorate;
use day_reactive::Signal;
use day_spec::Support;
use std::cell::RefCell;

thread_local! {
    static BUILT: RefCell<Vec<WebSettings>> = const { RefCell::new(Vec::new()) };
    static PATCHES: RefCell<Vec<WebPatch>> = const { RefCell::new(Vec::new()) };
}

static HOST: renderer::surface::Host = renderer::surface::Host {
    capabilities: renderer::Capabilities {
        private_browsing: Support::Unsupported,
        profiles: Support::Unsupported,
        clear_data: Support::Unsupported,
    },
    attach: |_, _, props: &WebProps| BUILT.with(|b| b.borrow_mut().push(props.settings)),
    update: |_, patch| PATCHES.with(|p| p.borrow_mut().push(patch.clone())),
    release: |_| {},
};

struct View(day_piece_webview::WebView);
impl Piece for View {
    fn build(self, cx: &mut BuildCx) -> RNode {
        self.0.build(cx)
    }
}

/// Settle the reactive graph and the event queue, then hand back the patches sent so far.
fn settle() -> Vec<WebPatch> {
    for _ in 0..4 {
        day_core::pump_events();
        day_reactive::flush_sync();
    }
    PATCHES.with(|p| p.borrow_mut().drain(..).collect())
}

fn launch(view: impl FnOnce() -> day_piece_webview::WebView + 'static) {
    let (mock, _probe) = day_mock::MockToolkit::new();
    day_core::launch_with(mock, day_spec::WindowOptions::default(), move || {
        renderer::surface::register(WebRenderer::Servo, &HOST).unwrap();
        View(view().renderer(WebRenderer::Servo).unwrap()).any()
    });
}

#[test]
fn settings_start_in_the_props_and_each_change_is_one_patch() {
    // A view without the modifiers is built with the defaults and never sends a setting.
    launch(|| web_view(Signal::new("about:blank".into())));
    assert_eq!(settle(), []);
    assert_eq!(
        BUILT.with(|b| b.borrow_mut().pop()),
        Some(WebSettings::default())
    );
    assert_eq!(
        WebSettings::default(),
        WebSettings {
            zoom: 1.0,
            desktop_mode: false,
            javascript: true,
        }
    );
    day_core::uninstall_tree();

    // Constants are initial values only. An out-of-range zoom is clamped before it is applied.
    launch(|| {
        web_view(Signal::new("about:blank".into()))
            .zoom(9.0)
            .desktop_mode(true)
            .javascript(false)
    });
    assert_eq!(settle(), []);
    assert_eq!(
        BUILT.with(|b| b.borrow_mut().pop()),
        Some(WebSettings {
            zoom: 5.0,
            desktop_mode: true,
            javascript: false,
        })
    );
    day_core::uninstall_tree();

    // Signals: the build carries their values, and only real changes become patches.
    let zoom = Signal::global(1.5);
    let desktop = Signal::global(true);
    let scripts = Signal::global(false);
    launch(move || {
        web_view(Signal::new("about:blank".into()))
            .zoom(zoom)
            .desktop_mode(desktop)
            .javascript(scripts)
    });
    assert_eq!(settle(), [], "building the view sends no setting patch");
    assert_eq!(
        BUILT.with(|b| b.borrow_mut().pop()),
        Some(WebSettings {
            zoom: 1.5,
            desktop_mode: true,
            javascript: false,
        })
    );

    zoom.set(2.0);
    assert_eq!(settle(), [WebPatch::Zoom(2.0)]);
    zoom.set(2.0);
    assert_eq!(settle(), [], "an unchanged zoom sends nothing");
    zoom.set(8.0);
    assert_eq!(settle(), [WebPatch::Zoom(5.0)], "clamped to the range");
    zoom.set(7.0);
    assert_eq!(settle(), [], "clamps to the zoom already applied");
    zoom.set(f64::NAN);
    assert_eq!(settle(), [WebPatch::Zoom(1.0)], "not a number is 100%");

    desktop.set(false);
    assert_eq!(settle(), [WebPatch::DesktopMode(false)]);
    desktop.set(false);
    assert_eq!(settle(), []);

    scripts.set(true);
    assert_eq!(settle(), [WebPatch::JavaScript(true)]);
    scripts.set(true);
    assert_eq!(settle(), []);

    // Changes made in one turn are each one patch, in no particular order.
    zoom.set(0.5);
    desktop.set(true);
    scripts.set(false);
    let mut patches = settle();
    patches.sort_by_key(|patch| format!("{patch:?}"));
    assert_eq!(
        patches,
        [
            WebPatch::DesktopMode(true),
            WebPatch::JavaScript(false),
            WebPatch::Zoom(0.5),
        ]
    );
    day_core::uninstall_tree();
}

#[test]
fn closures_drive_settings_like_signals() {
    let level = Signal::global(3u8);
    launch(move || {
        web_view(Signal::new("about:blank".into())).zoom(move || f64::from(level.get()) * 0.25)
    });
    assert_eq!(settle(), []);
    assert_eq!(
        BUILT.with(|b| b.borrow_mut().pop()).map(|s| s.zoom),
        Some(0.75)
    );
    level.set(6);
    assert_eq!(settle(), [WebPatch::Zoom(1.5)]);
    day_core::uninstall_tree();
}
