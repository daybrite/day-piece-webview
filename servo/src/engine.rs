// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! Shared Servo engine, navigation, evaluation, and resource lifecycle for all hosts.
use day_piece_webview::renderer::{
    background, data_request, engine_error, external_link_event, load_event, reply_event,
    resource_response, success_json,
};
use day_piece_webview::{ResourceProvider, ResourceResponse, WebPatch, WebProps};
use day_spec::{Event, NodeId};
use servo::{EventLoopWaker, Servo, ServoBuilder};
// Servo itself skips its default resource reader on OpenHarmony (`not(target_env = "ohos")` in
// servo's lib.rs), leaving registration to the embedder; this import is what links the crate and
// its `submit_resource_reader!` constructor into the app (see servo/Cargo.toml).
#[cfg(target_env = "ohos")]
use servo_default_resources as _;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};
thread_local! {
    static LOADS: RefCell<HashMap<u64, (NodeId, servo::WebResourceLoad)>> = RefCell::new(HashMap::new());
    static NEXT_LOAD: Cell<u64> = const { Cell::new(1) };
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
}
static WAKE_PENDING: AtomicBool = AtomicBool::new(false);
#[derive(Clone)]
pub(crate) struct Waker;
impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        if !WAKE_PENDING.swap(true, Ordering::AcqRel) {
            day_reactive::on_main(|| {
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
pub(crate) fn engine() -> Result<Servo, String> {
    // Servo runs its constellation, script, layout, and network on threads of its own, and a
    // panic there reaches only the default hook's stderr, which an Android or OpenHarmony
    // process does not surface: the engine then merely reports "Lost connection to
    // Constellation". Route every panic's thread, message, and location through the logger
    // first, then let the previous hook (Day's, or std's) do what it did.
    static PANIC_HOOK: std::sync::Once = std::sync::Once::new();
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current();
            log::error!(
                "panic in thread {:?}: {info}",
                thread.name().unwrap_or("<unnamed>")
            );
            previous(info);
        }));
    });
    ENGINE.with(|slot| {
        if let Some(engine) = slot.borrow().as_ref() {
            return Ok(engine.servo.clone());
        }
        #[cfg(target_os = "android")]
        {
            // The verifier reaches the system trust store through the app Context, which the
            // Java host keeps on `DayBridge.ctx` (day-toolkit-material's generated binding).
            day_toolkit_material::with_env(|env| {
                let context = day_toolkit_material::DayBridge::ctx(env)?;
                rustls_platform_verifier::android::init_with_env(env, context)
            })
            .map_err(|error| format!("Android certificate verifier: {error}"))?;
        }
        // Another native browser may already have installed the process-wide provider.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let storage = tempfile::Builder::new()
            .prefix("day-servo-")
            .tempdir_in(day_spec::present::app_temp_dir())
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
    #[cfg(all(
        any(feature = "gtk", feature = "qt"),
        target_os = "linux",
        not(target_env = "ohos")
    ))]
    let _graphics = match crate::linux_gl::Guard::enter() {
        Ok(guard) => guard,
        Err(error) => {
            log::error!("Servo graphics binding: {error}");
            return;
        }
    };
    let engine = ENGINE.with(|s| s.borrow().as_ref().map(|engine| engine.servo.clone()));
    if let Some(engine) = engine {
        engine.spin_event_loop();
    }
    #[cfg(all(feature = "appkit", target_os = "macos"))]
    crate::appkit::paint_dirty();
    #[cfg(any(
        feature = "gtk",
        feature = "qt",
        feature = "xaml",
        feature = "material",
        feature = "arkui"
    ))]
    crate::surface::paint_dirty();
}
pub(crate) struct State {
    pub(crate) node: Cell<NodeId>,
    pub(crate) dirty: Cell<bool>,
    pub(crate) error: RefCell<Option<String>>,
    pub(crate) props: WebProps,
    pub(crate) allowed_url: RefCell<String>,
    inline_provider: Option<ResourceProvider>,
}
impl State {
    pub(crate) fn report(&self, event: Event) {
        let node = self.node.get();
        day_reactive::on_main(move || day_core::enqueue_event(node, event));
    }
    pub(crate) fn external_url(&self, url: &str) -> String {
        if let Some(p) = &self.props.resources
            && let Some(path) =
                url.strip_prefix(&format!("https://day-resource.invalid/{}/", p.id()))
        {
            return format!("{}{path}", p.base_url());
        }
        url.to_owned()
    }
    fn inline_prefix(&self) -> Option<String> {
        self.inline_provider.as_ref().map(|p| {
            format!(
                "https://day-assets.invalid/{}/{}/",
                p.id(),
                self.props.inline_root
            )
        })
    }
    pub(crate) fn internal_url(&self, url: &str) -> String {
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
        let local = self
            .inline_prefix()
            .is_some_and(|prefix| url.starts_with(&prefix))
            || self.props.resources.as_ref().is_some_and(|p| {
                url.starts_with(&format!("https://day-resource.invalid/{}/", p.id()))
            })
            || (!self.props.inline_root.is_empty()
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
        let requested_url = load.request().url.as_str();
        let resource = self
            .props
            .resources
            .as_ref()
            .filter(|provider| {
                requested_url
                    .starts_with(&format!("https://day-resource.invalid/{}/", provider.id()))
            })
            .map(|provider| (provider.id(), self.external_url(requested_url)));
        let inline = self
            .inline_provider
            .as_ref()
            .zip(self.inline_prefix())
            .and_then(|(provider, prefix)| {
                requested_url.strip_prefix(&prefix).map(|path| {
                    (
                        provider.id(),
                        format!("{}__day_assets/{path}", provider.base_url()),
                    )
                })
            });
        let Some((provider_id, url)) = resource.or(inline) else {
            return;
        };
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
            day_reactive::on_main(move || complete_resource(id, response));
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

pub(crate) fn state(props: &WebProps, node: NodeId) -> Rc<State> {
    Rc::new(State {
        node: Cell::new(node),
        dirty: Cell::new(false),
        error: RefCell::new(None),
        // Mobile bundles are not ordinary files. Reuse the piece's bounded resource IO
        // and path validation; this root already came from the app's generated AssetDir.
        inline_provider: if !props.inline_root.is_empty()
            && day_spec::resolve_asset_dir(&props.inline_root).is_none()
        {
            Some(ResourceProvider::with_site(
                day_spec::AssetDir::dynamic(props.inline_root.clone()),
                |_| ResourceResponse::not_found(),
            ))
        } else {
            None
        },
        props: props.clone(),
        allowed_url: RefCell::new(String::new()),
    })
}
pub(crate) fn cancel_loads(node: NodeId) {
    LOADS.with(|loads| loads.borrow_mut().retain(|_, (owner, _)| *owner != node));
}
pub(crate) fn initial_url(state: &State) -> Option<url::Url> {
    let p = &state.props;
    if let Some(prefix) = state.inline_prefix() {
        url::Url::parse(&prefix).ok()?.join(&p.inline_start).ok()
    } else if !p.inline_root.is_empty() {
        day_spec::resolve_asset_dir(&p.inline_root)
            .and_then(|dir| url::Url::from_file_path(dir.join(&p.inline_start)).ok())
    } else {
        url::Url::parse(&state.internal_url(if p.url.is_empty() {
            "about:blank"
        } else {
            &p.url
        }))
        .ok()
    }
}
pub(crate) fn update(state: &Rc<State>, web: &servo::WebView, patch: &WebPatch) {
    match patch {
        WebPatch::Load(url) => {
            if let Ok(url) = url::Url::parse(&state.internal_url(url)) {
                *state.allowed_url.borrow_mut() = url.to_string();
                web.load(url);
            }
        }
        WebPatch::Back => {
            web.go_back(1);
        }
        WebPatch::Forward => {
            web.go_forward(1);
        }
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
                } else {
                    engine_error("Unsupported", "Servo profile operation unavailable")
                };
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
    }
}
