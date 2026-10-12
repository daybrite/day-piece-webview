// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! Synthetic host proves canvas dispatch, evaluation replies, and scope disposal without Servo.
use day_core::{BuildCx, Piece, RNode};
use day_piece_webview::{JsHandle, WebPatch, WebProps, WebRenderer, renderer, web_view};
use day_pieces::Decorate;
use day_reactive::{Scope, Signal};
use day_spec::Support;
use std::{
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

thread_local! {
    static ORDINARY_CANVAS: Cell<Option<RNode>> = const { Cell::new(None) };
    static SCOPE: RefCell<Option<Scope>> = const { RefCell::new(None) };
    static COMMANDS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}
static HOST: renderer::surface::Host = renderer::surface::Host {
    capabilities: renderer::Capabilities {
        private_browsing: Support::Unsupported,
        profiles: Support::Unsupported,
        clear_data: Support::Unsupported,
    },
    attach: |_, _, props: &WebProps| {
        assert_eq!(props.renderer, WebRenderer::Servo);
        COMMANDS.with(|c| c.borrow_mut().push("attach".into()));
    },
    update: |node, patch| {
        if let WebPatch::Eval { req, .. } = patch {
            COMMANDS.with(|c| c.borrow_mut().push("eval".into()));
            day_core::enqueue_event(
                day_core::rnode_to_id(node),
                renderer::reply_event(*req, "1\u{1f}42".into()),
            );
        }
    },
    release: |_| COMMANDS.with(|c| c.borrow_mut().push("release".into())),
};
struct Scoped(day_piece_webview::WebView);
impl Piece for Scoped {
    fn build(self, cx: &mut BuildCx) -> RNode {
        let scope = Scope::child();
        SCOPE.with(|s| *s.borrow_mut() = Some(scope));
        scope.enter(|| {
            let ordinary = cx.leaf(
                day_spec::kinds::CANVAS,
                &day_spec::props::CanvasProps::default(),
                day_core::Flex::default(),
            );
            ORDINARY_CANVAS.with(|node| node.set(Some(ordinary)));
            self.0.build(cx)
        })
    }
}
#[test]
fn surface_routes_eval_and_disposes_only_its_own_nodes() {
    assert_eq!(
        renderer::surface::register(WebRenderer::Servo, &HOST),
        Err(renderer::RegistrationError::WrongThread)
    );
    let (mock, probe) = day_mock::MockToolkit::new();
    let handle = JsHandle::new();
    let view_handle = handle;
    day_core::launch_with(mock, day_spec::WindowOptions::default(), move || {
        assert_eq!(
            renderer::surface::register(WebRenderer::WebView, &HOST),
            Err(renderer::RegistrationError::ReservedRenderer)
        );
        renderer::surface::register(WebRenderer::Servo, &HOST).unwrap();
        renderer::surface::register(WebRenderer::Servo, &HOST).unwrap();
        Scoped(
            web_view(Signal::new("about:blank".into()))
                .renderer(WebRenderer::Servo)
                .unwrap()
                .js(view_handle),
        )
        .any()
    });
    assert_eq!(probe.find_by_kind("day.canvas").len(), 2);
    assert_eq!(probe.find_by_kind("day.webview").len(), 0);
    let ordinary = ORDINARY_CANVAS.with(|node| node.get().unwrap());
    let reply = std::rc::Rc::new(RefCell::new(None));
    let result = reply.clone();
    assert!(day_core::piece_operation(
        ordinary,
        "day.webview.eval",
        "42",
        Box::new(move |value| {
            *result.borrow_mut() = Some(value);
        })
    ));
    assert!(
        reply.borrow().as_ref().unwrap().is_err(),
        "ordinary canvases must reject browser operations"
    );
    let mut eval = handle.eval("21 * 2");
    let mut cx = Context::from_waker(Waker::noop());
    let first = Pin::new(&mut eval).poll(&mut cx);
    let result = if first.is_pending() {
        day_core::pump_events();
        Pin::new(&mut eval).poll(&mut cx)
    } else {
        first
    };
    assert_eq!(result, Poll::Ready(Ok("42".into())));
    SCOPE.with(|s| s.borrow_mut().take().unwrap().dispose());
    assert_eq!(
        COMMANDS.with(|c| c.borrow().clone()),
        ["attach", "eval", "release"]
    );
    let mut after_close = handle.eval("42");
    assert!(matches!(
        Pin::new(&mut after_close).poll(&mut cx),
        Poll::Ready(Err(day_piece_webview::EvalError::ViewGone))
    ));
    day_core::uninstall_tree();
}
