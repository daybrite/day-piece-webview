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
use std::collections::HashMap;

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
    home_start: Signal<Option<String>>,
    resource_base: Signal<String>,
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
    force_reload: Trigger,
}
impl Browser {
    fn home(self) {
        self.status.set(String::new());
        self.home_start.set(None);
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
    fn can_share(self) -> bool {
        let url = self.location.get();
        url.starts_with("https://") || url.starts_with("http://")
    }
    fn share(self) {
        let url = self.location.get_untracked();
        if !self.can_share() {
            return;
        }
        if day::share_support() == Support::Native {
            if !day::share_url(&url, &self.navigation.get_untracked().title) {
                self.status.set(res::str::share_failed().format());
            }
        } else {
            day::task(async move {
                let message = if day::clipboard::write(day::clipboard::Content(vec![
                    day::clipboard::Representation::new("text/plain", url.into_bytes()),
                ]))
                .await
                .is_ok()
                {
                    res::str::link_copied().format()
                } else {
                    res::str::copy_failed().format()
                };
                self.status.set(message);
            });
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
#[derive(Clone, Copy)]
struct BrowserCommands {
    browser: Browser,
    private: Signal<bool>,
    generation: Signal<u64>,
    tools: Signal<bool>,
    address_focus: Signal<bool>,
}
#[derive(Clone, Copy)]
struct BrowserTabs {
    tabs: TabSet<String>,
    serial: Signal<u64>,
    urls: Signal<HashMap<String, String>>,
    titles: Signal<HashMap<String, String>>,
    profiles: Signal<HashMap<String, bool>>,
    views: Signal<HashMap<String, BrowserCommands>>,
    closed: Signal<Vec<String>>,
}
impl BrowserTabs {
    fn open(self, url: String, foreground: bool) {
        let private = self
            .active()
            .map(|view| view.private.get_untracked())
            .unwrap_or_else(|| day::prefs::get("demo.web.incognito").as_deref() == Some("true"));
        self.open_profile(url, foreground, private);
    }
    fn open_profile(self, url: String, foreground: bool, private: bool) {
        self.serial.update(|n| *n += 1);
        let key = self.serial.get_untracked().to_string();
        batch(|| {
            self.urls.update(|urls| {
                urls.insert(key.clone(), url);
            });
            self.profiles.update(|profiles| {
                profiles.insert(key.clone(), private);
            });
            self.tabs.open(key, foreground);
        });
    }
    fn active(self) -> Option<BrowserCommands> {
        self.tabs
            .selected()
            .and_then(|key| self.views.get().get(&key).copied())
    }
    fn close(self, key: &String) {
        if let Some(view) = self.views.get_untracked().get(key)
            && !view.private.get_untracked()
        {
            let url = tab_url(view.browser);
            self.closed.update(|closed| {
                closed.push(url);
                if closed.len() > 20 {
                    closed.remove(0);
                }
            });
        }
        self.tabs.close(key);
        self.urls.update(|urls| {
            urls.remove(key);
        });
        self.titles.update(|titles| {
            titles.remove(key);
        });
        self.profiles.update(|profiles| {
            profiles.remove(key);
        });
        if self.tabs.keys().is_empty() {
            self.open(String::new(), true);
        }
    }
    fn close_selected(self) {
        if let Some(key) = self.tabs.selected() {
            self.close(&key);
        }
    }
    fn reopen(self) {
        let mut closed = self.closed.get_untracked();
        if let Some(url) = closed.pop() {
            self.closed.set(closed);
            self.open(url, true);
        }
    }
    fn duplicate(self) {
        if let Some(view) = self.active() {
            self.open_profile(tab_url(view.browser), true, view.private.get_untracked());
        }
    }
    fn move_selected(self, after: bool) {
        if let Some(key) = self.tabs.selected()
            && let Some(at) = self.tabs.keys().iter().position(|k| k == &key)
        {
            self.tabs
                .move_to(&key, if after { at + 1 } else { at.saturating_sub(1) });
        }
    }
}
fn canonical_tab_url(browser: Browser, url: &str) -> String {
    let source = browser.source.get_untracked();
    if source == Source::Home
        && (url.starts_with("file:")
            || url.starts_with("qrc:")
            || url.starts_with("resource:")
            || support() == Support::Emulated)
    {
        let prefix = format!("/{}/", res::assets::site.as_str());
        if let Some((_, path)) = url.split_once(&prefix) {
            return format!("day://home/{path}");
        }
    }
    if source == Source::Resources {
        let base = browser.resource_base.get_untracked();
        if !base.is_empty()
            && let Some(path) = url.strip_prefix(&base)
        {
            return format!("day://resources/{path}");
        }
    }
    url.to_owned()
}
fn tab_url(browser: Browser) -> String {
    let location = browser.location.get_untracked();
    if location.is_empty() {
        browser.address.get_untracked()
    } else {
        canonical_tab_url(browser, &location)
    }
}
fn tab_widget_id(tab: u64, base: &str) -> String {
    if tab == 1 {
        base.to_owned()
    } else {
        format!("{base}-tab-{tab}")
    }
}
pub fn root() -> impl Piece {
    let tabs = BrowserTabs {
        tabs: TabSet::new(),
        serial: Signal::new(0),
        urls: Signal::new(HashMap::new()),
        titles: Signal::new(HashMap::new()),
        profiles: Signal::new(HashMap::new()),
        views: Signal::new(HashMap::new()),
        closed: Signal::new(Vec::new()),
    };
    let native_tabs =
        Signal::new(day::prefs::get("demo.web.native-tabs").as_deref() != Some("false"));
    watch(
        move || native_tabs.get(),
        move |value, _| {
            day::prefs::set("demo.web.native-tabs", &value.to_string());
        },
    );
    day::register_preferences_with(
        day::WindowOptions {
            title: res::str::browser_settings().format(),
            size: Size::new(520.0, 480.0),
            ..Default::default()
        },
        move || preferences(tabs, native_tabs),
    );
    tabs.open(String::new(), true);
    app_menu_reactive(move || {
        tabs.active()
            .map(|commands| browser_menus(tabs, commands))
            .unwrap_or_default()
    });
    column((document_tabs(
        tabs.tabs,
        res::str::new_tab(),
        res::str::close_tab(),
        move |key| {
            tabs.titles
                .get()
                .get(key)
                .filter(|title| !title.is_empty())
                .cloned()
                .unwrap_or_else(|| res::str::new_tab().format())
        },
        move |key| browser_page(tabs, key.clone()),
    )
    .native(move || native_tabs.get())
    .on_new(move || tabs.open(String::new(), true))
    .on_close(move |key| tabs.close(&key))
    .id("browser-tabs")
    .grow(),))
}
fn tab_controls(tabs: BrowserTabs, tab_number: u64) -> impl Piece {
    row((
        button(res::str::new_tab())
            .icon(Symbol::Add)
            .icon_only()
            .compact()
            .action(move || tabs.open(String::new(), true))
            .id(tab_widget_id(tab_number, "tab-new"))
            .width(40.0),
        button(res::str::close_tab())
            .icon(Symbol::Close)
            .icon_only()
            .compact()
            .action(move || tabs.close_selected())
            .id(tab_widget_id(tab_number, "tab-close"))
            .width(40.0),
        button(res::str::previous_tab())
            .icon(Symbol::Back)
            .icon_only()
            .compact()
            .action(move || tabs.tabs.select_next(true))
            .id(tab_widget_id(tab_number, "tab-previous"))
            .width(40.0),
        button(res::str::next_tab())
            .icon(Symbol::Forward)
            .icon_only()
            .compact()
            .action(move || tabs.tabs.select_next(false))
            .id(tab_widget_id(tab_number, "tab-next"))
            .width(40.0),
        button(res::str::move_tab_before())
            .icon(Symbol::Back)
            .icon_only()
            .compact()
            .action(move || tabs.move_selected(false))
            .id(tab_widget_id(tab_number, "tab-move-before"))
            .width(40.0),
        button(res::str::move_tab_after())
            .icon(Symbol::Forward)
            .icon_only()
            .compact()
            .action(move || tabs.move_selected(true))
            .id(tab_widget_id(tab_number, "tab-move-after"))
            .width(40.0),
    ))
    .spacing(6.0)
    .padding(6.0)
}
fn browser_menus(tabs: BrowserTabs, commands: BrowserCommands) -> Vec<MenuEntry> {
    let BrowserCommands {
        private: _,
        browser,
        generation,
        tools,
        address_focus,
    } = commands;
    vec![
        sub_menu(
            res::str::menu_file().format(),
            vec![
                menu_item(res::str::new_tab().format())
                    .key("t")
                    .id("browser-new-tab")
                    .action(move || tabs.open(String::new(), true)),
                menu_item(res::str::close_tab().format())
                    .key("w")
                    .id("browser-close-tab")
                    .action(move || tabs.close_selected()),
                menu_item(res::str::reopen_tab().format())
                    .shortcut(Shortcut::new("t").shift())
                    .enabled(!tabs.closed.get().is_empty())
                    .action(move || tabs.reopen()),
                menu_separator(),
                menu_item(if day::share_support() == Support::Native {
                    res::str::share().format()
                } else {
                    res::str::copy_link().format()
                })
                .id("browser-share")
                .enabled(browser.can_share())
                .action(move || browser.share()),
            ],
        )
        .bar_role(MenuBarRole::File),
        sub_menu(
            res::str::menu_tabs().format(),
            vec![
                menu_item(res::str::next_tab().format())
                    .shortcut(Shortcut::plain("Tab").control())
                    .action(move || tabs.tabs.select_next(false)),
                menu_item(res::str::previous_tab().format())
                    .shortcut(Shortcut::plain("Tab").control().shift())
                    .action(move || tabs.tabs.select_next(true)),
                menu_separator(),
                menu_item(res::str::move_tab_before().format())
                    .action(move || tabs.move_selected(false)),
                menu_item(res::str::move_tab_after().format())
                    .action(move || tabs.move_selected(true)),
                menu_item(res::str::duplicate_tab().format()).action(move || tabs.duplicate()),
            ],
        ),
        sub_menu(
            res::str::menu_go().format(),
            vec![
                menu_item(res::str::back().format())
                    .id("browser-back")
                    .key("[")
                    .enabled(support() == Support::Native && browser.navigation.get().can_go_back)
                    .action(move || browser.back.notify()),
                menu_item(res::str::forward().format())
                    .id("browser-forward")
                    .key("]")
                    .enabled(
                        support() == Support::Native && browser.navigation.get().can_go_forward,
                    )
                    .action(move || browser.forward.notify()),
                menu_separator(),
                menu_item(res::str::home().format())
                    .id("browser-home")
                    .shortcut(Shortcut::plain("Home").alt())
                    .action(move || {
                        browser.home();
                        generation.update(|n| *n = n.wrapping_add(1));
                    }),
            ],
        ),
        sub_menu(
            res::str::menu_browser().format(),
            vec![
                menu_item(res::str::open_location().format())
                    .id("browser-open-location")
                    .key("l")
                    .action(move || address_focus.set(true)),
                menu_item(res::str::reload().format())
                    .id("browser-reload")
                    .key("r")
                    .action(move || browser.reload.notify()),
                menu_item(res::str::force_reload().format())
                    .id("browser-force-reload")
                    .shortcut(Shortcut::new("r").shift())
                    .enabled(day_piece_webview::force_reload_support() == Support::Native)
                    .action(move || browser.force_reload.notify()),
                menu_item(res::str::stop().format())
                    .id("browser-stop")
                    .shortcut(Shortcut::plain("Escape"))
                    .enabled(support() == Support::Native && browser.navigation.get().loading)
                    .action(move || browser.stop.notify()),
                menu_separator(),
                menu_item(res::str::test_tools().format())
                    .id("browser-tools")
                    .checked(tools.get())
                    .action(move || tools.set(!tools.get_untracked())),
                menu_item(res::str::browser_settings().format())
                    .id("browser-settings")
                    .key(",")
                    .action(|| {
                        day::open_preferences();
                    }),
            ],
        ),
    ]
}
fn browser_page(tabs: BrowserTabs, tab_key: String) -> impl Piece {
    let tab_number = tab_key.parse::<u64>().unwrap_or(1);
    let initial = tabs
        .urls
        .get_untracked()
        .get(&tab_key)
        .cloned()
        .unwrap_or_default();
    let remote = !initial.is_empty()
        && !initial.starts_with("day://home")
        && !initial.starts_with("day://resources");
    let initial_source = if initial.starts_with("day://resources") {
        Source::Resources
    } else if remote {
        Source::Remote
    } else {
        Source::Home
    };
    let home_start = Signal::new(initial.strip_prefix("day://home/").map(str::to_owned));
    let resource_start = Signal::new(
        initial
            .strip_prefix("day://resources/")
            .unwrap_or("pages/index.html")
            .to_owned(),
    );
    let browser = Browser {
        source: Signal::new(initial_source),
        home_start,
        resource_base: Signal::new(String::new()),
        location: Signal::new(if remote {
            initial.clone()
        } else {
            String::new()
        }),
        address: Signal::new(if remote { initial } else { "day://home".into() }),
        navigation: Signal::new(NavigationState::default()),
        status: Signal::new(String::new()),
        received: Signal::new(String::new()),
        js: JsHandle::new(),
        go: Trigger::new(),
        back: Trigger::new(),
        forward: Trigger::new(),
        stop: Trigger::new(),
        reload: Trigger::new(),
        force_reload: Trigger::new(),
    };
    let private = Signal::new(
        tabs.profiles
            .get_untracked()
            .get(&tab_key)
            .copied()
            .unwrap_or(false),
    );
    let generation = Signal::new(0u64);
    let tools = Signal::new(false);
    let address_focus = Signal::new(false);
    let script = Signal::new(String::new());
    let answer = Signal::new(String::new());
    watch(
        move || private.get(),
        move |value, _| {
            day::prefs::set("demo.web.incognito", &value.to_string());
        },
    );
    let commands = BrowserCommands {
        private,
        browser,
        generation,
        tools,
        address_focus,
    };
    tabs.views.update(|views| {
        views.insert(tab_key.clone(), commands);
    });
    let cleanup_key = tab_key.clone();
    Scope::current().on_cleanup(move || {
        tabs.views.update(|views| {
            views.remove(&cleanup_key);
        })
    });
    let title_key = tab_key.clone();
    watch(
        move || browser.navigation.get().title,
        move |title, _| {
            tabs.titles.update(|titles| {
                titles.insert(title_key.clone(), title.clone());
            });
        },
    );
    let key = move || {
        (
            browser.source.get(),
            private.get(),
            generation.get(),
            browser.home_start.get(),
        )
    };
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
        tab_controls(tabs, tab_number),
        column((
            row((
                button(res::str::back())
                    .icon(Symbol::Back)
                    .icon_only()
                    .compact()
                    .enabled(move || {
                        support() == Support::Native && browser.navigation.get().can_go_back
                    })
                    .action(move || browser.back.notify())
                    .id(tab_widget_id(tab_number, "webview-back")),
                button(res::str::forward())
                    .icon(Symbol::Forward)
                    .icon_only()
                    .compact()
                    .enabled(move || {
                        support() == Support::Native && browser.navigation.get().can_go_forward
                    })
                    .action(move || browser.forward.notify())
                    .id(tab_widget_id(tab_number, "webview-forward")),
                button(res::str::reload())
                    .icon(Symbol::Refresh)
                    .icon_only()
                    .compact()
                    .action(move || browser.reload.notify())
                    .id(tab_widget_id(tab_number, "webview-reload")),
                button(res::str::stop())
                    .icon(Symbol::Stop)
                    .icon_only()
                    .compact()
                    .enabled(move || {
                        support() == Support::Native && browser.navigation.get().loading
                    })
                    .action(move || browser.stop.notify())
                    .id(tab_widget_id(tab_number, "webview-stop")),
                button(res::str::home())
                    .icon(Symbol::Home)
                    .icon_only()
                    .compact()
                    .action(move || {
                        browser.home();
                        generation.update(|n| *n = n.wrapping_add(1));
                    })
                    .id(tab_widget_id(tab_number, "webview-home")),
                spacer().grow_w(),
                when(
                    move || private.get(),
                    move || {
                        label(res::str::private_badge())
                            .font(Font::Caption)
                            .id(tab_widget_id(tab_number, "browser-private"))
                    },
                ),
                button(res::str::test_tools())
                    .icon(Symbol::Code)
                    .icon_only()
                    .compact()
                    .action(move || tools.set(!tools.get_untracked()))
                    .id(tab_widget_id(tab_number, "webview-tools")),
                button(res::str::browser_settings())
                    .icon(Symbol::Settings)
                    .icon_only()
                    .compact()
                    .action(|| {
                        day::open_preferences();
                    })
                    .id(tab_widget_id(tab_number, "webview-settings")),
            ))
            .spacing(4.0)
            .align(VAlign::Center),
            row((
                text_field(browser.address)
                    .focused(address_focus)
                    .placeholder(res::str::address_hint())
                    .on_submit(move || browser.navigate())
                    .id(tab_widget_id(tab_number, "webview-address"))
                    .grow_w(),
                button(res::str::go())
                    .icon(Symbol::Forward)
                    .icon_only()
                    .compact()
                    .action(move || browser.navigate())
                    .id(tab_widget_id(tab_number, "webview-go")),
                button(res::str::open_external())
                    .image(res::vectors::open_external)
                    .icon_only()
                    .compact()
                    .enabled(move || {
                        browser.location.get().starts_with("https://")
                            || browser.location.get().starts_with("http://")
                    })
                    .action(move || open_url(&browser.location.get_untracked()))
                    .id(tab_widget_id(tab_number, "webview-open-external")),
                button(move || {
                    if day::share_support() == Support::Native {
                        res::str::share().format()
                    } else {
                        res::str::copy_link().format()
                    }
                })
                .icon(Symbol::Share)
                .icon_only()
                .compact()
                .enabled(move || browser.can_share())
                .action(move || browser.share())
                .id(tab_widget_id(tab_number, "webview-share")),
            ))
            .spacing(8.0)
            .align(VAlign::Center),
        ))
        .spacing(6.0)
        .padding(10.0),
        each(items(move || vec![key()], |key| key.clone()), move |slot| {
            let (source, private, _, _) = slot.get();
            if private && day_piece_webview::private_browsing_support() == Support::Unsupported {
                return label(res::str::private_hint())
                    .id(tab_widget_id(tab_number, "webview-unsupported"))
                    .any();
            }
            let view = match source {
                Source::Home => {
                    let view = web_view_inline(res::assets::site).url_binding(browser.location);
                    if let Some(path) = home_start.get_untracked() {
                        view.start_page(path)
                    } else {
                        view
                    }
                }
                Source::Resources => {
                    return resource_site(
                        browser,
                        private,
                        tabs,
                        tab_number,
                        resource_start.get_untracked(),
                    );
                }
                Source::Remote => web_view(browser.location),
            };
            view.profile(demo_profile(private))
                .on_new_tab(
                    res::str::open_link_new_tab().format(),
                    res::str::open_link_background_tab().format(),
                    move |request| {
                        tabs.open_profile(
                            canonical_tab_url(browser, &request.url),
                            !request.background,
                            private,
                        )
                    },
                )
                .js(browser.js)
                .go(browser.go)
                .back(browser.back)
                .forward(browser.forward)
                .stop(browser.stop)
                .reload(browser.reload)
                .force_reload(browser.force_reload)
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
                .id(tab_widget_id(tab_number, "webview"))
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
            .id(tab_widget_id(tab_number, "webview-title")),
            when(
                move || browser.navigation.get().loading,
                move || {
                    spinner()
                        .frame(16.0, 16.0)
                        .id(tab_widget_id(tab_number, "browser-loading"))
                },
            ),
            label(move || browser.status.get())
                .font(Font::Caption)
                .single_line()
                .id(tab_widget_id(tab_number, "browser-status")),
        ))
        .spacing(8.0)
        .padding(Insets::symmetric(10.0, 6.0)),
        when(
            move || tools.get(),
            move || {
                column((
                    column((
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
                    .spacing(4.0),
                    row((
                        text_field(script)
                            .placeholder(res::str::js_hint())
                            .id(tab_widget_id(tab_number, "webview-js"))
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
                            .id(tab_widget_id(tab_number, "webview-js-run")),
                    ))
                    .spacing(8.0),
                    label(move || answer.get())
                        .font(Font::Footnote)
                        .single_line()
                        .id(tab_widget_id(tab_number, "webview-js-result")),
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
                    .id(tab_widget_id(tab_number, "webview-link")),
                    row((
                        button(res::str::home_next())
                            .icon(Symbol::Forward)
                            .icon_only()
                            .compact()
                            .enabled(move || browser.source.get() == Source::Home)
                            .action(move || {
                                // Use an application navigation, like entering a URL. A script's
                                // synthetic link click has no user activation and Chromium can
                                // deliberately exclude its starting page from Back/Forward.
                                day::task(async move {
                                    if let Ok(json) = browser
                                        .js
                                        .eval("document.getElementById('next-link').href")
                                        .await
                                        && let Ok(url) = serde_json::from_str::<String>(&json)
                                    {
                                        browser.location.set(url);
                                        browser.go.notify();
                                    }
                                });
                            })
                            .id(tab_widget_id(tab_number, "webview-next-page")),
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
                            .id(tab_widget_id(tab_number, "resource-demo")),
                    ))
                    .spacing(8.0),
                ))
                .spacing(6.0)
                .align(HAlign::Leading)
                .padding(10.0)
            },
        ),
    ))
}
fn preferences(tabs: BrowserTabs, native_tabs: Signal<bool>) -> impl Piece {
    let window = day::current_window();
    let private = Signal::new(day::prefs::get("demo.web.incognito").as_deref() == Some("true"));
    let clearing = Signal::new(false);
    let result = Signal::new(String::new());
    watch(
        move || private.get(),
        move |value, _| {
            day::prefs::set("demo.web.incognito", &value.to_string());
            if let Some(commands) = tabs.active() {
                commands.private.set(*value);
            }
        },
    );
    column((
        label(res::str::browser_settings()).font(Font::Title),
        labeled(
            res::str::native_tabs(),
            toggle(native_tabs).id("webview-native-tabs"),
        ),
        label(res::str::native_tabs_hint()).font(Font::Footnote),
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
                let Some(commands) = tabs.active() else {
                    return;
                };
                clearing.set(true);
                day::task(async move {
                    let cleared = commands.browser.js.clear_data().await.is_ok();
                    if cleared && let Some(value) = commands.generation.try_get() {
                        commands.generation.set(value.wrapping_add(1));
                    }
                    result.set(if cleared {
                        res::str::data_cleared().format()
                    } else {
                        res::str::data_clear_failed().format()
                    });
                    clearing.set(false);
                });
            })
            .enabled(move || {
                !clearing.get()
                    && tabs.active().is_some()
                    && day_piece_webview::clear_data_support() == Support::Native
            })
            .id("webview-clear-data"),
        label(move || result.get())
            .font(Font::Footnote)
            .id("webview-data-result"),
        button(res::str::settings_done())
            .action(move || {
                if let Some(window) = &window {
                    window.close();
                }
            })
            .id("webview-settings-done"),
    ))
    .spacing(12.0)
    .padding(20.0)
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

fn resource_site(
    browser: Browser,
    private: bool,
    tabs: BrowserTabs,
    tab_number: u64,
    start: String,
) -> AnyPiece {
    use day_piece_webview::{ResourceProvider, ResourceResponse, web_view_resources};
    // Localize on the UI thread; the provider itself runs on native IO workers.
    let title = res::str::resource_demo().format();
    let caption = res::str::resource_caption().format();
    let html = format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>{title}</title><link rel="stylesheet" href="../styles/main.css"></head><body><h1>{title}</h1><p>{caption}</p><img id="resource-image" src="../images/orb.svg"><iframe id="resource-child" src="child.html"></iframe><script src="../scripts/main.js"></script></body></html>"#
    );
    let provider = ResourceProvider::with_site(res::assets::site, move |request| {
        match request.path.as_str() {
        // Android must split an HTTP-style content type before calling WebResourceResponse.
        // This exercises the same spelling as Day-News' generated reader document.
        "pages/index.html" => ResourceResponse::new(if cfg!(feature = "mdc") { "text/html; charset=utf-8" } else { "text/html" },html.clone().into_bytes()),
        "pages/child.html" => ResourceResponse::new("text/html",b"<!doctype html><html><head><link rel=stylesheet href=../styles/main.css></head><body data-relative=child></body></html>".to_vec()),
        "styles/main.css" => ResourceResponse::new("text/css",b"@import '../__day_assets/css/style.css';@import './colors.css';body{font:18px system-ui;padding:24px;background:#182236;color:white}img{width:120px;height:120px}iframe{border:0;width:120px;height:120px}".to_vec()),
        "styles/colors.css" => ResourceResponse::new("text/css",b":root{--resource-loaded:yes}body{background-image:url('../images/orb.svg');background-repeat:no-repeat;background-position:right bottom}".to_vec()),
        "images/orb.svg" => ResourceResponse::new("image/svg+xml",br##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="120"><defs><radialGradient id="g" cx="30%" cy="25%"><stop stop-color="#fff"/><stop offset=".4" stop-color="#30dcc5"/><stop offset="1" stop-color="#176caa"/></radialGradient></defs><circle cx="60" cy="60" r="55" fill="url(#g)"/></svg>"##.to_vec()),
        "scripts/main.js" => ResourceResponse::new("text/javascript",b"document.documentElement.dataset.resourceScript='yes';".to_vec()),
        _=>ResourceResponse::not_found(),
    }
    });
    browser.resource_base.set(provider.base_url());
    web_view_resources(provider, &start)
        .on_new_tab(
            res::str::open_link_new_tab().format(),
            res::str::open_link_background_tab().format(),
            move |request| {
                tabs.open_profile(
                    canonical_tab_url(browser, &request.url),
                    !request.background,
                    private,
                )
            },
        )
        .profile(demo_profile(private))
        .url_binding(browser.location)
        .js(browser.js)
        .go(browser.go)
        .back(browser.back)
        .forward(browser.forward)
        .stop(browser.stop)
        .reload(browser.reload)
        .force_reload(browser.force_reload)
        .id(tab_widget_id(tab_number, "webview"))
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
