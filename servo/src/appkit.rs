// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! Experimental in-process Servo host. AppKit owns geometry and input; Servo owns the surface.
use day_piece_webview::renderer::{engine_error, new_tab_event, reply_event};
use day_piece_webview::{WebPatch, WebProps};
use day_spec::NodeId;
use dpi::PhysicalSize;
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSTrackingArea, NSView};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{AppKitWindowHandle, DisplayHandle, RawWindowHandle, WindowHandle};
use servo::{EventLoopWaker, InputEvent, WebViewBuilder};
use std::ptr::NonNull;
use std::{cell::RefCell, collections::HashMap, rc::Rc};

use crate::engine::{self, State, Waker, engine};
thread_local! {
    static HOSTS: RefCell<HashMap<usize, Retained<ServoView>>> = RefCell::new(HashMap::new());
    static SESSIONS: RefCell<HashMap<u64, Retained<ServoView>>> = RefCell::new(HashMap::new());
}
pub(crate) fn paint_dirty() {
    let hosts: Vec<_> = HOSTS.with(|s| s.borrow().values().cloned().collect());
    for host in hosts {
        if !host.ivars().state.dirty.replace(false) {
            continue;
        }
        if let Some(web) = host.ivars().web.borrow().as_ref() {
            web.paint();
            web.rendering_context().present();
        }
    }
}
struct Ivars {
    state: Rc<State>,
    web: RefCell<Option<servo::WebView>>,
    tracking: RefCell<Option<Retained<NSTrackingArea>>>,
}
define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "DayServoWebView"]
    #[ivars = Ivars]
    struct ServoView;
    impl ServoView {
        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking(&self) {
            // SAFETY: forwards NSView's tracking lifecycle, and the area belongs to this view.
            unsafe {
                let _: () = msg_send![super(self), updateTrackingAreas];
                if let Some(area) = self.ivars().tracking.borrow_mut().take() { self.removeTrackingArea(&area); }
                let options = objc2_app_kit::NSTrackingAreaOptions::MouseMoved
                    | objc2_app_kit::NSTrackingAreaOptions::ActiveInKeyWindow
                    | objc2_app_kit::NSTrackingAreaOptions::InVisibleRect;
                let area = NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(), self.bounds(), options, Some(self), None);
                self.addTrackingArea(&area);
                *self.ivars().tracking.borrow_mut() = Some(area);
            }
        }
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool { true }
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { true }
        #[unsafe(method(viewDidMoveToWindow))]
        fn moved(&self) {
            if self.window().is_some() { self.initialize(); self.resize(); }
        }
        #[unsafe(method(setFrameSize:))]
        fn frame_size(&self, size: NSSize) {
            // SAFETY: forwarding NSView's geometry setter to its superclass.
            unsafe { let _: () = msg_send![super(self), setFrameSize: size]; }
            self.resize();
        }
        #[unsafe(method(viewDidChangeBackingProperties))]
        fn backing_changed(&self) { self.resize(); }
        #[unsafe(method(becomeFirstResponder))]
        fn focus(&self) -> bool {
            self.with_web(|w| w.focus()); true
        }
        #[unsafe(method(resignFirstResponder))]
        fn blur(&self) -> bool {
            self.with_web(|w| w.blur()); true
        }
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) { self.mouse_button(event, servo::MouseButtonAction::Down, servo::MouseButton::Primary); }
        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) { self.mouse_button(event, servo::MouseButtonAction::Up, servo::MouseButton::Primary); }
        #[unsafe(method(rightMouseDown:))]
        fn right_down(&self, event: &NSEvent) { self.mouse_button(event, servo::MouseButtonAction::Down, servo::MouseButton::Secondary); }
        #[unsafe(method(rightMouseUp:))]
        fn right_up(&self, event: &NSEvent) { self.mouse_button(event, servo::MouseButtonAction::Up, servo::MouseButton::Secondary); }
        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.with_web(|w| { w.notify_input_event(InputEvent::MouseMove(servo::MouseMoveEvent::new(self.point(event)))); });
        }
        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) { self.mouseMoved(event); }
        #[unsafe(method(scrollWheel:))]
        fn wheel(&self, event: &NSEvent) {
            self.with_web(|w| { w.notify_input_event(InputEvent::Wheel(servo::WheelEvent::new(servo::WheelDelta {
                x: event.scrollingDeltaX(), y: event.scrollingDeltaY(), z: 0.0,
                mode: if event.hasPreciseScrollingDeltas() { servo::WheelMode::DeltaPixel } else { servo::WheelMode::DeltaLine },
            }, self.point(event)))); });
        }
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) { self.key(event, servo::KeyState::Down); }
        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) { self.key(event, servo::KeyState::Up); }
    }
);
impl ServoView {
    fn scale(&self) -> f64 {
        self.window().map(|w| w.backingScaleFactor()).unwrap_or(1.0)
    }
    fn size(&self) -> PhysicalSize<u32> {
        let size = self.bounds().size;
        PhysicalSize::new(
            (size.width * self.scale()).max(1.0) as u32,
            (size.height * self.scale()).max(1.0) as u32,
        )
    }
    fn initialize(&self) {
        if self.ivars().web.borrow().is_some() || self.ivars().state.error.borrow().is_some() {
            return;
        }
        // SAFETY: the main-thread NSView outlives its rendering context, which is dropped on release.
        let handle = unsafe {
            WindowHandle::borrow_raw(RawWindowHandle::AppKit(AppKitWindowHandle::new(
                NonNull::from(self).cast(),
            )))
        };
        let context = match servo::WindowRenderingContext::new(
            DisplayHandle::appkit(),
            handle,
            self.size(),
        ) {
            Ok(context) => Rc::new(context),
            Err(error) => {
                log::error!("Servo surface creation failed: {error:?}");
                *self.ivars().state.error.borrow_mut() = Some(format!("{error:?}"));
                return;
            }
        };
        let url = engine::initial_url(&self.ivars().state);
        let Some(url) = url else {
            return;
        };
        *self.ivars().state.allowed_url.borrow_mut() = url.to_string();
        let engine = match engine() {
            Ok(engine) => engine,
            Err(error) => {
                log::error!("Servo initialization failed: {error}");
                *self.ivars().state.error.borrow_mut() = Some(error);
                return;
            }
        };
        let web = WebViewBuilder::new(&engine, context)
            .delegate(self.ivars().state.clone())
            .url(url)
            .hidpi_scale_factor(euclid::Scale::new(self.scale() as f32))
            .build();
        *self.ivars().web.borrow_mut() = Some(web);
        Waker.wake();
    }
    fn resize(&self) {
        self.with_web(|w| {
            w.set_hidpi_scale_factor(euclid::Scale::new(self.scale() as f32));
            w.resize(self.size());
        });
    }
    fn with_web(&self, f: impl FnOnce(&servo::WebView)) {
        if let Some(web) = self.ivars().web.borrow().as_ref() {
            f(web);
            Waker.wake();
        }
    }
    fn point(&self, event: &NSEvent) -> servo::WebViewPoint {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        servo::DevicePoint::new((p.x * self.scale()) as f32, (p.y * self.scale()) as f32).into()
    }
    fn mouse_button(
        &self,
        event: &NSEvent,
        action: servo::MouseButtonAction,
        button: servo::MouseButton,
    ) {
        if self.ivars().state.props.tabs.is_some()
            && event
                .modifierFlags()
                .contains(NSEventModifierFlags::Command)
        {
            if action == servo::MouseButtonAction::Down {
                let point = self.convertPoint_fromView(event.locationInWindow(), None);
                let state = self.ivars().state.clone();
                let background = !event.modifierFlags().contains(NSEventModifierFlags::Shift);
                self.with_web(|web| {
                    let zoom = f64::from(web.page_zoom());
                    let script = format!(
                        "document.elementFromPoint({}, {})?.closest('a[href]')?.href || ''",
                        point.x / zoom,
                        point.y / zoom
                    );
                    web.evaluate_javascript(script, move |reply| {
                        if let Ok(servo::JSValue::String(url)) = reply
                            && !url.is_empty()
                        {
                            day_appkit::emit(
                                state.node.get(),
                                new_tab_event(state.external_url(&url), background),
                            );
                        }
                    });
                });
            }
            return;
        }
        if let Some(window) = self.window() {
            window.makeFirstResponder(Some(self));
        }
        self.with_web(|w| {
            w.notify_input_event(InputEvent::MouseButton(servo::MouseButtonEvent::new(
                action,
                button,
                self.point(event),
            )));
        });
    }
    fn key(&self, event: &NSEvent, state: servo::KeyState) {
        let named = match event.keyCode() {
            36 => Some(servo::NamedKey::Enter),
            48 => Some(servo::NamedKey::Tab),
            51 => Some(servo::NamedKey::Backspace),
            53 => Some(servo::NamedKey::Escape),
            117 => Some(servo::NamedKey::Delete),
            123 => Some(servo::NamedKey::ArrowLeft),
            124 => Some(servo::NamedKey::ArrowRight),
            125 => Some(servo::NamedKey::ArrowDown),
            126 => Some(servo::NamedKey::ArrowUp),
            _ => None,
        };
        let key = named.map(servo::Key::Named).unwrap_or_else(|| {
            servo::Key::Character(
                event
                    .characters()
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
            )
        });
        let flags = event.modifierFlags();
        let mut modifiers = servo::Modifiers::empty();
        for (native, flag) in [
            (NSEventModifierFlags::Shift, servo::Modifiers::SHIFT),
            (NSEventModifierFlags::Control, servo::Modifiers::CONTROL),
            (NSEventModifierFlags::Option, servo::Modifiers::ALT),
            (NSEventModifierFlags::Command, servo::Modifiers::META),
        ] {
            if flags.contains(native) {
                modifiers |= flag;
            }
        }
        self.with_web(|w| {
            w.notify_input_event(InputEvent::Keyboard(
                servo::KeyboardEvent::new_without_event(
                    state,
                    key,
                    servo::Code::Unidentified,
                    servo::Location::Standard,
                    modifiers,
                    event.isARepeat(),
                    false,
                ),
            ));
        });
    }
}
pub(super) fn make(mtm: MainThreadMarker, p: &WebProps, node: NodeId) -> Retained<NSView> {
    if p.session != 0
        && let Some(view) = SESSIONS.with(|s| s.borrow().get(&p.session).cloned())
    {
        view.ivars().state.node.set(node);
        HOSTS.with(|s| {
            s.borrow_mut()
                .insert(Retained::as_ptr(&view) as usize, view.clone())
        });
        return view.into_super();
    }
    let state = engine::state(p, node);
    let view = ServoView::alloc(mtm).set_ivars(Ivars {
        state,
        web: RefCell::new(None),
        tracking: RefCell::new(None),
    });
    // SAFETY: initializes the allocated NSView subclass with a nonempty initial frame.
    let view: Retained<ServoView> = unsafe {
        msg_send![super(view), initWithFrame: NSRect::new(NSPoint::ZERO, NSSize::new(1.0, 1.0))]
    };
    HOSTS.with(|s| {
        s.borrow_mut()
            .insert(Retained::as_ptr(&view) as usize, view.clone())
    });
    if p.session != 0 {
        SESSIONS.with(|s| s.borrow_mut().insert(p.session, view.clone()));
    }
    view.into_super()
}
pub(super) fn update(h: &Retained<NSView>, patch: &WebPatch) -> bool {
    let Some(host) = h.downcast_ref::<ServoView>() else {
        return false;
    };
    let state = &host.ivars().state;
    if let WebPatch::Eval { req, .. } = patch
        && let Some(error) = state.error.borrow().as_ref()
    {
        state.report(reply_event(*req, engine_error("ServoError", error)));
        return true;
    }
    host.with_web(|web| engine::update(state, web, patch));
    true
}
pub(super) fn release(h: &Retained<NSView>) -> bool {
    let Some(host) = h.downcast_ref::<ServoView>() else {
        return false;
    };
    HOSTS.with(|s| s.borrow_mut().remove(&(host as *const ServoView as usize)));
    if host.ivars().state.props.session == 0 {
        engine::cancel_loads(host.ivars().state.node.get());
        host.ivars().web.borrow_mut().take();
    }
    Waker.wake();
    true
}
