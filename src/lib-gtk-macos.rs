// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! WKWebView hosted in the GTK window's Cocoa content view. A GTK allocation anchor owns
//! its lifetime; frame-clock ticks align the native child with that anchor, and unmap detaches it.
use super::*;
use day_gtk::Gtk;
use day_spec::NodeId;
use gtk4::{glib::translate::ToGlibPtr, prelude::*};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSEvent, NSScreen, NSView, NSWindow};
use objc2_core_graphics::{CGEvent, CGEventField, CGEventFlags, CGScrollEventUnit};
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
        if let Some(root) = w.root().and_then(|r| r.dynamic_cast::<gtk4::Widget>().ok())
            && let (Some(bounds), Some(surface)) = (
                w.compute_bounds(&root),
                w.native().and_then(|n| n.surface()),
            )
        {
            let ptr: *mut gtk4::gdk::ffi::GdkSurface = surface.to_glib_none().0;
            let window = unsafe { gdk_macos_surface_get_native_window(ptr.cast()) };
            if let Some(window) = unsafe { window.as_ref() }
                && let Some(content) = window.contentView()
            {
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
        gtk4::glib::ControlFlow::Continue
    });
    install_scroll_bridge(&anchor, view.clone());
    let native = view.clone();
    anchor.connect_unmap(move |_| native.removeFromSuperview());
    anchor.upcast()
}
// GDK consumes native wheel events before NSApplication dispatches them. The GTK
// allocation anchor receives those translated events, but WKWebView is a Cocoa
// sibling, not a GTK child. Route only scrolls over this mapped view back to WebKit.
fn install_scroll_bridge(anchor: &gtk4::DrawingArea, view: Retained<NSView>) {
    let controller = gtk4::EventControllerLegacy::new();
    controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let weak_anchor = anchor.downgrade();
    let gesture = Cell::new(false);
    controller.connect_event(move |_, event| {
        let Some(scroll) = event.downcast_ref::<gtk4::gdk::ScrollEvent>() else {
            return gtk4::glib::Propagation::Proceed;
        };
        let Some(anchor) = weak_anchor.upgrade() else {
            return gtk4::glib::Propagation::Proceed;
        };
        let Some(root) = anchor
            .root()
            .and_then(|r| r.dynamic_cast::<gtk4::Widget>().ok())
        else {
            return gtk4::glib::Propagation::Proceed;
        };
        // GDK scroll events have no position on macOS. Query the event device on
        // its surface instead; event.position() remains useful on other event paths.
        let position = event.position().or_else(|| {
            let surface = event.surface()?;
            let device = event.device()?;
            surface.device_position(&device).map(|(x, y, _)| (x, y))
        });
        let (Some(bounds), Some((x, y))) = (anchor.compute_bounds(&root), position) else {
            return gtk4::glib::Propagation::Proceed;
        };
        if !anchor.is_mapped() || view.window().is_none() {
            return gtk4::glib::Propagation::Proceed;
        }
        let local = NSPoint::new(x - f64::from(bounds.x()), y - f64::from(bounds.y()));
        let local = if view.isFlipped() {
            local
        } else {
            NSPoint::new(local.x, view.bounds().size.height - local.y)
        };
        // NSView::hitTest takes coordinates in the receiver's superview.
        let parent = unsafe { view.superview() };
        let hit = view.convertPoint_toView(local, parent.as_deref());
        let Some(target) = view.hitTest(hit) else {
            return gtk4::glib::Propagation::Proceed;
        };
        let (dx, dy) = match scroll.direction() {
            gtk4::gdk::ScrollDirection::Up => (0.0, -1.0),
            gtk4::gdk::ScrollDirection::Down => (0.0, 1.0),
            gtk4::gdk::ScrollDirection::Left => (-1.0, 0.0),
            gtk4::gdk::ScrollDirection::Right => (1.0, 0.0),
            _ => scroll.deltas(),
        };
        let precise = scroll.unit() == gtk4::gdk::ScrollUnit::Surface;
        let units = if precise {
            CGScrollEventUnit::Pixel
        } else {
            CGScrollEventUnit::Line
        };
        let Some(cg) = CGEvent::new_scroll_wheel_event2(None, units, 2, -dy as i32, -dx as i32, 0)
        else {
            return gtk4::glib::Propagation::Proceed;
        };
        // Keep subpixel deltas and both axes. GDK's direction is the opposite of Cocoa's.
        for (point, fixed, delta) in [
            (
                CGEventField::ScrollWheelEventPointDeltaAxis1,
                CGEventField::ScrollWheelEventFixedPtDeltaAxis1,
                -dy,
            ),
            (
                CGEventField::ScrollWheelEventPointDeltaAxis2,
                CGEventField::ScrollWheelEventFixedPtDeltaAxis2,
                -dx,
            ),
        ] {
            CGEvent::set_double_value_field(Some(&cg), fixed, delta);
            if precise {
                CGEvent::set_double_value_field(Some(&cg), point, delta);
            }
        }
        if precise {
            let phase = if scroll.is_stop() {
                gesture.set(false);
                4
            } else if gesture.replace(true) {
                2
            } else {
                1
            };
            CGEvent::set_integer_value_field(
                Some(&cg),
                CGEventField::ScrollWheelEventScrollPhase,
                phase,
            );
        }
        let mut flags = CGEventFlags::empty();
        for (gdk, cocoa) in [
            (gtk4::gdk::ModifierType::SHIFT_MASK, CGEventFlags::MaskShift),
            (
                gtk4::gdk::ModifierType::CONTROL_MASK,
                CGEventFlags::MaskControl,
            ),
            (
                gtk4::gdk::ModifierType::ALT_MASK,
                CGEventFlags::MaskAlternate,
            ),
            (
                gtk4::gdk::ModifierType::META_MASK,
                CGEventFlags::MaskCommand,
            ),
        ] {
            if event.modifier_state().contains(gdk) {
                flags |= cocoa;
            }
        }
        CGEvent::set_flags(Some(&cg), flags);
        let mtm = MainThreadMarker::new().expect("GTK main thread");
        let point = view.convertPoint_toView(local, None);
        // eventWithCGEvent creates a windowless event. Its locationInWindow must
        // nevertheless be this view's window coordinates for WebKit's hit testing.
        let screens = NSScreen::screens(mtm);
        let screen_height = screens.firstObject().map_or(0.0, |s| s.frame().size.height);
        CGEvent::set_location(
            Some(&cg),
            objc2_core_foundation::CGPoint::new(point.x, screen_height - point.y),
        );
        if let Some(native) = NSEvent::eventWithCGEvent(&cg) {
            target.scrollWheel(&native);
            gtk4::glib::Propagation::Stop
        } else {
            gtk4::glib::Propagation::Proceed
        }
    });
    // The widget owns the controller; the weak anchor prevents a GTK reference cycle.
    anchor.add_controller(controller);
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
