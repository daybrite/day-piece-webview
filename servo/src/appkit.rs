// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! Experimental in-process Servo host. AppKit owns geometry and input; Servo owns the surface.
use day_piece_webview::renderer::{
    background, data_request, engine_error, external_link_event, load_event, new_tab_event,
    reply_event, resource_response, success_json,
};
use day_piece_webview::{ResourceResponse, WebPatch, WebProps};
use day_spec::Event;
use day_spec::NodeId;
use dispatch2::DispatchQueue;
use dpi::PhysicalSize;
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSTrackingArea, NSView};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{AppKitWindowHandle, DisplayHandle, RawWindowHandle, WindowHandle};
use servo::{EventLoopWaker, InputEvent, Servo, ServoBuilder, WebViewBuilder};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

thread_local! {
    static LOADS: RefCell<HashMap<u64, (NodeId, servo::WebResourceLoad)>> = RefCell::new(HashMap::new());
    static NEXT_LOAD: Cell<u64> = const { Cell::new(1) };
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
    static HOSTS: RefCell<HashMap<usize, Retained<ServoView>>> = RefCell::new(HashMap::new());
    static SESSIONS: RefCell<HashMap<u64, Retained<ServoView>>> = RefCell::new(HashMap::new());
}
static WAKE_PENDING: AtomicBool = AtomicBool::new(false);
#[derive(Clone)]
struct Waker;
impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        if !WAKE_PENDING.swap(true, Ordering::AcqRel) {
            DispatchQueue::main().exec_async(|| {
                WAKE_PENDING.store(false, Ordering::Release);
                pump();
            });
        }
    }
}
struct Engine {
    servo: Servo,
    // Keep the temporary storage directory alive until the engine is dropped.
    _storage: tempfile::TempDir,
}
fn engine() -> Result<Servo, String> {
    ENGINE.with(|slot| {
        if let Some(engine) = slot.borrow().as_ref() {
            return Ok(engine.servo.clone());
        }
        // Another native browser may already have installed the process-wide provider.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let storage = tempfile::Builder::new()
            .prefix("day-servo-")
            .tempdir()
            .map_err(|error| error.to_string())?;
        let engine = ServoBuilder::default()
            .opts(servo::Opts {
                config_dir: Some(storage.path().to_owned()),
                ..Default::default()
            })
            .event_loop_waker(Box::new(Waker))
            .build();
        *slot.borrow_mut() = Some(Engine {
            servo: engine.clone(),
            _storage: storage,
        });
        Ok(engine)
    })
}
fn pump() {
    let engine = ENGINE.with(|s| s.borrow().as_ref().map(|engine| engine.servo.clone()));
    if let Some(engine) = engine {
        engine.spin_event_loop();
    }
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
struct State {
    node: Cell<NodeId>,
    dirty: Cell<bool>,
    error: RefCell<Option<String>>,
    props: WebProps,
    allowed_url: RefCell<String>,
}
impl State {
    fn report(&self, event: Event) {
        day_appkit::emit(self.node.get(), event);
    }
    fn external_url(&self, url: &str) -> String {
        if let Some(p) = &self.props.resources
            && let Some(path) =
                url.strip_prefix(&format!("https://day-resource.invalid/{}/", p.id()))
        {
            return format!("{}{path}", p.base_url());
        }
        url.to_owned()
    }
    fn internal_url(&self, url: &str) -> String {
        if let Some(p) = &self.props.resources
            && let Some(path) = url.strip_prefix(&p.base_url())
        {
            return format!("https://day-resource.invalid/{}/{path}", p.id());
        }
        url.to_owned()
    }
}
impl servo::WebViewDelegate for State {
    fn notify_new_frame_ready(&self, _: servo::WebView) {
        self.dirty.set(true);
        Waker.wake();
    }
    fn notify_load_status_changed(&self, web: servo::WebView, status: servo::LoadStatus) {
        if status == servo::LoadStatus::Complete
            && let Some(url) = web.url()
        {
            self.report(load_event(self.external_url(url.as_str())));
        }
    }
    fn notify_crashed(&self, _: servo::WebView, reason: String, _: Option<String>) {
        log::error!("Servo view crashed: {reason}");
        *self.error.borrow_mut() = Some(reason);
    }
    fn request_navigation(&self, _: servo::WebView, request: servo::NavigationRequest) {
        let url = request.url.as_str();
        let permitted = self.allowed_url.borrow().as_str() == url;
        let local =
            self.props.resources.as_ref().is_some_and(|p| {
                url.starts_with(&format!("https://day-resource.invalid/{}/", p.id()))
            }) || (!self.props.inline_root.is_empty()
                && day_spec::resolve_asset_dir(&self.props.inline_root).is_some_and(|root| {
                    request
                        .url
                        .to_file_path()
                        .ok()
                        .and_then(|path| path.canonicalize().ok())
                        .zip(root.canonicalize().ok())
                        .is_some_and(|(path, root)| path.starts_with(root))
                }));
        if permitted
            || local
            || (!self.props.external_links
                && matches!(request.url.scheme(), "http" | "https" | "about"))
        {
            request.allow();
        } else {
            self.report(external_link_event(self.external_url(url)));
            request.deny();
        }
    }
    fn load_web_resource(&self, _: servo::WebView, load: servo::WebResourceLoad) {
        let Some(provider) = &self.props.resources else {
            return;
        };
        if !load
            .request()
            .url
            .as_str()
            .starts_with(&format!("https://day-resource.invalid/{}/", provider.id()))
        {
            return;
        }
        let provider_id = provider.id();
        let url = self.external_url(load.request().url.as_str());
        let method = load.request().method.to_string();
        let range = load
            .request()
            .headers
            .get("range")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let id = NEXT_LOAD.with(|n| {
            let id = n.get();
            n.set(id + 1);
            id
        });
        LOADS.with(|loads| loads.borrow_mut().insert(id, (self.node.get(), load)));
        // Providers may do archive IO. Servo responders remain on the main thread.
        background(move || {
            let response = resource_response(provider_id, &url, &method, &range);
            DispatchQueue::main().exec_async(move || complete_resource(id, response));
        });
    }
}
fn complete_resource(id: u64, response: ResourceResponse) {
    let Some((_, load)) = LOADS.with(|loads| loads.borrow_mut().remove(&id)) else {
        return;
    };
    let mut reply = servo::WebResourceResponse::new(load.request().url.clone());
    if let Ok(mime) = response.mime.parse() {
        reply.headers.insert("content-type", mime);
    }
    if let Ok(status) = response.status.try_into() {
        reply.status_code = status;
    }
    for (name, value) in &response.headers {
        if let (Ok(name), Ok(value)) = (name.parse::<http::header::HeaderName>(), value.parse()) {
            reply.headers.append(name, value);
        }
    }
    let mut intercepted = load.intercept(reply);
    intercepted.send_body_data(response.body.to_vec());
    intercepted.finish();
    Waker.wake();
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
        let p = &self.ivars().state.props;
        let url = if !p.inline_root.is_empty() {
            day_spec::resolve_asset_dir(&p.inline_root)
                .and_then(|dir| url::Url::from_file_path(dir.join(&p.inline_start)).ok())
        } else {
            url::Url::parse(&self.ivars().state.internal_url(&p.url)).ok()
        };
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
    let state = Rc::new(State {
        node: Cell::new(node),
        dirty: Cell::new(false),
        error: RefCell::new(None),
        props: p.clone(),
        allowed_url: RefCell::new(String::new()),
    });
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
    host.with_web(|web| match patch {
        WebPatch::Load(url) => {
            if let Ok(url) = url::Url::parse(&state.internal_url(url)) {
                *state.allowed_url.borrow_mut() = url.to_string(); web.load(url);
            }
        }
        WebPatch::Back => { web.go_back(1); }
        WebPatch::Forward => { web.go_forward(1); }
        WebPatch::Reload | WebPatch::ForceReload => web.reload(),
        WebPatch::Stop => { /* Servo 0.6 has no public stop-loading API. */ }
        WebPatch::Eval { req, script } => {
            let req = *req;
            if let Some(request) = data_request(script) {
                let reply = if request["operation"] == "navigation" {
                    success_json(serde_json::json!({
                        "url": web.url().map(|u| state.external_url(u.as_str())).unwrap_or_default(),
                        "title": web.page_title().unwrap_or_default(), "can_go_back": web.can_go_back(),
                        "can_go_forward": web.can_go_forward(), "loading": web.load_status() != servo::LoadStatus::Complete,
                    }))
                } else { engine_error("Unsupported", "Servo profile operation unavailable") };
                state.report(reply_event(req, reply));
            } else {
                let state = state.clone();
                web.evaluate_javascript(script, move |result| {
                    let reply = match result {
                        Ok(servo::JSValue::String(s)) => s,
                        result => engine_error("ServoError", &format!("{result:?}")),
                    };
                    state.report(reply_event(req, reply));
                });
            }
        }
    });
    true
}
pub(super) fn release(h: &Retained<NSView>) -> bool {
    let Some(host) = h.downcast_ref::<ServoView>() else {
        return false;
    };
    HOSTS.with(|s| s.borrow_mut().remove(&(host as *const ServoView as usize)));
    if host.ivars().state.props.session == 0 {
        LOADS.with(|loads| {
            loads
                .borrow_mut()
                .retain(|_, (node, _)| *node != host.ivars().state.node.get())
        });
        host.ivars().web.borrow_mut().take();
    }
    Waker.wake();
    true
}
