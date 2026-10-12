// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! Experimental adapter contract. Engines depend on this crate and register a host before
//! constructing views. The native piece never depends on an engine adapter.
use crate::{ResourceResponse, WebRenderer};
use day_spec::{Event, Support};

/// Storage capabilities advertised by a registered engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Isolated private browsing contexts.
    pub private_browsing: Support,
    /// Named storage profiles.
    pub profiles: Support,
    /// Deleting all data in a profile.
    pub clear_data: Support,
}

/// Registration failed without changing the active renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationError {
    /// This build does not include a supported host and engine.
    Unsupported,
    /// Registration must run on the UI thread.
    WrongThread,
    /// Another adapter already registered this renderer.
    AlreadyRegistered,
    /// The toolkit-native renderer cannot be replaced.
    ReservedRenderer,
}
impl std::fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "renderer host is unavailable in this build",
            Self::WrongThread => "register the renderer on the UI thread",
            Self::AlreadyRegistered => "renderer already registered by another adapter",
            Self::ReservedRenderer => "the native renderer cannot be replaced",
        })
    }
}
impl std::error::Error for RegistrationError {}

pub(crate) fn capabilities(renderer: WebRenderer) -> Option<Capabilities> {
    if let Some(host) = surface::get(renderer) {
        return Some(host.capabilities);
    }
    #[cfg(all(feature = "appkit", target_os = "macos"))]
    return appkit::get(renderer).map(|host| host.capabilities);
    #[cfg(not(all(feature = "appkit", target_os = "macos")))]
    {
        let _ = renderer;
        None
    }
}

/// Toolkit-independent offscreen hosts. Day supplies a canvas; the webview front end
/// retains its command/event contract.
pub mod surface {
    use super::{Capabilities, RegistrationError, WebRenderer};
    use crate::{WebPatch, WebProps};
    use day_core::{BuildCx, RNode};
    use std::{cell::RefCell, collections::HashSet, sync::OnceLock};

    /// Offscreen engines draw into Day's canvas. Evaluation is enabled only on owned nodes.
    pub const KIND: day_spec::PieceKind = day_spec::kinds::CANVAS;

    /// UI-thread callbacks. The adapter must preserve navigation, evaluation, resource,
    /// and session behavior when attaching a canvas node.
    pub struct Host {
        /// Engine storage capabilities.
        pub capabilities: Capabilities,
        /// Attach engine state and input handlers after the canvas node has been created.
        pub attach: fn(&mut BuildCx, RNode, &WebProps),
        /// Apply a command to an attached node.
        pub update: fn(RNode, &WebPatch),
        /// Detach engine state before the node's reactive scope is disposed.
        pub release: fn(RNode),
    }
    static SERVO: OnceLock<&'static Host> = OnceLock::new();
    thread_local! {
        static NODES: RefCell<HashSet<RNode>> = RefCell::new(HashSet::new());
    }

    /// Register on the running Day UI thread, before building webviews. Re-registering the
    /// same descriptor is harmless; replacing an active renderer is rejected.
    pub fn register(renderer: WebRenderer, host: &'static Host) -> Result<(), RegistrationError> {
        if !day_core::has_tree() {
            return Err(RegistrationError::WrongThread);
        }
        if renderer == WebRenderer::WebView {
            return Err(RegistrationError::ReservedRenderer);
        }
        let installed = SERVO.get_or_init(|| host);
        if std::ptr::eq(*installed, host) {
            Ok(())
        } else {
            Err(RegistrationError::AlreadyRegistered)
        }
    }

    pub(crate) fn get(renderer: WebRenderer) -> Option<&'static Host> {
        match renderer {
            WebRenderer::WebView => None,
            WebRenderer::Servo => SERVO.get().copied(),
        }
    }
    pub(crate) fn attach(host: &'static Host, cx: &mut BuildCx, node: RNode, props: &WebProps) {
        NODES.with(|nodes| nodes.borrow_mut().insert(node));
        day_reactive::Scope::current().on_cleanup(move || {
            NODES.with(|nodes| nodes.borrow_mut().remove(&node));
            (host.release)(node);
        });
        (host.attach)(cx, node, props);
    }
    pub(crate) fn owns(node: RNode) -> bool {
        NODES.with(|nodes| nodes.borrow().contains(&node))
    }
    pub(crate) fn send(node: RNode, patch: &WebPatch) -> bool {
        if !owns(node) {
            return false;
        }
        if let Some(host) = SERVO.get() {
            (host.update)(node, patch);
        }
        true
    }
}

pub(crate) fn send_patch(node: day_core::RNode, patch: crate::WebPatch) {
    if !surface::send(node, &patch) {
        day_core::with_tree(|tree| tree.patch(node, Box::new(patch), false));
    }
}

/// AppKit host registration. Registering a host does not initialize its engine.
#[cfg(all(feature = "appkit", target_os = "macos"))]
pub mod appkit {
    use super::{Capabilities, RegistrationError, WebRenderer};
    use crate::{WebPatch, WebProps};
    use day_spec::NodeId;
    use objc2::{MainThreadMarker, rc::Retained};
    use objc2_app_kit::NSView;
    use std::sync::OnceLock;

    /// Callbacks execute on AppKit's main thread. Hosts must preserve the piece's event,
    /// resource, profile, and retained-session contracts; see `docs/servo.md`.
    pub struct Host {
        /// Engine capabilities, independent of the toolkit's native web view.
        pub capabilities: Capabilities,
        /// Construct or reattach a view. Emit events to the supplied node.
        pub make: fn(MainThreadMarker, &WebProps, NodeId) -> Retained<NSView>,
        /// Apply a patch, returning false without side effects for views owned by another host.
        pub update: fn(&Retained<NSView>, &WebPatch) -> bool,
        /// Detach a view, returning false without side effects for views owned by another host.
        /// Retained sessions survive detachment; ordinary views dispose their engine context.
        pub release: fn(&Retained<NSView>) -> bool,
    }
    static SERVO: OnceLock<&'static Host> = OnceLock::new();

    fn install(
        slot: &OnceLock<&'static Host>,
        host: &'static Host,
    ) -> Result<(), RegistrationError> {
        let installed = slot.get_or_init(|| host);
        if std::ptr::eq(*installed, host) {
            Ok(())
        } else {
            Err(RegistrationError::AlreadyRegistered)
        }
    }

    /// Register once before creating views. Re-registering the same static host is harmless;
    /// replacing it is rejected so existing views retain their dispatch and capability contract.
    pub fn register(
        _mtm: MainThreadMarker,
        renderer: WebRenderer,
        host: &'static Host,
    ) -> Result<(), RegistrationError> {
        match renderer {
            WebRenderer::WebView => Err(RegistrationError::ReservedRenderer),
            WebRenderer::Servo => install(&SERVO, host),
        }
    }

    pub(crate) fn get(renderer: WebRenderer) -> Option<&'static Host> {
        match renderer {
            WebRenderer::WebView => None,
            WebRenderer::Servo => SERVO.get().copied(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        const CAPS: Capabilities = Capabilities {
            private_browsing: day_spec::Support::Unsupported,
            profiles: day_spec::Support::Unsupported,
            clear_data: day_spec::Support::Unsupported,
        };
        fn unused_make(_: MainThreadMarker, _: &WebProps, _: NodeId) -> Retained<NSView> {
            panic!("registration must not create views or initialize engines")
        }
        static FIRST: Host = Host {
            capabilities: CAPS,
            make: unused_make,
            update: |_, _| false,
            release: |_| false,
        };
        static SECOND: Host = Host {
            capabilities: Capabilities {
                profiles: day_spec::Support::Emulated,
                ..CAPS
            },
            ..FIRST
        };

        #[test]
        fn registration_is_idempotent_and_preserves_the_original_host() {
            let slot = OnceLock::new();
            assert!(slot.get().is_none());
            assert_eq!(install(&slot, &FIRST), Ok(()));
            assert_eq!(install(&slot, &FIRST), Ok(()));
            assert_eq!(
                install(&slot, &SECOND),
                Err(RegistrationError::AlreadyRegistered)
            );
            assert!(std::ptr::eq(*slot.get().unwrap(), &FIRST));
            assert_eq!(slot.get().unwrap().capabilities, CAPS);
        }
    }
}

/// Report a completed top-level page load.
pub fn load_event(url: String) -> Event {
    event(0.0, url)
}
/// Ask the application's external-link handler to decide a navigation.
pub fn external_link_event(url: String) -> Event {
    event(crate::LINK_REPORT, url)
}
/// Complete an evaluation request using its encoded JavaScript or data reply.
pub fn reply_event(request: u64, reply: String) -> Event {
    event(request as f64, reply)
}
fn event(num: f64, text: String) -> Event {
    Event::Custom {
        tag: "webview:renderer",
        num,
        text,
    }
}
/// Request a new browsing context without replacing the current view.
pub fn new_tab_event(url: String, background: bool) -> Event {
    crate::new_tab_event(url, background)
}
/// Decode an internal navigation/profile request. Ordinary JavaScript returns `None`.
pub fn data_request(script: &str) -> Option<serde_json::Value> {
    script
        .strip_prefix(crate::DATA_REQUEST)
        .map(|request| serde_json::from_str(request).unwrap_or_default())
}
/// Encode a successful structured data response.
pub fn success_json(value: serde_json::Value) -> String {
    format!("1{}{value}", crate::SEP)
}
/// Encode an engine failure for the shared evaluation API.
pub fn engine_error(name: &str, message: &str) -> String {
    crate::engine_error(name, message)
}
/// Serve a registered resource provider, including shared path and range validation.
pub fn resource_response(id: u64, url: &str, method: &str, range: &str) -> ResourceResponse {
    crate::resources::respond(id, url, method, range)
}
/// Run resource IO on the piece's bounded worker pool. Marshal results back to the UI thread
/// before accessing a native responder or emitting events.
#[cfg(not(target_arch = "wasm32"))]
pub fn background(work: impl FnOnce() + Send + 'static) {
    crate::resources::background(work);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_replies_decode_through_the_shared_evaluation_protocol() {
        let value = serde_json::json!({"title": "fixture\u{1f}title", "loading": false});
        let Event::Custom { num, text, .. } = reply_event(42, success_json(value.clone())) else {
            panic!("expected evaluation event")
        };
        assert_eq!(num, 42.0);
        let decoded: serde_json::Value =
            serde_json::from_str(&crate::decode(&text).unwrap()).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(
            crate::decode(&engine_error("FixtureError", "fixture failure")),
            Err(crate::EvalError::Threw {
                name: "FixtureError".into(),
                message: "fixture failure".into()
            })
        );
        assert!(data_request(&crate::wrap_script("42")).is_none());
        let script = format!(
            "{}{}",
            crate::DATA_REQUEST,
            serde_json::json!({"operation":"navigation"})
        );
        assert_eq!(data_request(&script).unwrap()["operation"], "navigation");
    }
}
