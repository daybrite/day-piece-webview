<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Resource providers

A `ResourceProvider` serves application-owned bytes by path. Open a document with
`web_view_resources(provider, path)`; the browser requests its images, stylesheets, scripts,
fonts, and child documents as needed. Relative links keep their ordinary meaning.

Use this for EPUBs, archive viewers, generated reports, or a bundled editor with runtime data.
Use `web_view_inline(res::assets::site)` when everything is already bundled. Providers avoid
whole-archive extraction, base64 transport, and rewriting every resource reference. They do
not implement an HTTP server or an asynchronous network client.

## A generated report

This helper accepts already-rendered HTML, including any localized headings, and shares the
response bytes across repeated requests:

```rust
use day_piece_webview::{ResourceProvider, ResourceResponse, WebView, web_view_resources};

fn report_view(html: String) -> WebView {
    let document = ResourceResponse::new("text/html", html.into_bytes());
    let provider = ResourceProvider::new(move |request| match request.path.as_str() {
        "report.html" => document.clone(),
        _ => ResourceResponse::not_found(),
    });
    web_view_resources(provider, "report.html")
}
```

Generate app-owned text from `res::str` accessors on the UI thread before calling the helper.
Use an HTML template engine that escapes text and attribute values. Publication text and
user-entered content are data; neither should be interpreted as markup without validation.

For a report with existing binary attachments, use a path-to-response map:

```rust
use std::collections::HashMap;
use day_piece_webview::{ResourceProvider, ResourceResponse};

fn report_resources(entries: HashMap<String, ResourceResponse>) -> ResourceProvider {
    ResourceProvider::new(move |request| {
        entries.get(&request.path).cloned().unwrap_or_else(ResourceResponse::not_found)
    })
}
```

For example, a runtime-generated `reports/month.html` entry can reference
`../images/chart.png`. Return `image/png` for the image and `text/css` for a stylesheet.
`ResourceResponse::clone()` shares its `Arc<[u8]>` body. This map is suitable for small reports;
use on-demand reads for large archives.

## Load an archive one entry at a time

The piece does not parse ZIP or EPUB. Keep the archive parser and manifest checks in the app.
The following adapter defines the boundary explicitly:

```rust
use std::sync::Arc;
use day_piece_webview::{ResourceProvider, ResourceResponse};

trait Publication: Send + Sync + 'static {
    // Read one manifest-approved entry. Return None for unknown/disallowed paths.
    // The result contains its media type and complete, bounded bytes.
    fn read_resource(&self, path: &str) -> Option<(String, Vec<u8>)>;
}

fn publication_provider(publication: Arc<dyn Publication>) -> ResourceProvider {
    ResourceProvider::new(move |request| {
        match publication.read_resource(&request.path) {
            Some((mime, bytes)) => ResourceResponse::new(mime, bytes),
            None => ResourceResponse::not_found(),
        }
    })
}
```

Open `OPS/chapters/first.xhtml` with `web_view_resources`. The chapter can reference
`../images/cover.jpg`, `../styles/book.css`, or `second.xhtml#heading`. CSS can contain
`url('../fonts/body.woff2')`. These paths come from the publication, not from bundled app
resource names. Return `application/xhtml+xml` for XHTML and the actual MIME type for each
resource; the adapters send `nosniff`.

A ZIP library requiring mutable access can be protected by a mutex, but a single archive lock
serializes reads. Keep decompression and resource-size limits explicit. Cache frequently used
stylesheets and fonts in a bounded cache; do not preload the entire book to satisfy this API.

## Bundled shell and dynamic content

`with_site` mounts an app's generated asset directory at `__day_assets/`. Other paths go to the
provider callback. This lets a reader shell and a publication share an origin:

```rust
use day_piece_webview::{
    JsHandle, ResourceProvider, ResourceRequest, ResourceResponse, WebView, web_view_resources,
};

fn reader_shell(
    js: JsHandle,
    serve_publication: impl Fn(ResourceRequest) -> ResourceResponse + Send + Sync + 'static,
) -> WebView {
    let provider = ResourceProvider::with_site(res::assets::reader, serve_publication);
    web_view_resources(provider, "__day_assets/index.html").js(js)
}
```

Inside `resource/assets/reader/index.html`, a document-relative frame source such as
`../book/OPS/chapter.xhtml` requests `book/OPS/chapter.xhtml` from the callback. Resolve that
prefix against the publication manifest. Within the chapter, relative resources resolve from
`book/OPS/`. The shell's own `./reader.css` remains under the bundled mount.

For chapter changes, update the frame's `src` to the next publication path. A bundled shell
can animate the transition, track reading position, and communicate through the
[JavaScript API](webview-eval.md). Supply app-owned web UI translations through generated
accessors; do not ship English fallback labels in the shell.

`__day_assets/` and the `X-Day-Bundled-Asset` response header are reserved. Use the generated
`res::assets::reader` constant to mount the bundle, rather than loading bundled resources with
literal names in the callback.

## Requests and responses

| Request field | Meaning |
|---|---|
| `path: String` | Percent-decoded path relative to the provider root; no leading slash |
| `query: Option<String>` | Raw query string, without `?`; parse it if needed |
| `method: String` | `GET` or `HEAD`; other methods are rejected before the callback |

Fragments remain in the browser. The dispatcher rejects invalid UTF-8, NULs, backslashes,
traversal components, and requests outside the provider namespace. Browser URL normalization
may resolve `..` before sending a request. The provider must still restrict reads to its
manifest/root; these checks do not authorize arbitrary filesystem access.

| Response field | Meaning |
|---|---|
| `status: u16` | Defaults to 200; 2xx and 4xx–5xx accepted; redirects/informational statuses rejected |
| `mime: String` | Resource media type, such as `text/css` or `image/jpeg` |
| `body: Arc<[u8]>` | Complete bytes for this resource; `new` also accepts `Vec<u8>` |
| `headers: Vec<(String, String)>` | Additional valid response headers, subject to backend limitations |

`ResourceResponse::not_found()` returns 404 with an empty body. `ResourceResponse::error(500)`
returns an empty error response. Supply the complete resource even for HEAD. The dispatcher
sets its length, strips the HEAD body, and applies a single byte range, including suffix ranges.
Invalid, multiple, or unsatisfiable ranges return 416. Statuses 204 and 205 have empty bodies.

Invalid header names/values are removed. Content type, length, range, cache, and `nosniff`
headers belong to the adapter. Responses use `Cache-Control: no-store` so engine caches do not
retain old provider data. Qt cannot expose arbitrary HTTP status semantics, and custom response
headers require Qt 6.6+. Do not assume identical `fetch` status or media-seeking behavior across
engines; see [Platforms](platforms.md#resource-loading).

## URLs

`provider.url(path)` percent-encodes each path segment. Literal `?`, `#`, and `%` in filenames
are encoded as filename characters. Append a separately encoded query or fragment if needed.
`base_url()` includes a trailing slash. Both are engine-specific and temporary; the web base
is relative to the deployed app. Do not persist these URLs or reconstruct their scheme.

Prefer relative URLs inside the served document. There is no OS scheme registration to perform
and no public web address for another app to open.

## Threads, memory, and lifetime

The callback is `Fn(ResourceRequest) -> ResourceResponse + Send + Sync + 'static`. Native
adapters run it on I/O workers: a fixed four-worker pool where the toolkit does not supply its
own worker. Android uses WebView's request thread. **On web-dom the callback runs synchronously
on the app's WASM thread and can block the UI.** It cannot await a network request.

Capture shared bytes or thread-safe archive state. Do not access Day signals, create widgets,
or format localized messages inside the callback. Native requests may run concurrently. Do not
panic; normal read failures should become responses.

The API buffers a complete resource, not a stream. `Arc` avoids copying bodies when responses
are cloned, but ranges and native/browser transfers may still copy bytes. A large individual
image or chapter can still consume substantial memory. Bound input sizes and cache usage.

A view retains its provider; clones can retain it longer. Requests after the last owner is gone
receive 410. Request cancellation is adapter-specific; a callback already reading/decompressing
bytes is not forcibly interrupted. Do not use cancellation as a memory or computation bound.

A `WebSession` pins its first provider and initial URL for the process lifetime. Engine retention
is supported only on [some backends](webview.md#retain-a-page-across-app-navigation); it is not
a way to swap publications under one stable key. Use ordinary views for transient documents.

## Browser deployment

`day build` stages `web/resource-worker.js`. Resource views use that service worker beneath
`assets/data/day-piece-webview/`; it does not claim the application's root/PWA scope. HTTPS or
localhost is required, service workers must be permitted, and the owning app tab must stay alive.
A deployment under a subdirectory is supported. An unknown owner returns 410; a nonresponsive
owner times out. No document bodies are stored in the service-worker cache.

The browser path uses transferable buffers and preserves Day's cross-origin isolation headers.
It still follows browser origin/CSP rules. Regular inline sites do not require this worker.
There is currently no dedicated `resource_support()` or provider-startup result API;
`inline_support()` alone cannot verify service-worker availability.

## Untrusted documents

Resource providers route bytes; they do not sanitize HTML, disable scripts, or block remote
requests. Restrict access to known resources, sanitize imported content, and apply a CSP suited
to the document. If the policy must work on older Qt, embed an appropriate CSP in the document
as well as using response headers. Do not serve secrets based only on possession of a generated
URL. External-link handling is a navigation convenience, not a security boundary.

## Verification

The [demo](../demo/src/lib.rs) and [walkthrough](../demo/dayscript/webview.yaml) exercise relative
JavaScript, CSS imports, a binary image response, a nested document, bundled CSS, and reload.
The [testing guide](testing.md) distinguishes native execution from host mocks and records the
Linux GTK failure found in the first CI run. Range, lifetime, and validation tests exist, but
not every native response/lifecycle path has end-to-end coverage yet.
