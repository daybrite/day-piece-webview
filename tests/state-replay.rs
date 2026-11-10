// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! A view asks its renderer for the page state once its handler exists (`WebPatch::ReportState`),
//! so a node re-attached to a retained session learns where the settled page stands. A report
//! made while the view is being created would reach a node that cannot hear it yet.
use day_core::{BuildCx, Piece, RNode};
use day_piece_webview::{PageState, WebPatch, WebProps, WebRenderer, renderer, web_view};
use day_pieces::Decorate;
use day_reactive::Signal;
use day_spec::Support;
use std::cell::RefCell;

thread_local! {
    static COMMANDS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn settled() -> PageState {
    PageState {
        url: "https://example.test/article".into(),
        title: "Article".into(),
        loading: false,
        progress: 1.0,
        can_go_back: true,
        can_go_forward: false,
    }
}

static HOST: renderer::surface::Host = renderer::surface::Host {
    capabilities: renderer::Capabilities {
        private_browsing: Support::Unsupported,
        profiles: Support::Unsupported,
        clear_data: Support::Unsupported,
    },
    attach: |_, _, _: &WebProps| COMMANDS.with(|c| c.borrow_mut().push("attach".into())),
    update: |node, patch| {
        if let WebPatch::ReportState = patch {
            COMMANDS.with(|c| c.borrow_mut().push("report".into()));
            day_core::enqueue_event(
                day_core::rnode_to_id(node),
                renderer::page_state_event(&settled()),
            );
        }
    },
    release: |_| COMMANDS.with(|c| c.borrow_mut().push("release".into())),
};

struct View(day_piece_webview::WebView);
impl Piece for View {
    fn build(self, cx: &mut BuildCx) -> RNode {
        self.0.build(cx)
    }
}

/// Drive the event queue and the reactive graph until `done` holds or the budget runs out.
/// The mock runs posted and delayed work at once, so each round settles a turn.
fn pump_until(mut done: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if done() {
            return true;
        }
        day_core::pump_events();
        day_reactive::flush_sync();
    }
    done()
}

#[test]
fn a_view_with_a_page_state_binding_asks_for_the_state_once_built() {
    let (mock, _probe) = day_mock::MockToolkit::new();
    let page = Signal::global(PageState::default());
    day_core::launch_with(mock, day_spec::WindowOptions::default(), move || {
        renderer::surface::register(WebRenderer::Servo, &HOST).unwrap();
        View(
            web_view(Signal::new("about:blank".into()))
                .renderer(WebRenderer::Servo)
                .unwrap()
                .page_state(page),
        )
        .any()
    });
    assert!(
        pump_until(|| page.get_untracked() == settled()),
        "the binding never received the replayed state: {:?}",
        page.get_untracked()
    );
    assert_eq!(
        COMMANDS.with(|c| c.borrow().clone()),
        ["attach", "report"],
        "one request, made after the view was attached"
    );
    day_core::uninstall_tree();
}
