<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Platforms and implementation

The Rust piece selects a content source and sends navigation/evaluation commands to a backend.
The backend creates a platform browser view, integrates it into Day layout, and reports URL,
link-policy, and evaluation events. `day build` selects features and stages the piece's native
sources, framework dependencies, and web assets.

## Renderers and prerequisites

| Target | Renderer | Requirements and hosting |
|---|---|---|
| macos-appkit | WKWebView | System WebKit framework; native NSView |
| ios-uikit | WKWebView | System WebKit framework; native UIView |
| macos-gtk | WKWebView | Cocoa child positioned from a GTK allocation anchor |
| linux-gtk | WebKitGTK 6 | GTK 4, libadwaita, WebKitGTK 6 development/runtime libraries |
| macos-qt, linux-qt | QWebEngineView | Qt 6 WebEngineWidgets at build time; otherwise a URL-label fallback |
| windows-qt | QWebEngineView or WebView2 | WebEngine if detected; otherwise a Wry WebView2 child in a QWidget |
| windows-gtk | WebView2 | Wry child window positioned from the GTK allocation |
| windows-xaml | WebView2 | Composition controller attached to a XAML Border; WebView2 Runtime required |
| android-mdc | Android WebView | System WebView; piece contributes the INTERNET permission |
| harmony-arkui | ArkWeb | ArkTS Web component; compatible installed ArkWeb runtime; resource handler requires API 12+ |
| web-dom | iframe | Host browser; no separate engine installation |

Windows XAML uses WebView2, **not** the older EdgeHTML XAML WebView. The composition controller
places its visual in the XAML tree and forwards pointer events. It is distinct from the child
HWND used by Wry on Windows GTK/Qt. The WebView2 SDK used to compile the shim does not replace
the installed WebView2 Runtime.

The XAML browser is closed by Day's piece-release hook. Temporary removal from the visual tree
(XAML `Unloaded`, including reparenting during cover layout) does not destroy it. Asynchronous
startup callbacks ignore a released view. On Windows Qt, Wry initialization is queued after
`showEvent` returns because Wry pumps Win32 messages while creating WebView2; starting it during
Day's tree construction can re-enter an unfinished layout. Closing the widget cancels queued
startup, and a close delivered during initialization releases the newly created browser.

Native child hosts, particularly macOS GTK and Windows GTK/Qt, have clipping/overlap constraints.
Test nested scrolling, popovers, keyboard focus, resizing, and visibility in the actual host.
On macOS GTK, a widget-only GTK screenshot omits the Cocoa child; capture the native window.

`support()`, `inline_support()`, and `eval_support()` inspect compile-time configuration. They
are not runtime engine probes. In particular, the Qt feature can report support even when the
build lacks WebEngine and uses the fallback. Runtime installation, startup, permissions, and
remote embedding failures still need error handling.

## Bundled sites

`web_view_inline` opens a generated asset directory using each engine's local-content channel.

| Engine | Bundled-content channel |
|---|---|
| WKWebView | File URL with an explicit read-access root; macOS GTK extracts its bundled tree first |
| WebKitGTK | GResource tree extracted to an application cache, then a file URL |
| Qt WebEngine | `qrc:/day/assets/…` |
| Android WebView | `file:///android_asset/…` |
| WebView2 on XAML | Virtual host `day-assets.example` mapped to a local asset directory |
| Wry WebView2 hosts | Local asset files, including extraction where needed |
| ArkWeb | `resource://rawfile/day/…` |
| web-dom | Deployed same-origin `assets/data/…` paths |

These are implementation details, not URLs applications should construct. Use generated
`res::assets` constants. File-origin fetch behavior varies; ordinary relative images/scripts
are more portable than fetching arbitrary sibling files from a file URL.

## Resource loading

`web_view_resources` installs a scoped request handler before loading the document. The browser
resolves relative URLs; the handler passes individual requests to the Rust provider and delivers
binary responses. No loopback listener is used.

| Target | Request interception | Important differences |
|---|---|---|
| AppKit, UIKit, macOS GTK | WKURLSchemeHandler, `day-resource://p…/` | Configured before WKWebView creation; stopped tasks suppress late delivery |
| Linux GTK | WebContext URI scheme handler, `day-resource://p…/` | Worker reads; response completion on the GTK main context; see the current [CI failure](testing.md#observed-results) |
| Qt WebEngine | QWebEngineUrlSchemeHandler, `day-resource://p…/` | Scheme registered before Qt startup; resource views get their own profile |
| Android | `shouldInterceptRequest`, reserved `https://day-resource.invalid/…` | WebResourceResponse on WebView's I/O thread |
| Windows XAML | WebView2 WebResourceRequested at reserved HTTPS origin | Iframe-aware filter, deferrals for worker reads, desktop DispatcherQueue completion |
| Windows Wry hosts | Asynchronous custom protocol, synthetic `http://day-resource.p…/` | WebView2 mapping; not a network HTTP endpoint |
| Harmony | ArkWeb NDK scheme handler at reserved HTTPS origin | Registered per web tag after controller attachment, before navigation; binary data bypasses ArkTS |
| web-dom | Scoped service worker and message channels | HTTPS/localhost, owning tab alive, synchronous Rust callback on app thread |

Qt replies through a MIME type and device, or a request error. It cannot reproduce arbitrary
HTTP status codes. Additional response headers need Qt 6.6+. Do not rely on uniform `fetch`
status codes, range/media behavior, or header-based policy enforcement on older Qt.

Other adapters carry the common response status/header contract, but host unit tests do not
prove every engine treats ranges and error responses identically. The integration suite currently
emphasizes successful document/subresource loading. See [test coverage](testing.md).

On Windows, resource completions return through `Windows.System.DispatcherQueue` for
XAML Islands and `Microsoft.UI.Dispatching.DispatcherQueue` for WinUI. `CoreDispatcher`
is a UWP/CoreWindow mechanism; Microsoft recommends [DispatcherQueue for XAML Islands](https://learn.microsoft.com/en-us/dotnet/communitytoolkit/windows/extensions/dispatcherqueueextensions). WebView2
response construction and deferral completion run on the creating UI thread.

On Harmony, bundled provider assets are read by native workers. Day's rawfile manager
must be process-wide, with reads protected against manager replacement; a thread-local
manager makes these requests return 404 even though ordinary UI-thread resource reads work.

The browser worker controls only `assets/data/day-piece-webview/`, including when the app is
served under a subdirectory. It forwards requests to the owning tab and transfers response bytes.
Bundled mounts are fetched from their deployed asset paths. It does not persist document bodies.
If service workers are disabled, resource views cannot load; ordinary inline views do not use
this worker. See [provider deployment](resource-provider.md#browser-deployment).

## Navigation, JavaScript, and sessions

| Feature | Native engines | web-dom |
|---|---|---|
| Load / reload | Implemented | Implemented; reload uses last app-supplied URL |
| Back / forward / stop | Implemented | No-ops |
| URL signal readback | Native navigation reports | Not implemented |
| JavaScript evaluation | Engine evaluation API | Same-origin iframe only |
| Inline/resource link policy | Native navigation callbacks | Top-level document anchor-click hook |
| Retained WebSession | WKWebView and Qt WebEngine only | Not implemented |

A remote website may refuse iframe embedding through CSP or `X-Frame-Options`. The host cannot
inspect a cross-origin document, and an iframe load event does not prove visible success.
The browser frame has no `sandbox` attribute by default; the piece does not isolate untrusted
HTML from the app origin. Link callbacks do not intercept every possible navigation or block
subresource requests.

`WebSession` retains an engine on AppKit, UIKit, macOS GTK, and Qt WebEngine. Other hosts rebuild
the engine. Resource/session bookkeeping still pins the first provider, so avoid sessions for
transient documents even on hosts that ignore engine retention.

## Packaging and diagnostics

- **Apple:** this crate declares WebKit.framework through piece metadata; `day build` links it.
- **Qt Flatpak:** piece metadata declares the Qt WebEngine BaseApp, included when the executable
  links WebEngine. Its version follows Day's selected Qt runtime.
- **GTK AppImage:** the launcher defaults `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` because
  bundled WebKit helpers can lack the AppArmor profile needed by bubblewrap on Ubuntu. This
  reduces isolation and applies to AppImage packaging, not the Flatpak/distro build. Treat
  imported web content accordingly.
- **Windows:** install the WebView2 Runtime. XAML startup errors include an operation and HRESULT;
  inspect stderr and evaluation errors. CI installs the runtime explicitly.
- **Harmony:** the shared workflow provisions a matching x86_64 engine for its emulator.
  See [Harmony testing](harmony-emulator.md); a successful HAP build alone does not prove rendering.
- **Browser:** deploy the piece's staged assets, use HTTPS or localhost, and inspect service-worker
  registration and iframe requests when resource views fail.

## Source map

| Area | Files |
|---|---|
| Public constructors, commands, evaluation lifecycle | [src/lib.rs](../src/lib.rs) |
| Provider registry, validation, ranges, worker pool | [src/resources.rs](../src/resources.rs) |
| macOS WKWebView host | [src/macos_web.rs](../src/macos_web.rs) |
| UIKit host | [src/lib-uikit.rs](../src/lib-uikit.rs) |
| WebKitGTK | [src/lib-gtk.rs](../src/lib-gtk.rs) |
| Qt | [src/lib-qt-shim.cpp](../src/lib-qt-shim.cpp) |
| Windows XAML | [src/lib-xaml-shim.cpp](../src/lib-xaml-shim.cpp) |
| Android | [src/DayWebView.java](../src/DayWebView.java) |
| Harmony | [platform/harmony/ets](../platform/harmony/ets), [src/lib-arkui.rs](../src/lib-arkui.rs) |
| Browser bridge and worker | [src/browser.rs](../src/browser.rs), [web/resource-worker.js](../web/resource-worker.js) |

The piece owns these adapters. Core Day supplies generic piece registration, native build
contributions, browser bridging, layout, and the named-operation interface used by dayscript.
