// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Web View Demo, the demo and on-device test app for `day-piece-webview`.
//!
//! One page: the support the piece reports for this platform, a JavaScript console, and a small
//! site bundled under `resource/assets/site/` shown in the web view. The site carries a link
//! addressed to this app, which the link policy hands back instead of opening a browser. Every
//! element carries a stable id, so `dayscript/webview.yaml` drives the same page on every target,
//! and the site loads from the app bundle, so none of the script's checks wait on a network.

use day::prelude::*;
use day_piece_webview::{
    JsHandle, LinkPolicy, eval_support, inline_support, support, web_view_inline,
};

// The mobile entry point; a plain cargo desktop build enters through src/main.rs.
day::day_start!(options: window(), root);

// Typed constants for everything under `resource/` (https://daybrite.dev/docs/resources).
day::resources!();

/// The scheme of the links the bundled site addresses to this app rather than to a browser.
const APP_SCHEME: &str = "webviewdemo://";

/// The window every entry point opens.
pub fn window() -> day::WindowOptions {
    day::WindowOptions {
        locales: Some((res::locales::DEFAULT, res::locales::CATALOG)),
        title_fn: Some(|| res::str::app_title().format()),
        size: day::prelude::Size::new(800.0, 680.0),
        ..Default::default()
    }
}

/// The whole app: the support rows, the console, the link readout, and the site.
pub fn root() -> impl Piece {
    info!("Web View Demo starting");

    // The script the console runs, and the JSON (or the error) the page answered with.
    let script = Signal::new(String::new());
    let answer = Signal::new(String::new());
    // The route of the last app link the site sent; empty until the first one arrives.
    let received = Signal::new(String::new());
    let js = JsHandle::new();
    let can_eval = eval_support() == Support::Native;

    column((
        label(res::str::app_title())
            .font(Font::Title)
            .id("webview-title"),
        label(res::str::caption()).font(Font::Footnote),
        section((
            labeled(res::str::support_remote(), support_label(support(), "webview-support")),
            labeled(
                res::str::support_inline(),
                support_label(inline_support(), "webview-inline-support"),
            ),
            labeled(
                res::str::support_eval(),
                support_label(eval_support(), "webview-eval-support"),
            ),
        ))
        .title(res::str::support_section()),
        row((
            text_field(script)
                .placeholder(res::str::js_hint())
                .id("webview-js"),
            button(res::str::js_run())
                .enabled(move || can_eval)
                .action(move || {
                    // `eval` answers through a future, so the result lands whenever the engine
                    // replies (https://daybrite.dev/docs/async).
                    day::task(async move {
                        let text = match js.eval(script.get_untracked()).await {
                            Ok(json) => json,
                            Err(e) => e.to_string(),
                        };
                        answer.set(text);
                    });
                })
                .id("webview-js-run"),
        ))
        .spacing(8.0),
        label(move || answer.get())
            .font(Font::Footnote)
            .id("webview-js-result"),
        label(move || {
            let route = received.get();
            if route.is_empty() {
                res::str::link_none().format()
            } else {
                res::str::link_received(route).format()
            }
        })
        .font(Font::Footnote)
        .id("webview-link"),
        site(js, received),
    ))
    .spacing(10.0)
    .align(HAlign::Leading)
    .padding(16.0)
}

/// One support answer as a label carrying `id`.
fn support_label(answer: Support, id: &'static str) -> impl Piece {
    label(match answer {
        Support::Native => res::str::support_native(),
        Support::Emulated => res::str::support_emulated(),
        Support::Unsupported => res::str::support_unsupported(),
    })
    .id(id)
}

/// The bundled site in the web view, or a line saying this build has no engine to show it in.
fn site(js: JsHandle, received: Signal<String>) -> AnyPiece {
    if inline_support() == Support::Unsupported {
        return label(res::str::site_unsupported())
            .font(Font::Footnote)
            .id("webview-unsupported")
            .any();
    }
    web_view_inline(res::assets::site)
        .js(js)
        .on_external_link(move |url| match url.strip_prefix(APP_SCHEME) {
            // A link addressed to the app: record it here and keep both the view and the
            // system browser where they are.
            Some(route) => {
                received.set(route.trim_matches('/').to_string());
                LinkPolicy::Ignore
            }
            None => LinkPolicy::OpenSystem,
        })
        .id("webview")
        .any()
}
