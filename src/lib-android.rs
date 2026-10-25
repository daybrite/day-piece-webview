// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// Android: android.webkit.WebView. The Java factory (`dev.daybrite.day.piece.webview.DayWebView`)
// is bundled with this crate in `src/DayWebView.java` and pulled into the app's Gradle build via
// `[package.metadata.day.android]`, which also contributes the INTERNET permission (the extension
// this piece motivates). The Java reports each finished URL back through DayBridge.nativeOnEvent's
// open Custom-event kind (12), §8.2's piece-defined event channel; the front-end handler maps the
// text payload to the bound URL.
// ---------------------------------------------------------------------------

use super::*;
use day_toolkit_material::jni::Env;
use day_toolkit_material::jni::objects::{JClass, JObject, JString};
use day_toolkit_material::{AHandle, Material, java, string_of};
use day_spec::NodeId;

// This piece's Java class (src/DayWebView.java, on the app classpath at build).
day_toolkit_material::java_class! {
    DayWebView => dev.daybrite.day.piece.webview.DayWebView,
    methods {
        static fn supportsProfiles() -> jboolean,
        static fn supportsDataClearing() -> jboolean,
        static fn makeWebView(id: jlong, url: JString, inline_prefix: JString, provider: jlong,
            profile: JString) -> android.view.View,
        static fn configureTabs(view: android.view.View, foreground: JString, background: JString),
        static fn setTransparent(view: android.view.View),
        static fn evalJs(view: android.view.View, req: jdouble, script: JString),
        static fn webCommand(view: android.view.View, code: jint, url: JString),
        static fn release(view: android.view.View),
        // Private in Java: only `resource`'s Rust half below calls it.
        static fn response(mime: JString, status: jint, header_lines: JString, body: jbyte[])
            -> android.webkit.WebResourceResponse,
    },
    native_methods {
        // Called by WebView's IO worker, never by the UI thread. A failure throws into Java,
        // where WebView answers the request as failed.
        static fn resource {
            sig = (provider: jlong, url: JString, method: JString, range: JString)
                -> android.webkit.WebResourceResponse,
            error_policy = day_toolkit_material::jni::errors::ThrowRuntimeExAndDefault,
        },
    },
}

/// One of DayWebView's capability probes (`supportsProfiles`, `supportsDataClearing`).
pub(crate) fn capability(method: &str) -> day_spec::Support {
    let supported = java(|env| match method {
        "supportsProfiles" => DayWebView::supportsProfiles(env),
        _ => DayWebView::supportsDataClearing(env),
    })
    .unwrap_or(false);
    if supported {
        day_spec::Support::Native
    } else {
        day_spec::Support::Unsupported
    }
}

fn make(_backend: &mut Material, p: &WebProps, id: NodeId) -> AHandle {
    if p.profile.private {
        return AHandle(day_toolkit_material::with_env(|env| {
            day_toolkit_material::placeholder_view(env, "web_view")
        }));
    }
    // Inline mode (docs/webview.md): the assets tree is the APK `assets/` root, and WebView
    // browses it through `file:///android_asset/` (exempt from the API-30 file-access
    // default). The URL is composed here; the Java side polices navigations against the
    // prefix and reports external ones back (LINK_REPORT).
    let (url, prefix) = if p.inline_root.is_empty() {
        (
            p.url.clone(),
            p.resources
                .as_ref()
                .map(ResourceProvider::base_url)
                .unwrap_or_default(),
        )
    } else {
        (
            format!("file:///android_asset/{}/{}", p.inline_root, p.inline_start),
            format!("file:///android_asset/{}/", p.inline_root),
        )
    };
    let provider = p.resources.as_ref().map(ResourceProvider::id).unwrap_or(0) as i64;
    let profile = if p.profile == WebProfile::default() {
        String::new()
    } else {
        p.profile.storage_key()
    };
    // A Java throw (e.g. the staged DayWebView class missing) must not panic inside realize:
    // the panic unwinds the JNI up-call and aborts. Placeholder instead.
    let view = day_toolkit_material::realize("web_view", |env| {
        let url = JString::from_str(env, &url)?;
        let prefix = JString::from_str(env, &prefix)?;
        let profile = JString::from_str(env, &profile)?;
        DayWebView::makeWebView(env, id.0 as i64, &url, &prefix, provider, &profile)
    });
    if let Some(labels) = &p.tabs {
        java(|env| {
            let foreground = JString::from_str(env, &labels.foreground)?;
            let background = JString::from_str(env, &labels.background)?;
            DayWebView::configureTabs(env, view.0.as_obj(), &foreground, &background)
        });
    }
    if p.transparent {
        java(|env| DayWebView::setTransparent(env, view.0.as_obj()));
    }
    view
}

fn update(_backend: &mut Material, h: &AHandle, patch: &WebPatch) {
    // Evaluation (docs/webview-eval.md): `evaluateJavascript` plus the outer-JSON unquote and
    // the null/empty→engine-error mapping live in DayWebView.evalJs; the reply rides the
    // kind-12 channel keyed by `req`, like every other backend.
    if let WebPatch::Eval { req, script } = patch {
        java(|env| {
            let script = JString::from_str(env, script)?;
            DayWebView::evalJs(env, h.0.as_obj(), *req as f64, &script)
        });
        return;
    }
    // Commands cross as (code, url): 0=load, 1=back, 2=forward, 3=stop, 4=reload.
    let (code, url) = match patch {
        WebPatch::Load(u) => (0, u.as_str()),
        WebPatch::Back => (1, ""),
        WebPatch::Forward => (2, ""),
        WebPatch::Stop => (3, ""),
        WebPatch::Reload => (4, ""),
        WebPatch::ForceReload => (5, ""),
        WebPatch::Eval { .. } => return, // handled above; keeps the match exhaustive
    };
    java(|env| {
        let url = JString::from_str(env, url)?;
        DayWebView::webCommand(env, h.0.as_obj(), code, &url)
    });
}

fn release(_backend: &mut Material, handle: &AHandle) {
    java(|env| DayWebView::release(env, handle.0.as_obj()));
}

day_pieces::renderer!(day_toolkit_material::RENDERERS, Material,
    kind: KIND, props: WebProps, patch: WebPatch,
    make: make, update: update, measure: day_pieces::fill_measure, release: release);

impl DayWebViewNativeInterface for DayWebViewAPI {
    type Error = day_toolkit_material::jni::errors::Error;

    fn resource<'l>(
        env: &mut Env<'l>,
        _class: JClass<'l>,
        provider: i64,
        url: JString<'l>,
        method: JString<'l>,
        range: JString<'l>,
    ) -> Result<JObject<'l>, Self::Error> {
        let url = string_of(env, &url)?;
        let method = string_of(env, &method)?;
        let range = string_of(env, &range)?;
        let r = super::resources::respond(provider as u64, &url, &method, &range);
        let bytes = env.byte_array_from_slice(&r.body)?;
        let mime = JString::from_str(env, &r.mime)?;
        let headers = JString::from_str(
            env,
            r.headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}\n"))
                .collect::<String>(),
        )?;
        DayWebView::response(env, &mime, r.status as i32, &headers, &bytes)
    }
}
