// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
use super::*;
use day_appkit::AppKit;
use day_spec::NodeId;
use objc2::{rc::Retained};
use objc2_app_kit::NSView;
fn make(b: &mut AppKit, p: &WebProps, id: NodeId) -> Retained<NSView> { super::macos_web::make(b.mtm(), p, id, day_appkit::emit, None) }
fn update(_: &mut AppKit, h: &Retained<NSView>, p: &WebPatch) { super::macos_web::update(h,p); }
fn release(_: &mut AppKit, h: &Retained<NSView>) { super::macos_web::release(h); }
day_pieces::renderer!(day_appkit::RENDERERS, AppKit,
 kind: KIND, props: WebProps, patch: WebPatch,
 make: make, update: update, measure: day_pieces::fill_measure, release: release);
