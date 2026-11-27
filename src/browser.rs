// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Browser behavior shipped with the piece, registered by Day's generic JavaScript bridge.

day_bridge::bridge! {
    #[day_bridge::declare]
    extern "day" {
        fn attach_browser(id: i32, base: &str);
        fn observe_browser_load(id: i32);
        fn attach_tabs(id: i32, foreground: &str, background: &str);
        fn attach_menu(id:i32, model:&str, foreground:&str, background:&str);
        fn resource_namespace() -> Result<f64, day_bridge::Error>;
        fn attach_resources(id: i32, base: &str, start: &str);
        fn resource_reply(token: i32, status: i32, headers: &str, body: &[u8]);
        fn eval_browser(id: i32, req: f64, script: &str);
        fn zoom_browser(id: i32, zoom: f64);
        fn scripts_browser(id: i32, enabled: bool);
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
        // WebView::zoom: CSS `zoom` on the frame's document, now and after each load. Only a
        // same-origin document lets its parent set it; a cross-origin one keeps 100%.
        function zoom_browser(id, zoom) {
            const frame = dayHost.dom.element(id);
            frame.__dayZoom = zoom;
            const apply = () => {
                try {
                    const root = frame.contentDocument?.documentElement;
                    if (root) root.style.zoom = frame.__dayZoom === 1 ? '' : String(frame.__dayZoom);
                } catch {}
            };
            if (!frame.__dayZoomHooked) {
                frame.__dayZoomHooked = true;
                frame.addEventListener('load', apply);
                dayHost.dom.onRelease(id, () => frame.removeEventListener('load', apply));
            }
            apply();
        }
        // WebView::javascript: a sandbox granting everything an unsandboxed frame has except
        // scripts, or none at all. It applies from the frame's next navigation on.
        function scripts_browser(id, enabled) {
            const frame = dayHost.dom.element(id);
            if (enabled) frame.removeAttribute('sandbox');
            else frame.setAttribute('sandbox', 'allow-same-origin allow-forms allow-popups ' +
                'allow-popups-to-escape-sandbox allow-modals allow-downloads ' +
                'allow-top-navigation-by-user-activation');
        }
        function observe_browser_load(id) {
            const frame = dayHost.dom.element(id);
            function loaded() {
                let url = frame.src || 'about:blank';
                try { url = frame.contentWindow.location.href; } catch {}
                // Resource-backed frames initially load an internal blank document
                // while their worker starts. It is not the requested page's load.
                if (url === 'about:blank' && frame.src !== 'about:blank') return;
                dayHost.dom.emit(id, 0, url);
                // Page state (num -5): what a frame can tell its parent, the URL and, same-origin
                // only, the title. An iframe exposes no progress or history across origins.
                let title = '';
                try { title = frame.contentDocument?.title || ''; } catch {}
                dayHost.dom.emit(id, -5, '000\u001f1\u001f' + url + '\u001f' + title);
            }
            frame.addEventListener('load', loaded);
            dayHost.dom.onRelease(id, () => frame.removeEventListener('load', loaded));
        }
        function attach_menu(id, model, foreground, background) {
            const entries=JSON.parse(model);
            const frame=dayHost.dom.element(id);
            let cleanup=()=>{};
            function loaded() {
                cleanup();
                let doc;
                try { doc=frame.contentDocument; } catch { return; }
                if (!doc) return;
                let panel=null, previous=null;
                function close(restore) {
                    if (!panel) return;
                    panel.remove(); panel=null;
                    document.removeEventListener('pointerdown',outside,true);
                    doc.removeEventListener('pointerdown',outside,true);
                    if (restore && previous?.isConnected) previous.focus({preventScroll:true});
                }
                const outside=e=>{if (!panel?.contains(e.target)) close(false);};
                function show(e) {
                    if (!e.isTrusted || e.shiftKey || !frame.isConnected) return;
                    const el=e.target;
                    if (el?.isContentEditable || el?.closest?.('input,textarea') || !doc.getSelection()?.isCollapsed) return;
                    const link=el?.closest?.('a[href]')?.href;
                    const image=el?.closest?.('img') || (el===doc.activeElement && el.querySelector?.('img'));
                    const source=image?.currentSrc||image?.src;
                    const allowed=value=>{
                        if (!value) return false;
                        try { const u=new URL(value);return ['http:','https:'].includes(u.protocol)&&!u.username&&!u.password&&!['day-resource.invalid','day-assets.example'].includes(u.hostname)&&!(u.origin===doc.location.origin && u.pathname.includes('/assets/data/day-piece-webview/')); }
                        catch {return false;}
                    };
                    // Only the browser's own menu can reliably save a cross-origin image.
                    if (allowed(source) && new URL(source).origin!==doc.location.origin &&
                        entries.some(item=>item.target==='download-image')) return;
                    const documentUrl=doc.location.href;
                    const actions=[];
                    for (const item of entries) {
                        const url=item.target==='link'?link:source;
                        if (!allowed(url)) continue;
                        if (item.target==='download-image') {
                            // A cross-origin download attribute can navigate instead of saving.
                            if (new URL(url).origin!==doc.location.origin) continue;
                            actions.push([item.label,()=>{const a=doc.createElement('a'); a.href=url;a.download='';a.click();}]);
                        } else if (item.browser_on_web) {
                            actions.push([item.label,()=>window.open(url,'_blank','noopener,noreferrer')]);
                        } else {
                            actions.push([item.label,()=>dayHost.dom.emit(id,-13,JSON.stringify({id:item.id,url,document:documentUrl}))]);
                        }
                    }
                    if (foreground && allowed(link)) for (const [title,bg] of [[foreground,false],[background,true]]) {
                        actions.push([title,()=>dayHost.dom.emit(id,-4,JSON.stringify({url:link,background:bg}))]);
                    }
                    if (!actions.length) return;
                    e.preventDefault();e.stopImmediatePropagation();
                    close(false);previous=doc.activeElement;
                    panel=document.createElement('div');
                    panel.setAttribute('role','menu');
                    Object.assign(panel.style,{position:'fixed',zIndex:2147483647,background:'Canvas',color:'CanvasText',border:'1px solid GrayText',borderRadius:'6px',padding:'4px',boxShadow:'0 2px 10px #0004'});
                    for (const [label,run] of actions) {
                        const row=document.createElement('button');
                        row.type='button'; row.textContent=label; row.setAttribute('role','menuitem');
                        Object.assign(row.style,{display:'block',textAlign:'start',width:'100%',color:'inherit',background:'transparent',font:'menu',border:0,padding:'5px 12px'});
                        row.addEventListener('click',pressed=>{
                            if (!pressed.isTrusted) return;
                            close(true);
                            if (frame.contentDocument===doc && doc.location.href===documentUrl) run();
                        });
                        panel.append(row);
                    }
                    const origin=frame.getBoundingClientRect(), fallback=el.getBoundingClientRect();
                    document.body.append(panel);
                    const x=origin.left+(e.clientX || fallback.left),y=origin.top+(e.clientY || fallback.bottom);
                    panel.style.left=Math.max(0,Math.min(x,innerWidth-panel.offsetWidth))+'px';
                    panel.style.top=Math.max(0,Math.min(y,innerHeight-panel.offsetHeight))+'px';
                    panel.addEventListener('keydown',key=>{
                        if (key.key==='Escape' || key.key==='Tab') {key.preventDefault();close(true);return;}
                        const rows=Array.from(panel.children),index=rows.indexOf(document.activeElement);
                        if (key.key==='ArrowDown'||key.key==='ArrowUp') {key.preventDefault();rows[(index+(key.key==='ArrowDown'?1:rows.length-1))%rows.length].focus();}
                    });
                    document.addEventListener('pointerdown',outside,true);
                    doc.addEventListener('pointerdown',outside,true);
                    panel.firstChild.focus();
                }
                doc.addEventListener('contextmenu',show,true);
                cleanup=()=>{close(false);doc.removeEventListener('contextmenu',show,true);};
            }
            frame.addEventListener('load',loaded);
            dayHost.dom.onRelease(id,()=>{cleanup();frame.removeEventListener('load',loaded);});
        }
        function attach_tabs(id, foreground, background) {
            const frame=dayHost.dom.element(id);
            const open=(url,background)=>dayHost.dom.emit(id,-4,JSON.stringify({url,background}));
            let current, cleanup=()=>{};
            function loaded() {
                cleanup();
                try { current=frame.contentDocument; if(!current)return; } catch {return;}
                let menu=null;
                const dismiss=()=>{menu?.remove();menu=null;};
                const link=e=>e.composedPath().find(n=>n?.tagName==='A'&&n.href);
                const click=e=>{const a=link(e);if(!a||!(e.metaKey||e.ctrlKey||e.button===1||a.target==='_blank'))return;e.preventDefault();e.stopImmediatePropagation();open(a.href,!e.shiftKey&&(e.metaKey||e.ctrlKey||e.button===1));};
                const context=e=>{
                    const a=link(e);if(!a)return;e.preventDefault();e.stopImmediatePropagation();dismiss();
                    menu=current.createElement('div');menu.setAttribute('role','menu');
                    Object.assign(menu.style,{position:'fixed',left:e.clientX+'px',top:e.clientY+'px',zIndex:2147483647,background:'Canvas',color:'CanvasText',border:'1px solid GrayText',padding:'4px',boxShadow:'0 2px 8px #0005'});
                    for(const [label,backgroundTab] of [[foreground,false],[background,true]]) {
                        const item=current.createElement('button');item.textContent=label;item.setAttribute('role','menuitem');item.style.display='block';item.onclick=()=>{open(a.href,backgroundTab);dismiss();};menu.append(item);
                    }
                    current.body.append(menu);menu.firstChild.focus();
                };
                const key=e=>{if(e.key==='Escape')dismiss();};
                current.addEventListener('click',click,true);current.addEventListener('auxclick',click,true);current.addEventListener('contextmenu',context,true);current.addEventListener('keydown',key,true);
                cleanup=()=>{dismiss();current.removeEventListener('click',click,true);current.removeEventListener('auxclick',click,true);current.removeEventListener('contextmenu',context,true);current.removeEventListener('keydown',key,true);};
            }
            frame.addEventListener('load',loaded);
            dayHost.dom.onRelease(id,()=>{cleanup();frame.removeEventListener('load',loaded);});
        }
        function attach_browser(id, base) {
            const frame = dayHost.dom.element(id);
            let current = null;
            function clicked(event) {
                const anchor = event.target?.closest?.('a[href]');
                if (!anchor) return;
                let url;
                try { url = new URL(anchor.getAttribute('href'), current.baseURI); }
                catch { return; }
                const site = new URL(dayHost.resources.siteUrl(base), document.baseURI);
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
    fn observe_browser_load(id: i32) { let _ = id; }
    #[day_bridge::impl(rust, platforms = [other])]
    fn attach_menu(id:i32,model:&str,foreground:&str,background:&str) {let _=(id,model,foreground,background);}
    #[day_bridge::impl(rust, platforms = [other])]
    fn attach_tabs(id:i32,foreground:&str,background:&str) {let _=(id,foreground,background);}
    #[day_bridge::impl(rust, platforms = [other])]
    fn attach_browser(id: i32, base: &str) { let _ = (id, base); }
    #[day_bridge::impl(rust, platforms = [other])]
    fn eval_browser(id: i32, req: f64, script: &str) { let _ = (id, req, script); }
    #[day_bridge::impl(rust, platforms = [other])]
    fn zoom_browser(id: i32, zoom: f64) { let _ = (id, zoom); }
    #[day_bridge::impl(rust, platforms = [other])]
    fn scripts_browser(id: i32, enabled: bool) { let _ = (id, enabled); }
}

pub(crate) fn observe_load(id: i32) {
    observe_browser_load(id);
}

pub(crate) fn menu(id: i32, items: &[crate::WebMenuItem], tabs: Option<&crate::TabLinkLabels>) {
    let (fg, bg) = tabs.map_or(("", ""), |t| (t.foreground.as_str(), t.background.as_str()));
    attach_menu(
        id,
        &serde_json::to_string(items).unwrap_or_default(),
        fg,
        bg,
    );
}
pub(crate) fn tabs(id: i32, labels: &crate::TabLinkLabels) {
    attach_tabs(id, &labels.foreground, &labels.background);
}
pub(crate) fn attach(id: i32, base: &str) {
    attach_browser(id, base);
}

pub(crate) fn eval(id: i32, req: f64, script: &str) {
    eval_browser(id, req, script);
}

/// Zoom the frame's document (same-origin only) now and after each load.
pub(crate) fn zoom(id: i32, zoom: f64) {
    zoom_browser(id, zoom);
}

/// Sandbox the frame without `allow-scripts`, or lift the sandbox, from its next load on.
pub(crate) fn scripts(id: i32, enabled: bool) {
    scripts_browser(id, enabled);
}

pub(crate) fn namespace() -> String {
    resource_namespace()
        .expect("resource namespace")
        .to_string()
}
/// Serve a provider through the resource worker and open `start`. `site` also polices the
/// frame's links against `base` (a provider's own site); without it the frame browses freely.
pub(crate) fn resources(id: i32, base: &str, start: &str, site: bool) {
    if site {
        attach_browser(id, base);
    }
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
        u64::from(provider),
        value["url"].as_str().unwrap_or(""),
        value["method"].as_str().unwrap_or(""),
        value["range"].as_str().unwrap_or(""),
    );
    let mut headers = r.headers.clone();
    headers.push(("Content-Type".into(), r.mime.clone()));
    resource_reply(
        token,
        i32::from(r.status),
        &serde_json::to_string(&headers).unwrap(),
        &r.body,
    );
}
