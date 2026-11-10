// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! The content hooks reach a renderer and come back through the front-end: the request filter,
//! rule lists, scripts, styles and message names ride in `WebProps::content`; a burst of blocked
//! requests, from any thread or from the event channel, reaches `on_blocked` as one batch; a page
//! message is answered with exactly one `ScriptReply`; rule list changes are one patch each; and
//! compile errors and media requests reach their handlers.
use day_core::{BuildCx, Piece, RNode};
use day_piece_webview::{
    BlockedLoad, ContentRuleList, ContentRulesError, Frames, InjectAt, MediaRequest, PageContent,
    RequestDecision, RequestInfo, ResourceKind, ScriptMessage, UserScript, WebPatch, WebProps,
    WebRenderer, renderer, web_view,
};
use day_pieces::Decorate;
use day_reactive::Signal;
use day_spec::Support;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

thread_local! {
    /// While set, the first `on_blocked` delivery blocks this many more requests from inside the
    /// callback: a burst arriving while a delivery is under way.
    static BURST: RefCell<Option<(day_piece_webview::RequestHooks, u32)>> = const { RefCell::new(None) };
    static BUILT: RefCell<Option<(RNode, PageContent)>> = const { RefCell::new(None) };
    static PATCHES: RefCell<Vec<WebPatch>> = const { RefCell::new(Vec::new()) };
}

static HOST: renderer::surface::Host = renderer::surface::Host {
    capabilities: renderer::Capabilities {
        private_browsing: Support::Unsupported,
        profiles: Support::Unsupported,
        clear_data: Support::Unsupported,
    },
    attach: |_, node, props: &WebProps| {
        BUILT.with(|b| *b.borrow_mut() = Some((node, props.content.clone())));
    },
    update: |_, patch| PATCHES.with(|p| p.borrow_mut().push(patch.clone())),
    release: |_| {},
};

struct View(day_piece_webview::WebView);
impl Piece for View {
    fn build(self, cx: &mut BuildCx) -> RNode {
        self.0.build(cx)
    }
}

fn settle() -> Vec<WebPatch> {
    for _ in 0..8 {
        day_core::pump_events();
        day_reactive::flush_sync();
    }
    PATCHES.with(|p| p.borrow_mut().drain(..).collect())
}

/// Drive the event queue and the reactive graph until `done` holds or the budget runs out.
fn pump_until(mut done: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if done() {
            return true;
        }
        day_core::pump_events();
        day_reactive::flush_sync();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    done()
}

fn emit(event: day_spec::Event) {
    let node = BUILT
        .with(|b| b.borrow().as_ref().map(|(node, _)| *node))
        .unwrap();
    day_core::enqueue_event(day_core::rnode_to_id(node), event);
}

fn block_script(hooks: &day_piece_webview::RequestHooks, i: u32) {
    let url = format!("https://ads.test/{i}.js");
    let request = RequestInfo {
        url: &url,
        top_url: Some("https://news.test/"),
        frame_url: None,
        kind: ResourceKind::Script,
        main_frame: false,
        method: "GET",
    };
    assert_eq!(hooks.decide(&request), RequestDecision::Block);
}

fn blocks_ads(request: &RequestInfo<'_>) -> RequestDecision {
    if request.url.contains("ads.") {
        RequestDecision::Block
    } else {
        RequestDecision::Allow
    }
}

#[test]
fn content_hooks_pass_through_and_blocked_reports_coalesce() {
    let (mock, _probe) = day_mock::MockToolkit::new();
    let batches: Rc<RefCell<Vec<Vec<BlockedLoad>>>> = Rc::default();
    let errors: Rc<RefCell<Vec<ContentRulesError>>> = Rc::default();
    let media: Rc<RefCell<Vec<MediaRequest>>> = Rc::default();
    let messages: Rc<RefCell<Vec<ScriptMessage>>> = Rc::default();
    let rules = Signal::global(vec![ContentRuleList::new("easy", "1", "[]")]);
    let (b, e, m, s) = (
        batches.clone(),
        errors.clone(),
        media.clone(),
        messages.clone(),
    );
    day_core::launch_with(mock, day_spec::WindowOptions::default(), move || {
        renderer::surface::register(WebRenderer::Servo, &HOST).unwrap();
        View(
            web_view(Signal::new("https://news.test/".into()))
                .renderer(WebRenderer::Servo)
                .unwrap()
                .request_filter(Arc::new(blocks_ads))
                .on_blocked(move |loads| {
                    b.borrow_mut().push(loads.to_vec());
                    if let Some((hooks, count)) = BURST.with(|burst| burst.borrow_mut().take()) {
                        for i in 0..count {
                            block_script(&hooks, i);
                        }
                    }
                })
                .content_rules(rules)
                .on_content_rules_error(move |error| e.borrow_mut().push(error.clone()))
                .user_style("ins.adsbygoogle{display:none}")
                .user_script(UserScript::new("window.first=1"))
                .user_script(
                    UserScript::new("window.second=2")
                        .at(InjectAt::DocumentEnd)
                        .frames(Frames::Main),
                )
                .on_script_message("echo", move |message| {
                    s.borrow_mut().push(message.clone());
                    Some(message.payload.to_uppercase())
                })
                .on_media_request(move |request| {
                    m.borrow_mut().push(request.clone());
                    request.camera && !request.microphone
                }),
        )
        .any()
    });
    assert_eq!(settle(), [], "building the view sends no content patch");

    // Everything the backend needs rides in the props.
    let content = BUILT
        .with(|b| b.borrow().as_ref().map(|(_, c)| c.clone()))
        .unwrap();
    assert_eq!(content.rules, [ContentRuleList::new("easy", "1", "[]")]);
    assert_eq!(content.styles.len(), 1);
    assert_eq!(
        content.scripts,
        [
            UserScript::new("window.first=1"),
            UserScript::new("window.second=2")
                .at(InjectAt::DocumentEnd)
                .frames(Frames::Main),
        ]
    );
    assert_eq!(content.messages, ["echo"]);
    assert!(content.media_handler);
    let hooks = content.requests.clone().expect("the request filter");

    // Blocked requests arriving while a delivery is pending or under way are delivered
    // together, in one later call: here one request, then a burst of 299 during its delivery.
    // Allowed requests never appear. (Engines call the filter from
    // their own threads; the hooks are `Send + Sync`, and the crate's unit tests block from
    // worker threads. The mock toolkit runs timers and wakes at once on the calling thread, so
    // the burst is staged inside the callback.)
    fn send_sync<T: Send + Sync>(_: &T) {}
    send_sync(&hooks);
    // A view that browses anywhere filters the resource namespace like any other site
    // (provider_sites_skip_the_filter_and_browsing_views_do_not covers both cases).
    assert!(hooks.filters_internal());
    let request = RequestInfo {
        url: "https://news.test/style.css",
        top_url: None,
        frame_url: None,
        kind: ResourceKind::Stylesheet,
        main_frame: false,
        method: "GET",
    };
    assert_eq!(hooks.decide(&request), RequestDecision::Allow);
    BURST.with(|burst| *burst.borrow_mut() = Some((hooks.clone(), 299)));
    block_script(&hooks, 1000);
    assert!(pump_until(|| batches.borrow().len() >= 2), "no delivery");
    settle();
    let sizes: Vec<usize> = batches.borrow().iter().map(Vec::len).collect();
    assert_eq!(sizes, [1, 299], "the burst arrives as one batch");
    assert!(
        batches
            .borrow()
            .iter()
            .flatten()
            .all(|load| load.kind == ResourceKind::Script && load.page_url == "https://news.test/")
    );
    batches.borrow_mut().clear();

    // Engines that block on their own report through the event channel, batched alike.
    let load = |n: u32| BlockedLoad {
        url: format!("https://tracker.test/{n}"),
        page_url: "https://news.test/".into(),
        kind: ResourceKind::Image,
    };
    emit(renderer::blocked_event(&[load(1), load(2)]));
    assert!(pump_until(|| !batches.borrow().is_empty()), "no delivery");
    assert_eq!(batches.borrow()[0], [load(1), load(2)]);

    // A page message gets exactly one reply, under the backend's id; an unknown name gets none.
    let message = |name: &str, payload: &str| ScriptMessage {
        name: name.into(),
        payload: payload.into(),
        origin: "https://news.test".into(),
        main_frame: true,
    };
    emit(renderer::script_message_event(7, message("echo", "hi")));
    assert_eq!(
        settle(),
        [WebPatch::ScriptReply {
            id: 7,
            reply: Some("HI".into())
        }]
    );
    assert_eq!(messages.borrow()[0], message("echo", "hi"));
    emit(renderer::script_message_event(8, message("unknown", "x")));
    assert_eq!(settle(), [WebPatch::ScriptReply { id: 8, reply: None }]);

    // Rule list changes are one patch each; unchanged lists send nothing.
    let next = vec![
        ContentRuleList::new("easy", "2", "[]"),
        ContentRuleList::new("privacy", "1", "[]"),
    ];
    rules.set(next.clone());
    assert_eq!(settle(), [WebPatch::ContentRules(next.clone())]);
    rules.set(next);
    assert_eq!(settle(), []);
    emit(renderer::content_rules_error_event("privacy", "bad regex"));
    settle();
    assert_eq!(
        *errors.borrow(),
        [ContentRulesError {
            id: "privacy".into(),
            message: "bad regex".into()
        }]
    );

    // Camera and microphone requests go to the app's handler.
    let request = MediaRequest {
        origin: "https://meet.test".into(),
        camera: true,
        microphone: false,
    };
    emit(renderer::media_event(3, request.clone()));
    assert_eq!(settle(), [WebPatch::MediaDecision { id: 3, allow: true }]);
    assert_eq!(*media.borrow(), [request]);
    day_core::uninstall_tree();
}

#[test]
fn a_view_without_hooks_carries_empty_content() {
    let (mock, _probe) = day_mock::MockToolkit::new();
    day_core::launch_with(mock, day_spec::WindowOptions::default(), move || {
        renderer::surface::register(WebRenderer::Servo, &HOST).unwrap();
        View(
            web_view(Signal::new("about:blank".into()))
                .renderer(WebRenderer::Servo)
                .unwrap(),
        )
        .any()
    });
    assert_eq!(settle(), []);
    let content = BUILT
        .with(|b| b.borrow().as_ref().map(|(_, c)| c.clone()))
        .unwrap();
    assert_eq!(content, PageContent::default());
    day_core::uninstall_tree();
}

/// The resource namespace is the app's own site in a view showing a provider's pages, so the
/// filter never sees it there; a view that browses anywhere and borrows a provider for some
/// pages filters those pages like any other site. Every engine follows these hooks.
#[test]
fn provider_sites_skip_the_filter_and_browsing_views_do_not() {
    use day_piece_webview::{ResourceProvider, ResourceResponse, web_view_resources};
    fn hooks_of(view: day_piece_webview::WebView) -> day_piece_webview::RequestHooks {
        let (mock, _probe) = day_mock::MockToolkit::new();
        day_core::launch_with(mock, day_spec::WindowOptions::default(), move || {
            renderer::surface::register(WebRenderer::Servo, &HOST).unwrap();
            View(
                view.renderer(WebRenderer::Servo)
                    .unwrap()
                    .request_filter(Arc::new(blocks_ads)),
            )
            .any()
        });
        settle();
        BUILT
            .with(|b| b.borrow_mut().take())
            .and_then(|(_, content)| content.requests)
            .expect("the request filter")
    }
    let provider = || ResourceProvider::new(|_| ResourceResponse::new("text/css", &b""[..]));
    let ads = |base: &str| format!("{base}ads.css");
    let decide = |hooks: &day_piece_webview::RequestHooks, url: &str| {
        hooks.decide(&RequestInfo {
            url,
            top_url: Some("https://news.test/"),
            frame_url: None,
            kind: ResourceKind::Stylesheet,
            main_frame: false,
            method: "GET",
        })
    };

    let site = provider();
    let base = site.base_url();
    let own = hooks_of(web_view_resources(site, "index.html"));
    assert!(!own.filters_internal());
    assert_eq!(decide(&own, &ads(&base)), RequestDecision::Allow);
    for namespace in ["https://day-resource.invalid/1/", "day-resource://p1/"] {
        assert_eq!(decide(&own, &ads(namespace)), RequestDecision::Allow);
    }
    // Other sites still go to the filter.
    assert_eq!(
        decide(&own, "https://ads.test/1.js"),
        RequestDecision::Block
    );

    let borrowed = provider();
    let base = borrowed.base_url();
    let browsing = hooks_of(web_view(Signal::new("https://news.test/".into())).resources(borrowed));
    assert!(browsing.filters_internal());
    assert_eq!(decide(&browsing, &ads(&base)), RequestDecision::Block);
    assert_eq!(
        decide(&browsing, "https://news.test/a.css"),
        RequestDecision::Allow
    );
}
