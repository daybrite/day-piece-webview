// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
use super::*;
use day_toolkit_appkit::AppKit;
use day_spec::NodeId;
use objc2::rc::Retained;
use objc2_app_kit::NSView;
fn make(b: &mut AppKit, p: &WebProps, id: NodeId) -> Retained<NSView> {
    if p.renderer != WebRenderer::WebView {
        if let Some(host) = super::renderer::appkit::get(p.renderer) {
            return (host.make)(b.mtm(), p, id);
        }
        // The checked builder rejects this; raw WebProps can bypass it. Never substitute WKWebView.
        log::error!("{:?} renderer has not been registered", p.renderer);
        return NSView::new(b.mtm());
    }
    super::macos_web::make(b.mtm(), p, id, day_toolkit_appkit::emit, None)
}
fn update(_: &mut AppKit, h: &Retained<NSView>, p: &WebPatch) {
    if super::renderer::appkit::get(WebRenderer::Servo).is_some_and(|host| (host.update)(h, p)) {
        return;
    }
    super::macos_web::update(h, p);
}
fn release(_: &mut AppKit, h: &Retained<NSView>) {
    if super::renderer::appkit::get(WebRenderer::Servo).is_some_and(|host| (host.release)(h)) {
        return;
    }
    super::macos_web::release(h);
}
day_pieces::renderer!(day_toolkit_appkit::RENDERERS, AppKit,
 kind: KIND, props: WebProps, patch: WebPatch,
 make: make, update: update, measure: day_pieces::fill_measure, release: release);
