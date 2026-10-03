// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Web View Demo, the demo and on-device test app for `day-piece-webview`.
//!
//! A conventional browser with native history, an editable address bar and private profiles.
//! A collapsible testing drawer exercises JavaScript, app links and resource providers against
//! a localized bundled website, so the cross-platform walkthrough does not require the network.

use day::prelude::*;
use day_piece_webview::{
    JsHandle, LinkPolicy, NavigationState, eval_support, inline_support, support, web_view,
    web_view_inline,
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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Source {
    Home,
    Resources,
    Remote,
}
#[derive(Clone, Copy)]
struct Browser {
    source: Signal<Source>,
    location: Signal<String>,
    address: Signal<String>,
    navigation: Signal<NavigationState>,
    status: Signal<String>,
    received: Signal<String>,
    js: JsHandle,
    go: Trigger,
    back: Trigger,
    forward: Trigger,
    stop: Trigger,
    reload: Trigger,
}
impl Browser {
    fn home(self) {
        self.status.set(String::new());
        self.source.set(Source::Home);
        self.location.set(String::new());
        self.address.set("day://home".into());
        self.reload.notify();
    }
    fn navigate(self) {
        match normalize_address(&self.address.get_untracked()) {
            Ok(url) if url == "day://home" => self.home(),
            Ok(url) if url == "day://resources" => {
                self.source.set(Source::Resources);
                self.location.set(String::new());
            }
            Ok(url) => {
                self.location.set(url.clone());
                self.address.set(url);
                if self.source.get_untracked() == Source::Remote {
                    self.go.notify();
                } else {
                    self.source.set(Source::Remote);
                }
                self.status.set(String::new());
            }
            Err(()) => self.status.set(res::str::address_invalid().format()),
        }
    }
    fn link(self, url: &str) -> LinkPolicy {
        if let Some(route) = url.strip_prefix(APP_SCHEME) {
            self.received.set(route.trim_matches('/').to_owned());
            LinkPolicy::Ignore
        } else if normalize_address(url).is_ok() {
            self.address.set(url.to_owned());
            self.navigate();
            LinkPolicy::Ignore
        } else {
            LinkPolicy::OpenSystem
        }
    }
}
fn normalize_address(input: &str) -> Result<String, ()> {
    let input = input.trim();
    if matches!(input, "day://home" | "day://resources" | "about:blank") {
        return Ok(input.into());
    }
    if input.is_empty() || input.chars().any(char::is_whitespace) {
        return Err(());
    }
    let candidate = if input.contains("://")
        || ["javascript:", "data:", "file:", "about:"]
            .iter()
            .any(|prefix| input.starts_with(prefix))
    {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let url = url::Url::parse(&candidate).map_err(|_| ())?;
    if matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
    {
        Ok(url.to_string())
    } else {
        Err(())
    }
}

fn home_localization() -> String {
    let text = serde_json::json!({"title":res::str::home_title().format(),"heading":res::str::home_heading().format(),"intro":res::str::home_intro().format(),"loaded":res::str::home_loaded().format(),"app":res::str::home_app_link().format(),"web":res::str::home_web_link().format(),"next":res::str::home_next().format(),"second":res::str::second_title().format(),"second_text":res::str::second_text().format()});
    format!("if (window.dayDemoLocalize) window.dayDemoLocalize({text})")
}
pub fn root() -> impl Piece {
    let browser = Browser {
        source: Signal::new(Source::Home),
        location: Signal::new(String::new()),
        address: Signal::new("day://home".into()),
        navigation: Signal::new(NavigationState::default()),
        status: Signal::new(String::new()),
        received: Signal::new(String::new()),
        js: JsHandle::new(),
        go: Trigger::new(),
        back: Trigger::new(),
        forward: Trigger::new(),
        stop: Trigger::new(),
        reload: Trigger::new(),
    };
    let private = Signal::new(day::prefs::get("demo.web.incognito").as_deref() == Some("true"));
    let generation = Signal::new(0u64);
    let clearing = Signal::new(false);
    let settings = Signal::new(None::<String>);
    let tools = Signal::new(false);
    let script = Signal::new(String::new());
    let answer = Signal::new(String::new());
    watch(
        move || private.get(),
        move |value, _| {
            day::prefs::set("demo.web.incognito", &value.to_string());
        },
    );
    let key = move || (browser.source.get(), private.get(), generation.get());
    let polling = day::task(async move {
        loop {
            let requested = key();
            if support() == Support::Native
                && !(requested.1
                    && day_piece_webview::private_browsing_support() == Support::Unsupported)
                && let futures_util::future::Either::Left((Ok(state), _)) =
                    futures_util::future::select(
                        Box::pin(browser.js.navigation_state()),
                        Box::pin(day::sleep(1000)),
                    )
                    .await
                && requested == key()
            {
                if !state.url.is_empty() && state.url != browser.location.get_untracked() {
                    browser.location.set(state.url.clone());
                    browser.address.set(state.url.clone());
                }
                browser.navigation.set_if_changed(state);
            }
            if support() == Support::Emulated && requested.0 == Source::Home && !requested.1 {
                let _ = futures_util::future::select(
                    Box::pin(browser.js.eval(home_localization())),
                    Box::pin(day::sleep(1000)),
                )
                .await;
            }
            day::sleep(300).await;
        }
    });
    day::reactive::Scope::current().on_cleanup(move || polling.abort());
    watch(
        move || browser.location.get(),
        move |url, _| {
            if !url.is_empty() {
                browser.address.set(url.clone());
            }
        },
    );
    column((
        column((
            row((
                button(res::str::back())
                    .icon(Symbol::Back)
                    .icon_only()
                    .enabled(move || {
                        support() == Support::Native && browser.navigation.get().can_go_back
                    })
                    .action(move || browser.back.notify())
                    .id("webview-back"),
                button(res::str::forward())
                    .icon(Symbol::Forward)
                    .icon_only()
                    .enabled(move || {
                        support() == Support::Native && browser.navigation.get().can_go_forward
                    })
                    .action(move || browser.forward.notify())
                    .id("webview-forward"),
                button(res::str::reload())
                    .icon(Symbol::Refresh)
                    .icon_only()
                    .action(move || browser.reload.notify())
                    .id("webview-reload"),
                button(res::str::stop())
                    .icon(Symbol::Stop)
                    .icon_only()
                    .enabled(move || {
                        support() == Support::Native && browser.navigation.get().loading
                    })
                    .action(move || browser.stop.notify())
                    .id("webview-stop"),
                button(res::str::home())
                    .icon(Symbol::Home)
                    .icon_only()
                    .action(move || {
                        browser.home();
                        generation.update(|n| *n = n.wrapping_add(1));
                    })
                    .id("webview-home"),
                spacer().grow_w(),
                when(
                    move || private.get(),
                    || {
                        label(res::str::private_badge())
                            .font(Font::Caption)
                            .id("browser-private")
                    },
                ),
                button(res::str::test_tools())
                    .icon(Symbol::Code)
                    .icon_only()
                    .action(move || tools.set(!tools.get_untracked()))
                    .id("webview-tools"),
                button(res::str::browser_settings())
                    .icon(Symbol::Settings)
                    .icon_only()
                    .action(move || settings.set(Some("settings".into())))
                    .id("webview-settings"),
            ))
            .spacing(4.0)
            .align(VAlign::Center),
            row((
                text_field(browser.address)
                    .placeholder(res::str::address_hint())
                    .on_submit(move || browser.navigate())
                    .id("webview-address")
                    .grow_w(),
                button(res::str::go())
                    .icon(Symbol::Forward)
                    .icon_only()
                    .action(move || browser.navigate())
                    .id("webview-go"),
                button(res::str::open_external())
                    .icon(Symbol::Share)
                    .icon_only()
                    .enabled(move || {
                        browser.location.get().starts_with("https://")
                            || browser.location.get().starts_with("http://")
                    })
                    .action(move || open_url(&browser.location.get_untracked()))
                    .id("webview-open-external"),
            ))
            .spacing(8.0)
            .align(VAlign::Center),
        ))
        .spacing(6.0)
        .padding(10.0),
        each(items(move || vec![key()], |key| *key), move |slot| {
            let (source, private, _) = slot.get();
            if private && day_piece_webview::private_browsing_support() == Support::Unsupported {
                return label(res::str::private_hint())
                    .id("webview-unsupported")
                    .any();
            }
            let view = match source {
                Source::Home => web_view_inline(res::assets::site).url_binding(browser.location),
                Source::Resources => return resource_site(browser, private),
                Source::Remote => web_view(browser.location),
            };
            view.profile(demo_profile(private))
                .js(browser.js)
                .go(browser.go)
                .back(browser.back)
                .forward(browser.forward)
                .stop(browser.stop)
                .reload(browser.reload)
                .on_external_link(move |url| {
                    if browser.source.get_untracked() == Source::Remote {
                        if let Some(route) = url.strip_prefix(APP_SCHEME) {
                            browser.received.set(route.trim_matches('/').into());
                            LinkPolicy::Ignore
                        } else {
                            LinkPolicy::InView
                        }
                    } else {
                        browser.link(url)
                    }
                })
                .on_load(move || {
                    if source == Source::Home {
                        let script = home_localization();
                        day::task(async move {
                            let _ = browser.js.eval(script).await;
                        });
                    }
                })
                .id("webview")
                .any()
        })
        .grow(),
        row((
            label(move || {
                let title = browser.navigation.get().title;
                if title.is_empty() {
                    res::str::app_title().format()
                } else {
                    title
                }
            })
            .single_line()
            .grow_w()
            .id("webview-title"),
            when(
                move || browser.navigation.get().loading,
                || spinner().frame(16.0, 16.0),
            ),
            label(move || browser.status.get())
                .font(Font::Caption)
                .single_line()
                .id("browser-status"),
        ))
        .spacing(8.0)
        .padding(Insets::symmetric(10.0, 6.0)),
        when(
            move || tools.get(),
            move || {
                column((
                    row((
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
                    .spacing(12.0),
                    row((
                        text_field(script)
                            .placeholder(res::str::js_hint())
                            .id("webview-js")
                            .grow_w(),
                        button(res::str::js_run())
                            .action(move || {
                                day::task(async move {
                                    let text = match browser.js.eval(script.get_untracked()).await {
                                        Ok(json) => json,
                                        Err(_) => res::str::js_failed().format(),
                                    };
                                    answer.set(text);
                                });
                            })
                            .enabled(|| eval_support() == Support::Native)
                            .id("webview-js-run"),
                    ))
                    .spacing(8.0),
                    label(move || answer.get())
                        .font(Font::Footnote)
                        .single_line()
                        .id("webview-js-result"),
                    label(move || {
                        let route = browser.received.get();
                        if route.is_empty() {
                            res::str::link_none().format()
                        } else {
                            res::str::link_received(route).format()
                        }
                    })
                    .font(Font::Footnote)
                    .single_line()
                    .id("webview-link"),
                    button(res::str::resource_demo())
                        .action(move || {
                            browser.location.set(String::new());
                            browser.source.set(
                                if browser.source.get_untracked() == Source::Resources {
                                    Source::Home
                                } else {
                                    Source::Resources
                                },
                            );
                        })
                        .id("resource-demo"),
                ))
                .spacing(6.0)
                .align(HAlign::Leading)
                .padding(10.0)
            },
        ),
        cover(settings, move |_: &String| {
            column((
                label(res::str::browser_settings()).font(Font::Title),
                labeled(
                    res::str::incognito_mode(),
                    toggle(private).id("webview-incognito"),
                ),
                labeled(
                    res::str::support_private(),
                    support_label(
                        day_piece_webview::private_browsing_support(),
                        "webview-private-support",
                    ),
                ),
                labeled(
                    res::str::support_profiles(),
                    support_label(
                        day_piece_webview::profile_support(),
                        "webview-profile-support",
                    ),
                ),
                label(res::str::private_hint()).font(Font::Footnote),
                button(res::str::forget_data())
                    .action(move || {
                        clearing.set(true);
                        day::task(async move {
                            let text = if browser.js.clear_data().await.is_ok() {
                                generation.update(|n| *n = n.wrapping_add(1));
                                res::str::data_cleared().format()
                            } else {
                                res::str::data_clear_failed().format()
                            };
                            answer.set(text);
                            clearing.set(false);
                        });
                    })
                    .enabled(move || {
                        !clearing.get()
                            && day_piece_webview::clear_data_support() == Support::Native
                    })
                    .id("webview-clear-data"),
                label(move || answer.get())
                    .font(Font::Footnote)
                    .id("webview-data-result"),
                button(res::str::settings_done())
                    .action(move || settings.set(None))
                    .id("webview-settings-done"),
            ))
            .spacing(12.0)
            .padding(20.0)
        })
        .unrouted(),
    ))
}
fn support_label(answer: Support, id: &'static str) -> impl Piece {
    label(match answer {
        Support::Native => res::str::support_native(),
        Support::Emulated => res::str::support_emulated(),
        Support::Unsupported => res::str::support_unsupported(),
    })
    .font(Font::Caption)
    .id(id)
}

fn resource_site(browser: Browser, private: bool) -> AnyPiece {
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
        .profile(demo_profile(private))
        .url_binding(browser.location)
        .js(browser.js)
        .go(browser.go)
        .back(browser.back)
        .forward(browser.forward)
        .stop(browser.stop)
        .reload(browser.reload)
        .id("webview")
        .any()
}

fn demo_profile(private: bool) -> day_piece_webview::WebProfile {
    if private {
        day_piece_webview::WebProfile::private("demo")
    } else if day_piece_webview::profile_support() == Support::Native {
        day_piece_webview::WebProfile::persistent("demo")
    } else {
        day_piece_webview::WebProfile::default()
    }
}

#[cfg(test)]
mod browser_tests {
    use super::normalize_address;

    #[test]
    fn addresses_accept_hosts_and_preserve_navigation_components() {
        assert_eq!(
            normalize_address(" example.test/news?q=one#story "),
            Ok("https://example.test/news?q=one#story".into())
        );
        assert_eq!(
            normalize_address("http://localhost:8123/next"),
            Ok("http://localhost:8123/next".into())
        );
        for builtin in ["day://home", "day://resources", "about:blank"] {
            assert_eq!(normalize_address(builtin), Ok(builtin.into()));
        }
    }

    #[test]
    fn addresses_reject_code_local_files_credentials_and_invalid_input() {
        for input in [
            "",
            "two words",
            "javascript:alert(1)",
            "data:text/html,hello",
            "file:///etc/passwd",
            "about:config",
            "https://user:password@example.test/",
            "ftp://example.test/",
        ] {
            assert!(normalize_address(input).is_err(), "accepted {input}");
        }
    }
}
