// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Browser behavior shipped with the piece, registered by Day's generic JavaScript bridge.

day_bridge::bridge! {
    #[day_bridge::declare]
    extern "day" {
        fn attach_browser(id: i32, base: &str);
        fn resource_namespace() -> Result<f64, day_bridge::Error>;
        fn attach_resources(id: i32, base: &str, start: &str);
        fn resource_reply(token: i32, status: i32, headers: &str, body: &[u8]);
        fn eval_browser(id: i32, req: f64, script: &str);
    }

    #[day_bridge::impl(js, platforms = [web])]
    js!(r#"
        const randomId = crypto.getRandomValues(new Uint32Array(2));
        const namespaceId = String(randomId[0] * 2097152 + (randomId[1] >>> 11));
        let resourceToken=0;
        const resourcePorts=new Map();
        function resource_namespace() { return Number(namespaceId); }
        navigator.serviceWorker?.addEventListener('message',event=>{
            const r=event.data;
            if(r?.type!=='day-resource' || r.namespace!==namespaceId)return;
            const token=++resourceToken;
            resourcePorts.set(token,event.ports[0]);
            try {dayHost.exports().day_web_resource_browser(token,r.provider,...dayHost.intoWasm(JSON.stringify(r)));}
            catch(error){resource_reply(token,500,'[]',new Uint8Array());console.error(error);}
        });
        function resource_reply(token,status,headers,body) {
            const port=resourcePorts.get(token);resourcePorts.delete(token);
            if(!port)return;
            const bytes=body.slice();
            port.postMessage({status,headers:JSON.parse(headers),body:bytes},[bytes.buffer]);port.close();
        }
        async function attach_resources(id,base,start) {
            const frame=dayHost.dom.element(id);
            let alive=true;dayHost.dom.onRelease(id,()=>{alive=false});
            try {
                const root=new URL('assets/data/day-piece-webview/',document.baseURI);
                const reg=await navigator.serviceWorker.register(new URL('resource-worker.js',root),{scope:root.pathname});
                const worker=reg.installing||reg.waiting||reg.active;
                if(worker.state!=='activated') await new Promise((resolve,reject)=>{
                    const changed=()=>{
                        if(worker.state==='activated'||worker.state==='redundant'){
                            worker.removeEventListener('statechange',changed);
                            worker.state==='activated'?resolve():reject(new Error('Resource worker failed'));
                        }
                    };
                    worker.addEventListener('statechange',changed);
                    changed();
                });
                if(alive)frame.src=new URL(start,document.baseURI).href;
            }catch(error){if(alive)dayHost.dom.emit(id,-2,String(error));}
        }
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
    fn resource_namespace() -> Result<f64, day_bridge::Error> { Ok(0.0) }
    #[day_bridge::impl(rust, platforms = [other])]
    fn attach_resources(id:i32,base:&str,start:&str) {let _=(id,base,start);}
    #[day_bridge::impl(rust, platforms = [other])]
    fn resource_reply(token:i32,status:i32,headers:&str,body:&[u8]) {let _=(token,status,headers,body);}
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

pub(crate) fn namespace() -> String {
    resource_namespace()
        .expect("resource namespace")
        .to_string()
}
pub(crate) fn resources(id: i32, base: &str, start: &str) {
    attach_browser(id, base);
    attach_resources(id, base, start);
}
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn day_web_resource_browser(
    token: i32,
    provider: u32,
    ptr: *mut u8,
    len: usize,
) {
    let bytes = unsafe { day_bridge::__take_wasm(ptr, len) };
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    let r = crate::resources::respond(
        provider as u64,
        value["url"].as_str().unwrap_or(""),
        value["method"].as_str().unwrap_or(""),
        value["range"].as_str().unwrap_or(""),
    );
    let mut headers = r.headers.clone();
    headers.push(("Content-Type".into(), r.mime.clone()));
    resource_reply(
        token,
        r.status as i32,
        &serde_json::to_string(&headers).unwrap(),
        &r.body,
    );
}
