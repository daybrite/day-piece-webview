// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! WKWebView hosted in the GTK window's Cocoa content view. A GTK allocation anchor owns
//! its lifetime; frame-clock ticks align the native child with that anchor, and unmap detaches it.
use super::*;
use day_gtk::Gtk;
use day_spec::NodeId;
use gtk4::{glib::translate::ToGlibPtr, prelude::*};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSView, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::ffi::c_void;
unsafe extern "C" {
    fn gdk_macos_surface_get_native_window(surface: *mut c_void) -> *mut NSWindow;
}
day_core::tls_group! { static VIEWS: RefCell<HashMap<usize,Retained<NSView>>> = RefCell::new(HashMap::new()); }
fn key(w: &gtk4::Widget) -> usize {
    w.as_ptr() as usize
}
fn make(_: &mut Gtk, p: &WebProps, id: NodeId) -> gtk4::Widget {
    let anchor = gtk4::DrawingArea::new();
    anchor.set_hexpand(true);
    anchor.set_vexpand(true);
    let assets = if p.inline_root.is_empty() {
        None
    } else {
        super::gtk_assets::extract_site(&p.inline_root).ok()
    };
    let view = super::macos_web::make(
        MainThreadMarker::new().expect("GTK main thread"),
        p,
        id,
        day_gtk::emit,
        assets,
    );
    VIEWS.with(|m| {
        m.borrow_mut()
            .insert(key(anchor.upcast_ref()), view.clone())
    });
    let native = view.clone();
    anchor.add_tick_callback(move |w, _| {
        if let Some(root) = w.root().and_then(|r| r.dynamic_cast::<gtk4::Widget>().ok()) {
            if let (Some(bounds), Some(surface)) = (
                w.compute_bounds(&root),
                w.native().and_then(|n| n.surface()),
            ) {
                let ptr: *mut gtk4::gdk::ffi::GdkSurface = surface.to_glib_none().0;
                let window = unsafe { gdk_macos_surface_get_native_window(ptr.cast()) };
                if let Some(window) = unsafe { window.as_ref() } {
                    if let Some(content) = window.contentView() {
                        if unsafe { native.superview() }.is_none() {
                            content.addSubview(&native)
                        }
                        let height = content.bounds().size.height;
                        let y = if content.isFlipped() {
                            bounds.y() as f64
                        } else {
                            height - bounds.y() as f64 - bounds.height() as f64
                        };
                        native.setFrame(NSRect::new(
                            NSPoint::new(bounds.x() as f64, y),
                            NSSize::new(bounds.width() as f64, bounds.height() as f64),
                        ));
                    }
                }
            }
        }
        gtk4::glib::ControlFlow::Continue
    });
    let native = view.clone();
    anchor.connect_unmap(move |_| native.removeFromSuperview());
    anchor.upcast()
}
fn update(_: &mut Gtk, w: &gtk4::Widget, p: &WebPatch) {
    if let Some(v) = VIEWS.with(|m| m.borrow().get(&key(w)).cloned()) {
        super::macos_web::update(&v, p)
    }
}
fn release(_: &mut Gtk, w: &gtk4::Widget) {
    if let Some(v) = VIEWS.with(|m| m.borrow_mut().remove(&key(w))) {
        v.removeFromSuperview();
        super::macos_web::release(&v)
    }
}
day_pieces::renderer!(day_gtk::RENDERERS,Gtk,kind:KIND,props:WebProps,patch:WebPatch,make:make,update:update,measure:day_pieces::fill_measure,release:release);
