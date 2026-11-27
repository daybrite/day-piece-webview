// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

use super::{WebMenuAction, WebMenuItem, WebMenuTarget, browsing_apple, context_menu};
use block2::RcBlock;
use day_spec::{Event, NodeId};
use objc2::{msg_send, runtime::AnyObject};
use objc2_foundation::{NSError, NSString, NSURL, NSURLRequest};

#[derive(serde::Deserialize)]
pub(crate) struct Hit {
    pub(crate) document: String,
    pub(crate) link: String,
    pub(crate) image: String,
}

impl Hit {
    pub(crate) fn action(&self, item: &WebMenuItem) -> Option<WebMenuAction> {
        let url = match item.target {
            WebMenuTarget::Link => &self.link,
            WebMenuTarget::Image | WebMenuTarget::DownloadImage => &self.image,
        };
        context_menu::web_address(url).then(|| WebMenuAction {
            id: item.id.clone(),
            document: self.document.clone(),
            url: url.clone(),
        })
    }
}

/// WebKit may supply a synthesized `NSEvent` to `willOpenMenu`. Capture the DOM target
/// of the real gesture in the isolated client world before the native menu opens.
pub(crate) fn install(config: &AnyObject, standalone_images: bool) {
    let mut source = String::from(
        r"
        const capture = event => {
            if (event.isTrusted) globalThis.__dayWebMenuHit = {element:event.composedPath()[0],time:performance.now()};
        };
        addEventListener('contextmenu',capture,true);
        addEventListener('pointerdown',event=>{
            if (event.button===2 || event.ctrlKey || event.pointerType==='touch') capture(event);
        },true);
        addEventListener('touchstart',capture,{capture:true,passive:true});
        addEventListener('keydown',event=>{
            if (event.key==='ContextMenu' || (event.shiftKey && event.key==='F10')) capture(event);
        },true);
        addEventListener('blur',()=>{globalThis.__dayWebMenuHit=null});
    ",
    );
    if standalone_images {
        source.push_str(r"addEventListener('DOMContentLoaded',()=>{
            const css=document.createElement('style');
            css.textContent='img{-webkit-touch-callout:none}a[href] img{-webkit-touch-callout:default}';
            document.head?.append(css);
        },{once:true});");
    }
    unsafe {
        let world: &AnyObject = msg_send![objc2::class!(WKContentWorld), defaultClientWorld];
        let raw: *mut AnyObject = msg_send![objc2::class!(WKUserScript), alloc];
        let raw: *mut AnyObject = msg_send![raw,initWithSource:&*NSString::from_str(&source),injectionTime:0isize,forMainFrameOnly:true,inContentWorld:world];
        let script = objc2::rc::Retained::from_raw(raw).expect("WKUserScript init");
        let controller: objc2::rc::Retained<AnyObject> = msg_send![config, userContentController];
        let _: () = msg_send![&*controller,addUserScript:&*script];
    }
}

/// Hit-test in the client world. The host point is normalized so page zoom and the
/// visual viewport use the same CSS coordinates as elementFromPoint.
pub(crate) fn hit(
    web: &AnyObject,
    point: Option<(f64, f64)>,
    reply: impl Fn(Option<Hit>) + 'static,
) {
    let fallback = point.map_or_else(|| "document.activeElement".to_owned(), |(x,y)| {
        format!("document.elementFromPoint({x}*(visualViewport?.width || innerWidth),{y}*(visualViewport?.height || innerHeight))")
    });
    let js = format!(
        r"(() => {{
        const saved = globalThis.__dayWebMenuHit;
        const el = saved?.element?.isConnected && performance.now()-saved.time < 2500 ? saved.element : ({fallback});
        if (!el || el.isContentEditable || el.closest('input,textarea')) return null;
        const img = el.closest('img') || (el === document.activeElement && el.querySelector('img'));
        return JSON.stringify({{document:location.href,link:el.closest('a[href]')?.href || '',image:img?.currentSrc || img?.src || ''}});
    }})()"
    );
    let done = RcBlock::new(move |value: *mut AnyObject, _error: *mut NSError| {
        let hit = unsafe { value.as_ref() }
            .and_then(|v| v.downcast_ref::<NSString>())
            .and_then(|v| serde_json::from_str(&v.to_string()).ok());
        reply(hit);
    });
    unsafe {
        let world: &AnyObject = msg_send![objc2::class!(WKContentWorld), defaultClientWorld];
        let _: () = msg_send![web,evaluateJavaScript:&*NSString::from_str(&js),inFrame:std::ptr::null::<AnyObject>(),inContentWorld:world,completionHandler:&*done];
    }
}

pub(crate) fn select(
    web: &AnyObject,
    id: NodeId,
    emit: fn(NodeId, Event),
    target: WebMenuTarget,
    action: &WebMenuAction,
) {
    let current: Option<objc2::rc::Retained<NSURL>> = unsafe { msg_send![web, URL] };
    if current
        .and_then(|u| u.absoluteString())
        .is_none_or(|u| u.to_string() != action.document)
    {
        return;
    }
    if target == WebMenuTarget::DownloadImage {
        let Some(url) = NSURL::URLWithString(&NSString::from_str(&action.url)) else {
            return;
        };
        let request = NSURLRequest::requestWithURL(&url);
        let started = RcBlock::new(move |download: &AnyObject| {
            browsing_apple::adopt_download(emit, download, id);
        });
        unsafe {
            let _: () =
                msg_send![web, startDownloadUsingRequest:&*request,completionHandler:&*started];
        }
    } else {
        emit(
            id,
            context_menu::event(&action.id, &action.url, &action.document),
        );
    }
}
