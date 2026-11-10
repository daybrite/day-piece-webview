// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! WebKit content rule lists end to end on macOS: a list compiles into its own store (never the
//! user's default one), a second request finds the compiled list instead of compiling again, a
//! broken list reports its error, a real WKWebView with the list attached loads one image and is
//! refused the other, and a navigation that arrives while a view's first page waits for its lists
//! replaces that page and loads under the lists.
//!
//! WKWebView and the rule list store answer on the main thread's run loop, which a test thread
//! never runs, so this test has its own `main` (`harness = false` in Cargo.toml).

#[cfg(target_os = "macos")]
fn main() {
    use day_piece_webview::__webkit_content_probe as probe;
    use day_piece_webview::ContentRuleList;
    use std::time::{Duration, Instant};

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *const std::ffi::c_void;
        fn CFRunLoopRunInMode(mode: *const std::ffi::c_void, seconds: f64, once: u8) -> i32;
    }
    // WKWebView's class must be registered before the probe asks for it.
    #[link(name = "WebKit", kind = "framework")]
    unsafe extern "C" {}

    let spin_until = |done: &dyn Fn() -> bool, tick: &dyn Fn()| {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done() && Instant::now() < deadline {
            tick();
            // SAFETY: the main thread's own run loop, in the default mode.
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 0) };
        }
        done()
    };

    let root = std::env::temp_dir().join(format!("day-webview-rules-{}", std::process::id()));
    let store = root.join("store");
    let site = root.join("site");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::create_dir_all(&site).unwrap();
    // A 1×1 transparent PNG, served twice under two names.
    const PIXEL: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    std::fs::write(site.join("ok.png"), PIXEL).unwrap();
    std::fs::write(site.join("blocked.png"), PIXEL).unwrap();
    std::fs::write(
        site.join("index.html"),
        "<!doctype html><title></title><script>const r={};function done(k,v){r[k]=v;\
         if(Object.keys(r).length===2)document.title='ok:'+r.ok+' blocked:'+r.blocked;}</script>\
         <img src='ok.png' onload=\"done('ok','load')\" onerror=\"done('ok','error')\">\
         <img src='blocked.png' onload=\"done('blocked','load')\" onerror=\"done('blocked','error')\">",
    )
    .unwrap();
    let lists = vec![ContentRuleList::new(
        "fixture",
        "1",
        r#"[{"trigger":{"url-filter":"blocked\\.png"},"action":{"type":"block"}}]"#,
    )];

    // Compile, attach, load: WebKit refuses the blocked image and loads the other.
    let first = probe::start(&store, &lists, &site.join("index.html"));
    let loaded = spin_until(&|| !first.borrow().title.is_empty(), &|| {
        probe::read_title(&first)
    });
    let compiled = first.borrow().compiled.clone();
    assert_eq!(compiled, Some(Ok(1)), "the list compiles");
    assert!(loaded, "the page never finished loading its images");
    assert_eq!(first.borrow().title, "ok:load blocked:error");

    // The same id and version again: found in the store, not compiled again.
    let start = Instant::now();
    let second = probe::start(&store, &lists, &site.join("index.html"));
    assert!(spin_until(&|| second.borrow().compiled.is_some(), &|| {}));
    assert_eq!(second.borrow().compiled, Some(Ok(1)));
    println!("lookup of a compiled list took {:?}", start.elapsed());

    // A list WebKit cannot parse reports an error under its own id.
    let broken = vec![ContentRuleList::new("broken", "1", "[{\"trigger\":{}}]")];
    let third = probe::start(&store, &broken, &site.join("index.html"));
    assert!(spin_until(&|| third.borrow().compiled.is_some(), &|| {}));
    match third.borrow().compiled.clone() {
        Some(Err(errors)) => {
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].id, "broken");
            assert!(!errors[0].message.is_empty());
        }
        other => panic!("a broken list compiled: {other:?}"),
    }

    // A navigation while the first page waits for its lists replaces that page: the view ends
    // on the page asked for last, loaded under the lists, never on the startup page. A fresh
    // store, so the lists really compile while the navigation arrives.
    std::fs::write(
        site.join("initial.html"),
        "<!doctype html><title>Initial</title>",
    )
    .unwrap();
    let fresh = root.join("fresh-store");
    std::fs::create_dir_all(&fresh).unwrap();
    let navigated = probe::navigate_while_compiling(
        &fresh,
        &lists,
        &site.join("initial.html"),
        &site.join("index.html"),
    );
    assert_eq!(
        navigated.borrow().compiled,
        Some(Ok(1)),
        "the navigation arrived after the first page was released"
    );
    let finished = spin_until(&|| navigated.borrow().title.starts_with("ok:"), &|| {
        probe::read_title(&navigated)
    });
    assert!(
        finished,
        "the view never reached the requested page (title {:?})",
        navigated.borrow().title
    );
    // Nothing loads over it afterwards.
    let settle = Instant::now() + Duration::from_millis(1500);
    spin_until(&|| Instant::now() >= settle, &|| {
        probe::read_title(&navigated)
    });
    assert_eq!(navigated.borrow().title, "ok:load blocked:error");

    let _ = std::fs::remove_dir_all(&root);
    println!("content-rules-macos: ok");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
