<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Resource providers

A `ResourceProvider` serves a tree of application-owned bytes to a web view. Open a document
with `web_view_resources(provider, path)`. The browser resolves relative images, stylesheets,
CSS imports, fonts, scripts and child documents itself. There is no loopback server, archive
extraction, base64 bridge or blob-URL rewriting.

Use this for EPUB readers, archive viewers, generated reports and other documents whose
resources are available by path but are not a deployed website. Continue using
`web_view_inline(res::assets::site)` for a wholly bundled site.

## API

```rust
use day_piece_webview::{ResourceProvider, ResourceResponse, web_view_resources};

// `archive` is application state implementing a path → (MIME, bytes) lookup.
// Its captured state must be Send + Sync. It should read only the requested entry.
let provider = ResourceProvider::new(move |request| {
    match archive.read(&request.path) {
        Ok((mime, bytes)) => ResourceResponse::new(mime, bytes),
        Err(_) => ResourceResponse::not_found(),
    }
});
let view = web_view_resources(provider, "chapters/first.xhtml");
```

A document at `chapters/first.xhtml` can use `../images/cover.jpg`, `../styles/book.css`,
`second.xhtml#heading`, and CSS `url('../fonts/body.woff2')` unchanged. Publication paths and
text are data supplied by the publication, not application resource names or translations.

`ResourceRequest` contains a decoded, root-relative `path`, an optional raw `query`, and
`method` (`GET` or `HEAD`). Fragments stay in the browser. Traversal components, backslashes,
NULs, invalid UTF-8 and requests for another provider's namespace are rejected before the
callback. An archive provider should additionally restrict access to its manifest; do not
join arbitrary request paths to an unrestricted filesystem root.

`ResourceResponse` contains `status`, `mime`, `body: Arc<[u8]>` and `headers`. `new` accepts
shared bytes or a `Vec<u8>`; cloned responses share their body. Return the complete requested
resource for both GET and HEAD. The adapter supplies lengths, removes the HEAD body, and
handles a single byte range, including suffix ranges. Unsatisfiable/multiple ranges return
416. Statuses 2xx and 4xx–5xx are accepted; redirects and informational statuses are not.
Qt cannot expose the same HTTP status semantics (see below).

Responses are `no-store`, preventing a previous provider from surviving in an engine cache.
Retain a bounded application cache if decompression is expensive. The API buffers one resource
at a time; it is not an unbounded streaming or video API. Keep individual resources bounded.

## Bundled shell and dynamic content

`with_site` mounts a generated asset directory at `__day_assets/`. All other paths go to the
callback. The shell and dynamic documents share the same origin, so the shell can manage a
child document without modifying its relative references.

```rust
let provider = ResourceProvider::with_site(res::assets::reader, move |request| {
    // Serve publication resources under book/, for example book/OPS/chapter.xhtml.
    publication_response(request)
});
let view = web_view_resources(provider, "__day_assets/index.html")
    .js(reader_js)
    .on_external_link(handle_reader_link);
```

In this example the shell opens `../book/OPS/chapter.xhtml` in an iframe. References inside
that chapter resolve under `book/OPS/`. Use the generated `res::assets::reader` constant;
links *inside* the directory remain document-relative. Format app-owned UI text using
localized generated accessors on the UI thread before capturing it in a provider.

`provider.url(path)` encodes a root-relative path; `base_url()` returns its base. Treat these
URLs as opaque and temporary. Native custom schemes and synthetic HTTPS/browser paths differ
by engine. Do not persist them, concatenate a hardcoded scheme, or register an OS URL handler.
`__day_assets/` and the `X-Day-Bundled-Asset` response header are reserved for the bundled mount.

## Execution and lifetime

The callback is synchronous. Native adapters invoke it on I/O workers (a fixed four-worker
pool where the toolkit does not supply its own worker). Web DOM invokes it on the app's WASM
thread; a slow callback will block that tab. Do not read Day signals, create toolkit objects,
format localized messages or panic inside the callback. Capture immutable data, shared bytes
or a thread-safe archive handle instead.

A view retains its provider. Shared provider handles can keep the namespace alive longer;
a request after its last handle is dropped receives 410. Native adapters retain request
objects while loading, and stopped/destroyed views do not receive a late callback. A callback
already reading bytes is not forcibly interrupted: finishing that synchronous read may still
consume work after the browser abandons a request. This is not a cancellable network client.

A `WebSession` retains its first provider and starting URL along with its engine, including
while detached. Like existing sessions, this lasts for the process lifetime. Reuse a stable
session only for a persistent document surface, not one session per book. Ordinary views
release their namespace when removed.

Top-level navigation outside the provider follows the existing external-link policy.
Subresource loading is not a sandbox: providers serve bytes; they do not sanitize HTML or
prevent remote requests. For untrusted documents, remove active content and supply a CSP.
Stanza also embeds the CSP in the document for Qt versions without response-header support.
Providers must not expose secrets merely because the URL contains a generated identifier.

## Backends

| Target | Resource channel | Particulars |
|---|---|---|
| macOS AppKit, macOS GTK, iOS UIKit | `WKURLSchemeHandler` | Installed in the configuration before creating the web view. Stopped tasks cannot receive responses. |
| Android MDC | `WebViewClient.shouldInterceptRequest` | Reserved synthetic HTTPS origin; binary `WebResourceResponse` on the WebView I/O thread. No localhost listener or cleartext permission. |
| Linux GTK | `WebContext.register_uri_scheme` | WebKitGTK response streams, status and headers; completion on the GTK main context. |
| macOS/Linux Qt | `QWebEngineUrlSchemeHandler` | Scheme registered before Qt startup; isolated profile per resource view. Qt 6.6+ supports custom response headers. Qt exposes reply/error rather than arbitrary HTTP statuses; do not depend on fetch status or HTTP range/media semantics here. |
| Windows XAML | WebView2 `WebResourceRequested` | Reserved HTTPS origin, document/iframe filter, deferral while workers read, response completion on the UI dispatcher. Requires the WebView2 runtime and its iframe-aware request-filter API. |
| Windows GTK/Qt | Wry's asynchronous WebView2 protocol handler | Uses Wry's synthetic HTTP origin mapping; same provider callback. Windows Qt builds with WebEngine use the Qt handler above. |
| Harmony ArkUI | ArkWeb NDK scheme handlers, API 12+ | Registers the reserved HTTPS handler by web tag after controller attachment and before document navigation. Response data bypasses ArkTS. |
| Web DOM | Scoped service worker and transferable byte buffers | HTTPS or localhost required. The worker controls only `assets/data/day-piece-webview/`, leaving the app/PWA scope alone. The owning tab must remain alive. Responses preserve Day's cross-origin isolation headers. |

Web deployment must include the piece's `web/` assets; `day build` stages these automatically.
Serving the app from a subdirectory works without a root-scope service worker. Missing owners
time out with 410; content is not stored in the service-worker cache. Generated page URLs are
session-local, not bookmarks. Private-browsing/browser policies that prohibit service workers
also prohibit resource views; regular bundled views do not need a service worker.

Native API references: [Apple scheme handler](https://developer.apple.com/documentation/webkit/wkurlschemehandler),
[Android request interception](https://developer.android.com/reference/android/webkit/WebViewClient#shouldInterceptRequest(android.webkit.WebView,%20android.webkit.WebResourceRequest)),
[WebKitGTK scheme registration](https://webkitgtk.org/reference/webkit2gtk/stable/method.WebContext.register_uri_scheme.html),
[Qt request jobs](https://doc.qt.io/qt-6/qwebengineurlrequestjob.html),
[WebView2 local content](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/working-with-local-content).

## Tests

`demo/dayscript/webview.yaml` opens generated HTML and verifies a relative script, CSS import,
image, nested document and stylesheet from a generated bundled asset directory. It reloads
the resource view, then returns to the regular bundled view. The shared CI matrix runs the
same assertions on every primary platform, including Windows and HarmonyOS.

`cargo test` covers path validation, provider isolation/lifetime, shared bodies, byte ranges,
HEAD, response validation and reentrant/concurrent providers. `node --test tests/*.mjs`
exercises the shipped browser bridge, service worker and Harmony controller lifecycle.
Native engine execution still matters: a Rust cross-check does not establish working rendering.
