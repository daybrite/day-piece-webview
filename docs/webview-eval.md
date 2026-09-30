---
title: "JavaScript and app integration"
description: "Read page state, send structured data, and handle web view readiness and errors."
---
<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# JavaScript and app integration

Attach a `JsHandle` to one web view with `.js(handle)`, then call `handle.eval(script).await`
from a Day task. The result is **JSON text**, not a Rust value or a raw JavaScript string.
Evaluation works with all three content constructors. web-dom requires a same-origin frame.

## Read structured page state

```rust
use day_piece_webview::JsHandle;

async fn reading_position(js: JsHandle) -> Result<serde_json::Value, String> {
    let json = js.eval(
        "({ chapter: location.pathname, scroll: window.scrollY, height: document.body.scrollHeight })"
    ).await.map_err(|error| error.to_string())?;
    serde_json::from_str(&json).map_err(|error| error.to_string())
}
```

Add `serde_json = "1"` to the application's dependencies for these JSON examples. A string
result such as `document.title` includes JSON quotes; deserialize it as `String`. Arrays,
objects, booleans, and numbers retain their JSON representation. `undefined` becomes `null`.
Thrown exceptions and values that cannot be serialized, such as cyclic objects, become errors.

An expression returning a Promise is not awaited. For asynchronous work, start it in the page,
store the result in page state, then read that state after readiness is established. Keep expensive
page computation out of synchronous evaluation.

## Send localized labels and application data

Serialize values instead of interpolating strings into JavaScript source:

```rust
use day_piece_webview::{EvalError, JsHandle};

async fn update_chart(js: JsHandle, samples: Vec<f64>) -> Result<(), EvalError> {
    // Localize on the UI thread. The app defines this generated translation key.
    let payload = serde_json::json!({
        "title": res::str::chart_title().format(),
        "samples": samples,
    });
    js.eval(format!("window.chart.update({payload})")).await?;
    Ok(())
}
```

In the bundled page, define the corresponding application API:

```javascript
window.chart = {
  update({ title, samples }) {
    document.querySelector("#chart-title").textContent = title;
    renderSeries(samples); // The application's chart renderer.
  }
};
```

Use `textContent` for text. Do not build `innerHTML` from user data. The page does not need
English fallback labels: the application supplies all UI translations through generated
accessors. JSON serialization protects the script boundary; it does not make later unsafe DOM
operations safe. Only evaluate code that the application controls.

For documents and binary payloads, use a [resource provider](resource-provider.md). A short
JavaScript command can select a chapter URL while the browser loads its body and images on demand.

## Wait for the page, not just the widget

Creating a view does not mean its browser process or document is ready. There is no public
load-complete callback yet. A bundled page can expose a readiness flag after initialization:

```javascript
initializeReader().then(() => { window.readerReady = true; });
```

An app can check it before enabling a command:

```rust
use day_piece_webview::{EvalError, JsHandle};

async fn reader_is_ready(js: JsHandle) -> Result<bool, EvalError> {
    Ok(js.eval("window.readerReady === true").await? == "true")
}
```

Retry transient startup failures through a bounded application task/timer; do not spin in a tight
loop. Stop retries when the view is removed, and reset readiness on document navigation. The
iframe load event alone is insufficient to prove a remote page rendered successfully.

For page-to-app actions, use a recognized anchor URL and
[`.on_external_link()`](webview.md#links-from-web-content-into-the-app). On web-dom the hook
observes anchor clicks, not arbitrary `location` assignments. It is not a general-purpose
message channel for untrusted web pages.

## Task and error handling

This example assumes the app defines `res::str::page_unavailable()` and displays the supplied
status signal. Technical details go to diagnostics rather than directly into an untranslated
error label:

```rust
use day::prelude::*;
use day_piece_webview::{EvalError, JsHandle};

fn next_page(js: JsHandle, status: Signal<String>) {
    day::task(async move {
        match js.eval("window.reader.nextPage()").await {
            Ok(_) => status.set(String::new()),
            Err(EvalError::ViewGone) => {}, // Navigation may have removed the view.
            Err(error) => {
                eprintln!("reader command failed: {error}");
                status.set(res::str::page_unavailable().format());
            }
        }
    });
}
```

| Error | Meaning |
|---|---|
| `Unsupported` | No evaluation implementation compiled for the target |
| `ViewGone` | The handle has no realized view, or its view was disposed |
| `Threw { name, message }` | JavaScript threw, including serialization errors |
| `Engine(String)` | Engine, access, or reply-decoding failure; includes cross-origin browser restrictions |

A handle can be reused when replacing a view, but bind it to only one live view at a time.
Each evaluation gets a request ID. Nothing runs until the future is polled. Dropping the future
removes the pending reply registration; it **does not cancel JavaScript already dispatched**.
There is no built-in evaluation timeout. Applications needing one should bound the future's
lifetime and disregard stale results after navigation.

`eval_support()` reports a compiled implementation, not document readiness or permission to
inspect a particular origin. No backend can make cross-origin browser iframe evaluation work
by changing this flag.

## Test the actual page

Dayscript's `web_eval` uses the same evaluation implementation. It retries the assertion until
the configured timeout:

```yaml
- web_eval:
    id: reader
    script: "window.readerReady === true"
    text: "true"
    timeout_secs: 30
- web_eval:
    id: reader
    script: "document.querySelector('img').naturalWidth > 0"
    text: "true"
```

Use `text` for an exact result or `contains` for a substring. An omitted comparison checks only
that evaluation succeeds. A successful script does not prove visible rendering; assert geometry
and inspect screenshots too. [Testing and CI](testing.md) lists the existing assertions.

## Backend transport

Apple uses `evaluateJavaScript`, WebKitGTK uses `evaluate_javascript`, Qt uses `runJavaScript`,
Android uses `evaluateJavascript`, Windows uses WebView2, Harmony uses ArkWeb `runJavaScript`,
and web-dom evaluates through the same-origin iframe window. See [Platforms](platforms.md).

Internally, the piece wraps code in a JSON/error envelope and removes each engine's outer
string encoding before resolving the request. Applications should use `JsHandle` and `EvalError`,
not parse that internal transport envelope. Native UTF-16 conversion, Unicode, and error replies
have host tests; the demo checks actual engine replies across the primary platform matrix.
