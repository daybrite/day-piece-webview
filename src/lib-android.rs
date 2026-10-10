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
use day_android::DayEnv;
use day_android::jni::objects::JValue;
use day_android::{AHandle, Android, with_env};
use day_spec::NodeId;

/// This piece's Java class (src/DayWebView.java, on the app classpath at build).
const WEBVIEW_CLASS: &str = "dev/daybrite/day/piece/webview/DayWebView";

pub(crate) fn capability(method: &str) -> day_spec::Support {
    let supported = with_env(|env| {
        env.dcall_static(WEBVIEW_CLASS, method, "()Z", &[])
            .ok()
            .and_then(|value| value.z().ok())
            .unwrap_or(false)
    });
    if supported {
        day_spec::Support::Native
    } else {
        day_spec::Support::Unsupported
    }
}

fn make(_backend: &mut Android, p: &WebProps, id: NodeId) -> AHandle {
    if p.profile.private {
        return with_env(|env| AHandle(day_android::placeholder_view(env, "web_view")));
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
    with_env(|env| {
        // A Java throw (e.g. the staged DayWebView class missing) must not panic inside
        // realize: the panic unwinds the JNI up-call and aborts. Placeholder instead.
        let made = env.new_string(&url).ok().and_then(|url| {
            let prefix = env.new_string(&prefix).ok()?;
            let profile = env
                .new_string(if p.profile == WebProfile::default() {
                    String::new()
                } else {
                    p.profile.storage_key()
                })
                .ok()?;
            day_android::try_make_view_on(
                    env,
                    WEBVIEW_CLASS,
                    "makeWebView",
                    "(JLjava/lang/String;Ljava/lang/String;JLjava/lang/String;)Landroid/view/View;",
                    &[
                        JValue::Long(id.0 as i64),
                        JValue::Object(&url),
                        JValue::Object(&prefix),
                        JValue::Long(
                            p.resources.as_ref().map(ResourceProvider::id).unwrap_or(0) as i64
                        ),
                        JValue::Object(&profile),
                    ],
                )
                .ok()
        });
        let view = made.unwrap_or_else(|| {
            log::warn!(
                "day-piece-webview: DayWebView.makeWebView failed; substituting a placeholder"
            );
            day_android::placeholder_view(env, "web_view")
        });
        if let Some(labels) = &p.tabs {
            if let (Ok(foreground), Ok(background)) = (
                env.new_string(&labels.foreground),
                env.new_string(&labels.background),
            ) {
                let _ = env.dcall_static(
                    WEBVIEW_CLASS,
                    "configureTabs",
                    "(Landroid/view/View;Ljava/lang/String;Ljava/lang/String;)V",
                    &[
                        JValue::Object(view.as_obj()),
                        JValue::Object(&foreground),
                        JValue::Object(&background),
                    ],
                );
            }
        }
        if p.transparent {
            let _ = env.dcall_static(
                WEBVIEW_CLASS,
                "setTransparent",
                "(Landroid/view/View;)V",
                &[JValue::Object(view.as_obj())],
            );
        }
        AHandle(view)
    })
}

fn update(_backend: &mut Android, h: &AHandle, patch: &WebPatch) {
    // Evaluation (docs/webview-eval.md): `evaluateJavascript` plus the outer-JSON unquote and
    // the null/empty→engine-error mapping live in DayWebView.evalJs; the reply rides the
    // kind-12 channel keyed by `req`, like every other backend.
    if let WebPatch::Eval { req, script } = patch {
        with_env(|env| {
            let Ok(s) = env.new_string(script) else {
                return;
            };
            let _ = env.dcall_static(
                WEBVIEW_CLASS,
                "evalJs",
                "(Landroid/view/View;DLjava/lang/String;)V",
                &[
                    JValue::Object(h.0.as_obj()),
                    JValue::Double(*req as f64),
                    JValue::Object(&s),
                ],
            );
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
    with_env(|env| {
        let Ok(s) = env.new_string(url) else { return };
        let _ = env.dcall_static(
            WEBVIEW_CLASS,
            "webCommand",
            "(Landroid/view/View;ILjava/lang/String;)V",
            &[
                JValue::Object(h.0.as_obj()),
                JValue::Int(code),
                JValue::Object(&s),
            ],
        );
    });
}

fn release(_backend: &mut Android, handle: &AHandle) {
    with_env(|env| {
        let _ = env.dcall_static(
            WEBVIEW_CLASS,
            "release",
            "(Landroid/view/View;)V",
            &[JValue::Object(handle.0.as_obj())],
        );
    });
}

day_pieces::renderer!(day_android::RENDERERS, Android,
    kind: KIND, props: WebProps, patch: WebPatch,
    make: make, update: update, measure: day_pieces::fill_measure, release: release);

// Called by WebView's IO worker, never by the UI thread.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_daybrite_day_piece_webview_DayWebView_resource(
    mut unowned: day_android::jni::EnvUnowned,
    _class: day_android::jni::objects::JClass,
    provider: i64,
    url: day_android::jni::objects::JString,
    method: day_android::jni::objects::JString,
    range: day_android::jni::objects::JString,
) -> day_android::jni::sys::jobject {
    unowned
        .with_env(|env| -> day_android::jni::errors::Result<_> {
            let url = env.dstr(&url)?;
            let method = env.dstr(&method)?;
            let range = env.dstr(&range)?;
            let r = super::resources::respond(provider as u64, &url, &method, &range);
            let bytes = env.byte_array_from_slice(&r.body)?;
            let mime = env.new_string(&r.mime)?;
            let headers = env.new_string(
                r.headers
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}\n"))
                    .collect::<String>(),
            )?;
            let response = env
                .dcall_static(
                    WEBVIEW_CLASS,
                    "response",
                    "(Ljava/lang/String;ILjava/lang/String;[B)Landroid/webkit/WebResourceResponse;",
                    &[
                        JValue::Object(&mime),
                        JValue::Int(r.status as i32),
                        JValue::Object(&headers),
                        JValue::Object(&bytes),
                    ],
                )?
                .l()?;
            Ok(response.into_raw())
        })
        .resolve::<day_android::jni::errors::ThrowRuntimeExAndDefault>()
}
