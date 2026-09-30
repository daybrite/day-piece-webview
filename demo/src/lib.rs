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
    let resources = Signal::new(false);
    let script = Signal::new(String::new());
    let answer = Signal::new(String::new());
    // The route of the last app link the site sent; empty until the first one arrives.
    let received = Signal::new(String::new());
    let js = JsHandle::new();
    let reload = Trigger::new();
    let can_eval = eval_support() == Support::Native;
    let has_engine = inline_support() != Support::Unsupported;

    column((
        label(res::str::app_title())
            .font(Font::Title)
            .id("webview-title"),
        label(res::str::caption()).font(Font::Footnote),
        section((
            labeled(
                res::str::support_remote(),
                support_label(support(), "webview-support"),
            ),
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
            button(res::str::reload())
                .enabled(move || has_engine)
                .action(move || reload.notify())
                .id("webview-reload"),
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
        button(res::str::resource_demo())
            .action(move || resources.set(!resources.get_untracked()))
            .id("resource-demo"),
        when(move || resources.get(), move || resource_site(js, reload))
            .otherwise(move || site(js, received, reload)),
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
fn site(js: JsHandle, received: Signal<String>, reload: Trigger) -> AnyPiece {
    if inline_support() == Support::Unsupported {
        return label(res::str::site_unsupported())
            .font(Font::Footnote)
            .id("webview-unsupported")
            .any();
    }
    web_view_inline(res::assets::site)
        .js(js)
        .reload(reload)
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

fn resource_site(js: JsHandle, reload: Trigger) -> AnyPiece {
    use day_piece_webview::{ResourceProvider, ResourceResponse, web_view_resources};
    // Localize on the UI thread; the provider itself runs on native IO workers.
    let title = res::str::resource_demo().format();
    let caption = res::str::resource_caption().format();
    let html = format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>{title}</title><link rel="stylesheet" href="../styles/main.css"></head><body><h1>{title}</h1><p>{caption}</p><img id="resource-image" src="../images/orb.svg"><iframe id="resource-child" src="child.html"></iframe><script src="../scripts/main.js"></script></body></html>"#
    );
    let provider = ResourceProvider::with_site(res::assets::site, move |request| {
        match request.path.as_str() {
        "pages/index.html" => ResourceResponse::new("text/html",html.clone().into_bytes()),
        "pages/child.html" => ResourceResponse::new("text/html",b"<!doctype html><html><head><link rel=stylesheet href=../styles/main.css></head><body data-relative=child></body></html>".to_vec()),
        "styles/main.css" => ResourceResponse::new("text/css",b"@import '../__day_assets/css/style.css';@import './colors.css';body{font:18px system-ui;padding:24px;background:#182236;color:white}img{width:120px;height:120px}iframe{border:0;width:120px;height:120px}".to_vec()),
        "styles/colors.css" => ResourceResponse::new("text/css",b":root{--resource-loaded:yes}body{background-image:url('../images/orb.svg');background-repeat:no-repeat;background-position:right bottom}".to_vec()),
        "images/orb.svg" => ResourceResponse::new("image/svg+xml",br##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="120"><defs><radialGradient id="g" cx="30%" cy="25%"><stop stop-color="#fff"/><stop offset=".4" stop-color="#30dcc5"/><stop offset="1" stop-color="#176caa"/></radialGradient></defs><circle cx="60" cy="60" r="55" fill="url(#g)"/></svg>"##.to_vec()),
        "scripts/main.js" => ResourceResponse::new("text/javascript",b"document.documentElement.dataset.resourceScript='yes';".to_vec()),
        _=>ResourceResponse::not_found(),
    }
    });
    web_view_resources(provider, "pages/index.html")
        .js(js)
        .reload(reload)
        .id("webview")
        .any()
}
