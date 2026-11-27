// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! Exercise native `WKWebView` menu tracking, target/action dispatch and download bytes.
//! AppKit requires a main-thread event loop, hence the standalone test harness.
#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    mac::run();
}

#[cfg(target_os = "macos")]
mod mac {
    use block2::RcBlock;
    use day_piece_webview::{DownloadEvent, WebMenuItem, WebMenuTarget, WebProfile, web_view};
    use objc2::{MainThreadMarker, msg_send, rc::Retained, runtime::AnyObject};
    use objc2_app_kit::{
        NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSMenu,
        NSMenuDidBeginTrackingNotification, NSView,
    };
    use objc2_foundation::{NSPoint, NSRunLoopCommonModes, NSString};
    use objc2_web_kit::WKWebView;
    use std::{
        cell::Cell,
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        rc::Rc,
        time::Duration,
    };

    // Synthetic SVG is intentionally distinct from both its link and its src fallback.
    const IMAGE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect width="100" height="100" fill="blue"/></svg>"#;
    const LABELS: [&str; 3] = ["Fixture link", "Fixture image", "Fixture download"];

    fn serve() -> String {
        let server = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut stream in server.incoming().flatten() {
                let mut reader = BufReader::new(&stream);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                }
                let (status, mime, cookie, data): (&str, &str, &str, &[u8]) = if first
                    .starts_with("GET /article ")
                {
                    ("200 OK","text/html","Set-Cookie: fixture=menu; HttpOnly; Path=/\r\n",
                     br#"<body style="margin:0"><a href="/story"><picture><source srcset="/displayed.svg"><img src="/fallback.svg" width="150" height="150"></picture></a></body>"#)
                } else if first.starts_with("GET /displayed.svg ")
                    && headers
                        .to_ascii_lowercase()
                        .contains("cookie: fixture=menu")
                {
                    ("200 OK", "image/svg+xml", "", IMAGE)
                } else {
                    ("404 Not Found", "text/plain", "", b"fixture miss")
                };
                write!(stream,"HTTP/1.1 {status}\r\n{cookie}Content-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",data.len()).unwrap();
                let _ = stream.write_all(data);
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    /// Also runs while the native menu's nested tracking loop is active.
    fn later(seconds: f64, f: impl Fn() + 'static) {
        let block = RcBlock::new(move |_timer: &AnyObject| f());
        unsafe {
            let timer: Retained<AnyObject> = msg_send![objc2::class!(NSTimer),timerWithTimeInterval:seconds,repeats:false,block:&*block];
            let run_loop: &AnyObject = msg_send![objc2::class!(NSRunLoop), mainRunLoop];
            let _: () = msg_send![run_loop,addTimer:&*timer,forMode:NSRunLoopCommonModes];
        }
    }

    fn find_web(view: Retained<NSView>) -> Option<Retained<NSView>> {
        if view.downcast_ref::<WKWebView>().is_some() {
            return Some(view);
        }
        view.subviews().iter().find_map(find_web)
    }

    fn right_click() {
        let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
        let web = app
            .windows()
            .iter()
            .filter_map(|w| w.contentView())
            .find_map(find_web)
            .unwrap();
        let window = web.window().unwrap();
        let y = if web.isFlipped() {
            40.0
        } else {
            web.bounds().size.height - 40.0
        };
        let pt = web.convertPoint_toView(NSPoint::new(40.0, y), None);
        let event=NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::RightMouseDown,pt,NSEventModifierFlags::empty(),objc2_foundation::NSProcessInfo::processInfo().systemUptime(),window.windowNumber(),None,0,1,1.0
        ).unwrap();
        window.sendEvent(&event);
    }

    fn choose(menu: Retained<NSMenu>, label: &'static str, retries: u8) {
        let name = NSString::from_str(label);
        if let Some(item) = menu.itemWithTitle(&name)
            && !item.isHidden()
            && item.isEnabled()
        {
            assert!(
                menu.indexOfItemWithTag(6) >= 0,
                "WebKit Copy Image must survive augmentation"
            );
            menu.performActionForItemAtIndex(menu.indexOfItem(&item));
            menu.cancelTracking();
        } else {
            if retries == 1 {
                for i in &menu.itemArray() {
                    eprintln!(
                        "{} hidden={} enabled={}",
                        i.title(),
                        i.isHidden(),
                        i.isEnabled()
                    );
                }
            }
            assert!(retries > 0, "missing active native command {label}");
            later(0.05, move || choose(menu.clone(), label, retries - 1));
        }
    }

    pub fn run() {
        let origin = serve();
        let count = Rc::new(Cell::new(0_usize));
        let progress = count.clone();
        let handler = RcBlock::new(move |note: &AnyObject| {
            let menu: Retained<NSMenu> = unsafe { msg_send![note, object] };
            let label = LABELS[progress.get()];
            later(0.1, move || choose(menu.clone(), label, 40));
        });
        unsafe {
            let center: &AnyObject = msg_send![objc2::class!(NSNotificationCenter), defaultCenter];
            let _: Retained<AnyObject> = msg_send![center,addObserverForName:NSMenuDidBeginTrackingNotification,object:std::ptr::null::<AnyObject>(),queue:std::ptr::null::<AnyObject>(),usingBlock:&*handler];
        }
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(25));
            eprintln!("native context-menu test timed out");
            std::process::exit(1);
        });
        day_core::launch_with(
            day_toolkit_appkit::AppKit::new(),
            day_spec::WindowOptions::default(),
            move || {
                let page = format!("{origin}/article");
                let expected_doc = page.clone();
                let count_open = count.clone();
                let dest = std::env::temp_dir()
                    .join(format!("day-menu-fixture-{}.svg", std::process::id()));
                let article = web_view(day_reactive::Signal::new(page))
                .profile(WebProfile::private("menu-fixture"))
                .on_load(|| later(0.3,right_click))
                .web_context_menu([
                    WebMenuItem::new("link",LABELS[0],WebMenuTarget::Link),
                    WebMenuItem::new("image",LABELS[1],WebMenuTarget::Image),
                    WebMenuItem::new("download",LABELS[2],WebMenuTarget::DownloadImage),
                ], move |event| {
                    assert_eq!(event.document,expected_doc);
                    let step=count_open.get();
                    let (id,path)=[("link","/story"),("image","/displayed.svg")][step];
                    assert_eq!(event.id,id); assert_eq!(event.url,format!("{origin}{path}"));
                    count_open.set(step+1); later(1.2,right_click);
                })
                .on_download(move |request,reply|{
                    assert_eq!(count.get(),2);assert!(request.url.ends_with("/displayed.svg"));
                    let file=dest.clone();
                    reply.save_to(&dest,move |event|match event {
                        DownloadEvent::Finished{..}=>{
                            assert_eq!(std::fs::read(&file).unwrap(),IMAGE);
                            std::fs::remove_file(&file).unwrap();
                            println!("Verified native link, displayed image, Copy Image, and authenticated download");
                            std::process::exit(0);
                        }
                        DownloadEvent::Failed(message)=>panic!("download failed: {message}"),
                        DownloadEvent::Progress{..}=>{}
                    });
                });
                day_core::AnyPiece::new(article)
            },
        );
    }
}
