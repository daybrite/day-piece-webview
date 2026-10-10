// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
use super::windows_web::Host;
use super::*;
use day_gtk::Gtk;
use day_spec::NodeId;
use gtk4::{glib::translate::ToGlibPtr, prelude::*};
unsafe extern "C" {
    fn gdk_win32_surface_get_handle(surface: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}
type Live = Rc<RefCell<Option<Rc<Host>>>>;
day_core::tls_group! {static VIEWS: RefCell<HashMap<usize,Live>>=RefCell::new(HashMap::new());}
fn make(_: &mut Gtk, p: &WebProps, id: NodeId) -> gtk4::Widget {
    let anchor = gtk4::DrawingArea::new();
    anchor.set_widget_name(&format!("day-webview-{}", id.0));
    let live: Live = Rc::new(RefCell::new(None));
    VIEWS.with(|m| {
        m.borrow_mut()
            .insert(anchor.as_ptr() as usize, live.clone())
    });
    let (url, prefix) = if p.inline_root.is_empty() {
        (
            p.url.clone(),
            p.resources
                .as_ref()
                .map(ResourceProvider::base_url)
                .unwrap_or_default(),
        )
    } else {
        match super::gtk_assets::extract_site(&p.inline_root) {
            Ok(dir) => {
                let root = url::Url::from_directory_path(dir).unwrap();
                (root.join(&p.inline_start).unwrap().into(), root.into())
            }
            Err(e) => {
                log::error!("webview assets: {e}");
                ("about:blank".into(), String::new())
            }
        }
    };
    let profile = p.profile.clone();
    let tabs = p.tabs.is_some();
    let state = live.clone();
    anchor.add_tick_callback(move |w, _| {
        let Some(root) = w.root().and_then(|r| r.dynamic_cast::<gtk4::Widget>().ok()) else {
            return gtk4::glib::ControlFlow::Continue;
        };
        if let (Some(rect), Some(surface)) = (
            w.compute_bounds(&root),
            w.native().and_then(|n| n.surface()),
        ) {
            if state.borrow().is_none() {
                let ptr: *mut gtk4::gdk::ffi::GdkSurface = surface.to_glib_none().0;
                let hwnd = unsafe { gdk_win32_surface_get_handle(ptr.cast()) };
                match Host::new(
                    hwnd as isize,
                    url.clone(),
                    prefix.clone(),
                    id,
                    day_gtk::emit,
                    profile.clone(),
                    tabs,
                ) {
                    Ok(host) => *state.borrow_mut() = Some(Rc::new(host)),
                    Err(e) => {
                        log::error!("WebView2: {e}");
                        return gtk4::glib::ControlFlow::Break;
                    }
                }
            }
            let host = state.borrow().clone();
            if let Some(host) = host {
                host.bounds(
                    rect.x() as f64,
                    rect.y() as f64,
                    rect.width() as f64,
                    rect.height() as f64,
                );
                let _ = host.view.set_visible(true);
            }
        }
        gtk4::glib::ControlFlow::Continue
    });
    anchor.connect_unmap(move |_| {
        if let Some(host) = live.borrow().as_ref() {
            let _ = host.view.set_visible(false);
        }
    });
    anchor.upcast()
}
fn update(_: &mut Gtk, w: &gtk4::Widget, p: &WebPatch) {
    let live = VIEWS.with(|m| m.borrow().get(&(w.as_ptr() as usize)).cloned());
    let host = live.and_then(|l| l.borrow().clone());
    if let Some(host) = host {
        host.patch(p)
    } else if let WebPatch::Eval { req, .. } = p {
        if let Some(id) = w
            .widget_name()
            .strip_prefix("day-webview-")
            .and_then(|s| s.parse().ok())
        {
            day_gtk::emit(
                NodeId(id),
                Event::Custom {
                    tag: "webview:eval",
                    num: *req as f64,
                    text: engine_error(
                        "WebView2",
                        "View not ready; check WebView2 Runtime installation",
                    ),
                },
            );
        }
    }
}
fn release(_: &mut Gtk, w: &gtk4::Widget) {
    if let Some(live) = VIEWS.with(|m| m.borrow_mut().remove(&(w.as_ptr() as usize))) {
        live.borrow_mut().take();
    }
}
day_pieces::renderer!(day_gtk::RENDERERS,Gtk,kind:KIND,props:WebProps,patch:WebPatch,make:make,update:update,measure:day_pieces::fill_measure,release:release);
