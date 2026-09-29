// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Browser behavior shipped with the piece, registered by Day's generic JavaScript bridge.

day_bridge::bridge! {
    #[day_bridge::declare]
    extern "day" {
        fn attach_browser(id: i32, base: &str);
        fn eval_browser(id: i32, req: f64, script: &str);
    }

    #[day_bridge::impl(js, platforms = [web])]
    js!(r#"
        function eval_browser(id, req, script) {
            let reply;
            try {
                const frame = dayHost.dom.element(id);
                if (!frame?.contentDocument) throw new Error('Frame is unavailable or cross-origin');
                reply = frame.contentWindow.eval(script);
                if (typeof reply !== 'string') throw new Error('Non-string evaluation reply');
            } catch (error) {
                reply = '0\u001f' + (error.name || 'Error') + '\u001f' + error.message;
            }
            dayHost.dom.emit(id, req, reply);
        }
        function attach_browser(id, base) {
            const frame = dayHost.dom.element(id);
            const site = new URL(base, document.baseURI);
            let current = null;
            function clicked(event) {
                const anchor = event.target?.closest?.('a[href]');
                if (!anchor) return;
                let url;
                try { url = new URL(anchor.getAttribute('href'), current.baseURI); }
                catch { return; }
                if ((url.origin === site.origin && url.pathname.startsWith(site.pathname))
                    || url.href === 'about:blank') return;
                event.preventDefault();
                dayHost.dom.emit(id, -1, url.href);
            }
            function detachDocument() {
                current?.removeEventListener('click', clicked, { capture: true });
                current = null;
            }
            function loaded() {
                detachDocument();
                try { current = frame.contentDocument; } catch { return; }
                current?.addEventListener('click', clicked, { capture: true });
            }
            frame.addEventListener('load', loaded);
            dayHost.dom.onRelease(id, () => {
                frame.removeEventListener('load', loaded);
                detachDocument();
            });
        }
    "#);
    #[day_bridge::impl(rust, platforms = [other])]
    fn attach_browser(id: i32, base: &str) { let _ = (id, base); }
    #[day_bridge::impl(rust, platforms = [other])]
    fn eval_browser(id: i32, req: f64, script: &str) { let _ = (id, req, script); }
}

pub(crate) fn attach(id: i32, base: &str) {
    attach_browser(id, base);
}

pub(crate) fn eval(id: i32, req: f64, script: &str) {
    eval_browser(id, req, script);
}
