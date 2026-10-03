<!-- Copyright © The Daybrite Project; SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Browser profiles and privacy

A `WebProfile` is the storage identity of a browser, independent of a retained `WebSession`.
Configure it before realizing a view. Multiple views with the same profile share cookies,
local storage, IndexedDB and other engine-managed website data. Persistent profiles survive
application restarts. Private profiles are separate, memory-only stores; switching modes
requires rebuilding the view. Existing persistent data is preserved when entering private mode.

```rust
use day_piece_webview::{JsHandle, WebProfile, web_view};
let profile = WebProfile::persistent("publication-account");
let browser = web_view(site_url).profile(profile.clone());
let reader = web_view(article_url).profile(profile).js(reader_js);
```

`.directory(path)` requests a storage root; the stable profile identity includes that root.
Qt, WebKitGTK and WebView2 use an app-owned subdirectory for each identity. Apple and Android
use system-managed profile locations: the requested directory changes identity but cannot
move the engine's physical storage. Do not put secrets in profile names.

`JsHandle::cookie_header(url).await` reads the engine's native cookie jar, including HttpOnly
cookies eligible for that URL. It enables a native HTTP reader to use a login completed in a
browser. Treat the result as a credential: do not log it, and retrieve it again for **each**
redirect destination rather than forwarding a previous Cookie header across origins.
`set_cookie(url, response_header).await` imports an HTTP Set-Cookie response into the same jar.

`JsHandle::clear_data().await` clears the **entire profile**, including cookies, cache and
persistent website storage. Assign a dedicated profile to a site if a Forget Site button must
not erase other accounts. Await completion before sending new requests. Qt resets all pages in
the affected profile before releasing its engine and removing its owned storage directories.
An error is reported if deletion fails; it is never silently reported as success.

| Engine | Named persistent profiles | Private mode | Complete profile clearing |
|---|---|---|---|
| WKWebView (AppKit, macOS GTK, UIKit) | macOS 14 / iOS 17+, system-managed identifiers | Shared nonpersistent store | All website-data types |
| Qt WebEngine | Shared disk profiles; custom roots | Off-the-record profile | Reset pages, release profile, delete data/cache |
| WebKitGTK 6 | Shared NetworkSession; custom roots | Ephemeral NetworkSession | WebsiteDataManager, all types |
| WebView2 (WinUI, XAML, Windows GTK/Qt) | Named profiles and user-data root | InPrivate controller | ClearBrowsingDataAll |
| Android WebView | AndroidX MULTI_PROFILE, system-managed | Unsupported by stock WebView | AndroidX DELETE_BROWSING_DATA required |
| ArkWeb | App-wide, origin-isolated store; named profiles emulated | Native incognito Web component | Unsupported: no safe complete per-profile API |
| Browser iframe | Host browser storage; no profile control | Unsupported | Unsupported |

Check `profile_support()`, `private_browsing_support()` and `clear_data_support()` when exposing
controls. Native means the backend implements the API; Android/WebView2 additionally require
an installed engine version supporting the requested operation. Apple and Android report named-profile
support at runtime; Android also probes complete deletion support. On older Apple systems explicit named profiles use isolated memory rather
than falling back to the global persistent jar. ArkWeb's default and private stores share by
origin but cannot provide independent named account profiles. Clearing is explicitly rejected
there rather than deleting unrelated sites. Windows Wry's incognito context is engine-managed;
it does not guarantee independent named private accounts.

Private requests on unsupported engines are blocked before resource registration/navigation;
JavaScript/data operations fail instead of falling back to persistent storage. Private browsing
controls browser storage only, not application-managed downloads or the app's own databases.
The demo's Browser Settings page rebuilds its browser when Incognito Mode changes and exposes
the engine's capabilities and profile clearing for manual experiments.
