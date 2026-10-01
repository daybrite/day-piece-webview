// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! WKURLSchemeHandler shared by AppKit, UIKit and macOS GTK.
use super::resources;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSData, NSString, NSURLRequest};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
struct Ivars {
    id: u64,
    tasks: Rc<RefCell<HashMap<usize, day_core::TaskHandle>>>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DayResourceSchemeHandler"]
    #[ivars = Ivars]
    struct Handler;
    unsafe impl NSObjectProtocol for Handler {}
    impl Handler {
        #[unsafe(method(webView:startURLSchemeTask:))]
        fn start(&self, _web:&AnyObject, task:&AnyObject) {
            let request:Retained<NSURLRequest> = unsafe {msg_send![task, request]};
            let Some(url)=request.URL() else {return};
            let text=url.absoluteString().map(|s|s.to_string()).unwrap_or_default();
            let method=request.HTTPMethod().map(|s|s.to_string()).unwrap_or_else(||"GET".into());
            let range=request.valueForHTTPHeaderField(&NSString::from_str("Range")).map(|s|s.to_string()).unwrap_or_default();
            let key=task as *const _ as usize;
            let task=unsafe { Retained::retain(task as *const _ as *mut AnyObject) }.unwrap();
            let id=self.ivars().id;
            let (tx,rx)=day_async::oneshot();
            resources::background(move ||tx.send(resources::respond(id,&text,&method,&range)));
            let tasks=Rc::downgrade(&self.ivars().tasks);
            let handle=day_core::task(async move {
                let Ok(r)=rx.await else{return};
                // Removing the live entry is the cancellation gate. WebKit forbids answering a
                // stopped task; stopURLSchemeTask aborts this future before it can respond.
                if let Some(tasks)=tasks.upgrade(){tasks.borrow_mut().remove(&key);}else{return}
                unsafe {
                    let dict:Retained<AnyObject>=msg_send![objc2::class!(NSMutableDictionary), new];
                    for (k,v) in r.headers.iter().chain(std::iter::once(&("Content-Type".into(),r.mime.clone()))) {
                        let _:()=msg_send![&dict, setObject:&*NSString::from_str(v), forKey:&*NSString::from_str(k)];
                    }
                    let response:objc2::rc::Allocated<AnyObject>=msg_send![objc2::class!(NSHTTPURLResponse), alloc];
                    let response:Retained<AnyObject>=msg_send![response, initWithURL:&*url, statusCode:r.status as isize, HTTPVersion:&*NSString::from_str("HTTP/1.1"), headerFields:&*dict];
                    let _:()=msg_send![&task, didReceiveResponse:&*response];
                    // NSData copies once into WebKit's response, avoiding base64/JS parsing.
                    let data=NSData::with_bytes(&r.body);
                    let _:()=msg_send![&task, didReceiveData:&*data];
                    let _:()=msg_send![&task, didFinish];
                }
            });
            if !handle.is_finished(){ self.ivars().tasks.borrow_mut().insert(key,handle); }
        }
        #[unsafe(method(webView:stopURLSchemeTask:))]
        fn stop(&self,_web:&AnyObject,task:&AnyObject) {
            let handle=self.ivars().tasks.borrow_mut().remove(&(task as *const _ as usize));
            if let Some(handle)=handle { handle.abort(); }
        }
    }
);
pub(crate) fn configuration(id: Option<u64>, mtm: MainThreadMarker) -> Retained<AnyObject> {
    unsafe {
        let config: Retained<AnyObject> = msg_send![objc2::class!(WKWebViewConfiguration), new];
        // Public WKPreferences API (macOS 12.3 / iOS 15.4). Guard older deployments.
        let preferences: Retained<AnyObject> = msg_send![&config, preferences];
        let fullscreen = objc2::sel!(setElementFullscreenEnabled:);
        let supported: bool = msg_send![&preferences, respondsToSelector: fullscreen];
        if supported {
            let _: () = msg_send![&preferences, setElementFullscreenEnabled: true];
        }
        if let Some(id) = id {
            let handler = Handler::alloc(mtm).set_ivars(Ivars {
                id,
                tasks: Rc::new(RefCell::new(HashMap::new())),
            });
            let handler: Retained<Handler> = msg_send![super(handler), init];
            let _: () = msg_send![&config,setURLSchemeHandler:&*handler,forURLScheme:&*NSString::from_str("day-resource")];
        }
        config
    }
}
