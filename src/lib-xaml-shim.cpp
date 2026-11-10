// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// The web-view piece's C++/WinRT shim, parallel to src/lib-qt-shim.cpp.
//
// day-toolkit-winui hosts UWP system XAML (DAY_XAML_NS, base Windows SDK, no WinAppSDK) inside a
// Win32 window via XAML Islands. The system-XAML web view, Windows.UI.Xaml.Controls.WebView (EdgeHTML),
// is unsupported in that host: it renders blank, never raises NavigationCompleted, and crashes on
// navigation. The supported engine is WebView2, hosted here in windowless / visual-hosting mode,
// the same technique the official XAML WebView2 controls use internally:
//
//   * make() boxes a plain XAML Border (transparent, hit-testable, with a faint URL label) as the day
//     handle. day lays it out like any leaf.
//   * A CoreWebView2CompositionController renders the page into a Windows.UI.Composition Visual instead
//     of its own HWND. We splice that visual into the XAML tree with ElementCompositionPreview::
//     SetElementChildVisual(Border, visual), so the web view is a node in the XAML visual tree:
//     correct z-order, clipping, DPI and layout, no separate window to track, no airspace.
//   * Input: a raw child HWND over the XAML island gets no mouse input, because the island's
//     ContentIsland InputSite owns pointer input for its whole surface. Windowless hosting turns
//     that around: the InputSite delivers pointer events to the Border (XAML), and we forward them
//     to the controller's SendMouseInput. So clicks/scroll/drag work, routed through XAML's own
//     input.
//   * Day's piece release closes an anonymous browser; a named WebSession retains its engine.
//     XAML Unloaded also fires during reparenting; it is not a destruction notification.
//   * If WebView2 creation fails, the Border shows the operation and HRESULT; eval replies and
//     stderr report the same error (including a hint for a missing runtime).
//
// WebView2LoaderStatic.lib is statically linked by build.rs (no DLL to bundle); the WebView2 Runtime
// must be installed separately where the OS does not supply it (including CI runners).

#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Foundation.Collections.h> // JsonObject::Insert needs the IMap definitions
#include <winrt/Windows.Data.Json.h>
#include <sstream>
#include <iomanip>
#include <ctime>
#include <cctype>
#include <algorithm>
#include <winrt/Windows.UI.h>             // Color (transparent, hit-testable Border background)
#include <winrt/Windows.UI.Composition.h> // Compositor, ContainerVisual (the render target)
#include <winrt/Windows.UI.Input.h>       // PointerPoint(Properties), PointerUpdateKind
// One shim, two XAML stacks (docs/winui.md): windows-xaml builds it against system XAML,
// windows-winui (DAY_WINUI) against WinUI 3. The namespaces differ; the controls do not.
#ifdef DAY_WINUI
#define DAY_XAML_NS winrt::Microsoft::UI::Xaml
#else
#define DAY_XAML_NS winrt::Windows::UI::Xaml
#endif
#ifdef DAY_WINUI
#include <winrt/Microsoft.UI.Xaml.h>
#else
#include <winrt/Windows.UI.Xaml.h>
#endif
#ifdef DAY_WINUI
#include <winrt/Microsoft.UI.Xaml.Controls.h>
#else
#include <winrt/Windows.UI.Xaml.Controls.h>
#endif
#ifdef DAY_WINUI
#include <winrt/Microsoft.UI.Xaml.Hosting.h> // ElementCompositionPreview (splice visual into the tree)
#else
#include <winrt/Windows.UI.Xaml.Hosting.h> // ElementCompositionPreview (splice visual into the tree)
#endif
#ifdef DAY_WINUI
#include <winrt/Microsoft.UI.Xaml.Input.h>   // PointerRoutedEventArgs
#else
#include <winrt/Windows.UI.Xaml.Input.h>   // PointerRoutedEventArgs
#endif
#ifdef DAY_WINUI
#include <winrt/Microsoft.UI.Xaml.Media.h>
#else
#include <winrt/Windows.UI.Xaml.Media.h>
#endif

#include <windows.h>
#include <wrl.h>
#include <wrl/event.h>
#include <wincodec.h> // WIC: scaling a page snapshot (JsHandle::snapshot) before re-encoding it
#include <WebView2.h>
#include <WebView2EnvironmentOptions.h>
#ifdef DAY_WINUI
// WinUI 3 has a real WebView2 control; its core object is the same ICoreWebView2 the system-XAML
// path drives, reached through ICoreWebView2Interop2.
#include <winrt/Microsoft.Web.WebView2.Core.h>
#include <winrt/Microsoft.UI.Dispatching.h> // DispatcherQueue (completions back to the UI thread)
#include <functional>
#include <WebView2Interop.h>
#endif

#include <cmath>
#include <cstdio>
#include <cstdint>
#include <map>
#include <string>

#include "xaml-strings.h"
#include "resources.h"
#include <winrt/Windows.System.h> // DispatcherQueue for desktop XAML Islands
#include <memory>
#include <vector>
#include <algorithm>
#include <cstdlib>
#include <cwchar>

using namespace winrt;
namespace WF = winrt::Windows::Foundation;
namespace WUI = winrt::Windows::UI;
namespace WUC = winrt::Windows::UI::Composition;
namespace WUInput = winrt::Windows::UI::Input;
namespace WUX = DAY_XAML_NS;
namespace WUXC = DAY_XAML_NS::Controls;
namespace WUXH = DAY_XAML_NS::Hosting;
namespace WUXI = DAY_XAML_NS::Input;
namespace WUXM = DAY_XAML_NS::Media;
namespace wrl = ::Microsoft::WRL;

// Exported by day-toolkit-xaml-sys (already linked into the app). The host HWND is the composition
// controller's parentWindow (for DPI / IME / input association); the page still renders windowless.
extern "C" void *day_xaml_box(void *iinspectable_abi);
extern "C" void *day_xaml_unbox(void *handle);
extern "C" void *day_xaml_host_hwnd();
extern "C" void day_xaml_delete(void *handle);

static winrt::hstring hs(const char *s) {
    if (!s || !*s)
        return winrt::hstring{};
    int len = MultiByteToWideChar(CP_UTF8, 0, s, -1, nullptr, 0);
    if (len <= 1)
        return winrt::hstring{};
    std::wstring w(static_cast<size_t>(len - 1), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s, -1, w.data(), len);
    return winrt::hstring{w};
}

static std::string to_utf8(winrt::hstring const &h) {
    return day_webview::utf8(std::wstring_view(h.c_str(), h.size()));
}

// Per-web-view state. Keyed by the day handle (the boxed Border) so async callbacks and later
// operations find it, and a callback that outlives teardown is a safe no-op (find returns null).
struct WebViewCtx {
    bool loading = false;
    HWND parent{}; // host window, the composition controller's parentWindow
    wrl::ComPtr<ICoreWebView2CompositionController> compositionController; // SendMouseInput, visual
    wrl::ComPtr<ICoreWebView2Controller> controller; // Bounds, IsVisible, focus, Close (same object)
    wrl::ComPtr<ICoreWebView2> webview;
    WUXC::Border placeholder{nullptr}; // XAML host element; the render visual is its child
#ifdef DAY_WINUI
    // WinUI: the WebView2 control, the placeholder's child once created. It owns the controller
    // (bounds, DPI, input, focus), so `compositionController`/`controller` stay null here.
    WUXC::WebView2 control{nullptr};
#else
    WUC::ContainerVisual rootVisual{nullptr};
#endif
    uint64_t id{};
    void (*cb)(uint64_t, const char *){};
    std::wstring pending_url; // navigated once the controller is ready
    std::string startup_error; // retained for eval replies as well as the visible fallback
    double scale{1.0};        // DIP → physical-pixel factor (host-window DPI / 96), the rasterization scale
    // Inline mode (docs/webview.md): the URL prefix of the bundled site under the virtual host
    // (empty = remote mode), the local folder that host maps to (empty = the exe-relative
    // default), and the callback a cancelled external navigation reports through.
    std::wstring inline_prefix;
    uint64_t resource_provider = 0;
    std::wstring inline_dir;
    void (*link_cb)(uint64_t, const char *){};
    void (*tab_cb)(uint64_t, const char *, bool){};
    std::wstring tab_foreground, tab_background;
    // WebView::transparent: the engine's default background is cleared once the controller is
    // up, and the URL label under the render visual is blanked so it cannot show through.
    bool transparent{};
    uint64_t session{};
    std::wstring profile_name;
    std::wstring profile_directory;
    bool private_mode{};
    // Live page state (WebView::page_state). WebView2 raises no progress events, so `progress`
    // steps through fixed points as the navigation events arrive; `last_state` suppresses repeats.
    double progress{1.0};
    std::string last_state;
    void *engine_handle{}; // privately owned; callbacks never capture a released Day node handle
    // WebView::zoom and WebView::javascript, applied once the engine is up and on each change.
    // WinUI zooms through CSS (apply_zoom); `zoom_script` is that build's document-created
    // script id, so a later factor can replace it.
    double zoom{1.0};
    bool scripts{true};
    std::wstring zoom_script;
    // WebView::request_filter (the Rust hooks token, 0 for none) and the page scripts
    // (WebView::user_script, ::user_style, the dayMessage shim), installed when the engine is
    // up and before its first navigation. `pending_top` is the URL the last top-level
    // NavigationStarting named, which is how a Document request is told to be the main frame's.
    uint64_t hooks{};
    std::vector<std::wstring> page_scripts;
    bool content_installed{};
    std::wstring pending_top;
};

// Hold COM objects on the UI apartment; workers only load immutable Rust bytes.
struct ResourcePending {
    wrl::ComPtr<ICoreWebView2Environment> environment;
    wrl::ComPtr<ICoreWebView2WebResourceRequestedEventArgs> args;
    wrl::ComPtr<ICoreWebView2Deferral> deferral;
#ifdef DAY_WINUI
    winrt::Microsoft::UI::Dispatching::DispatcherQueue dispatcher{nullptr};
#else
    winrt::Windows::System::DispatcherQueue dispatcher{nullptr};
#endif
};
static void resource_done(void *context, DayResourceResponse *response) {
    auto pending=std::shared_ptr<ResourcePending>(static_cast<ResourcePending *>(context));
    auto bytes=std::shared_ptr<DayResourceResponse>(response,day_web_resource_free);
    try {
        auto deliver=[pending,bytes] {
            wrl::ComPtr<IStream> stream;
            if (SUCCEEDED(CreateStreamOnHGlobal(nullptr,TRUE,&stream))) {
                ULONG written=0;
                const auto length=day_web_resource_len(bytes.get());
                // IStream::Write takes ULONG; never silently truncate a large response.
                const auto data=day_web_resource_data(bytes.get());
                size_t offset=0;
                while (offset<length) {
                    auto chunk=static_cast<ULONG>((std::min)(length-offset,size_t(64*1024)));
                    if (FAILED(stream->Write(data+offset,chunk,&written)) || written!=chunk) break;
                    offset+=written;
                }
                if (offset==length) {
                    LARGE_INTEGER zero{};stream->Seek(zero,STREAM_SEEK_SET,nullptr);
                    std::string headers=day_web_resource_headers(bytes.get());
                    headers += std::string("Content-Type: ")+day_web_resource_mime(bytes.get())+"\r\n";
                    wrl::ComPtr<ICoreWebView2WebResourceResponse> native;
                    pending->environment->CreateWebResourceResponse(stream.Get(),day_web_resource_status(bytes.get()),L"Resource",hs(headers.c_str()).c_str(),&native);
                    if(native)pending->args->put_Response(native.Get());
                }
            }
            pending->deferral->Complete();
        };
        // CoreDispatcher is a CoreWindow/UWP dispatcher, not the desktop island's
        // message queue. Both desktop stacks use their corresponding DispatcherQueue.
        pending->dispatcher.TryEnqueue(deliver);
    } catch (winrt::hresult_error const &) {
        // The dispatcher can close while the worker is reading. Closing WebView2 cancels
        // its requests; the owned response/deferral are released without re-entering a view.
    }
}

// The virtual host the app's asset tree is mapped under for inline sites. `.example` is
// reserved for exactly this use, the same convention Microsoft's own WebView2 docs model.
static const wchar_t *kDayAssetsHost = L"day-assets.example";

static std::map<void *, WebViewCtx *> g_webviews;
static std::map<uint64_t, WebViewCtx *> g_sessions;

static WebViewCtx *find_ctx(void *handle) {
    auto it = g_webviews.find(handle);
    return it == g_webviews.end() ? nullptr : it->second;
}

// Set once from Rust: WebView::request_filter (true blocks) and page messages (the shim's JSON
// envelope plus the posting document's URI). WebView2 calls both on the UI thread.
static bool (*g_filter_cb)(uint64_t, const char *, const char *, int, bool, const char *) = nullptr;
static void (*g_message_cb)(uint64_t, const char *, const char *) = nullptr;

// ---- JavaScript evaluation (docs/webview-eval.md) --------------------------
//
// One file-static callback shared by every web view; each reply carries its own node id, so the
// Rust side registers it once rather than per view (the Qt shim's arrangement).
static void (*g_eval_cb)(uint64_t, uint64_t, const char *) = nullptr;
// One page-state report, in the front-end's `<loading><back><forward>␟<progress>␟<url>␟<title>`.
static void (*g_state_cb)(uint64_t, const char *) = nullptr;

static void report_state(WebViewCtx *c) {
    // A retained session view between mounts has id 0: nobody to tell.
    if (!g_state_cb || !c || !c->webview || !c->id)
        return;
    LPWSTR src = nullptr, title = nullptr;
    BOOL back = FALSE, forward = FALSE;
    c->webview->get_Source(&src);
    c->webview->get_DocumentTitle(&title);
    c->webview->get_CanGoBack(&back);
    c->webview->get_CanGoForward(&forward);
    std::string text;
    text.push_back(c->loading ? '1' : '0');
    text.push_back(back ? '1' : '0');
    text.push_back(forward ? '1' : '0');
    text.push_back('\x1f');
    char progress[16];
    std::snprintf(progress, sizeof progress, "%.2f", c->loading ? c->progress : 1.0);
    text += progress;
    text.push_back('\x1f');
    // A page can title itself with a lone surrogate, which the conversion refuses: report an
    // empty field rather than throw out of an engine callback.
    auto narrow = [](LPWSTR w) {
        try {
            return day_webview::utf8(std::wstring_view(w ? w : L""));
        } catch (...) {
            return std::string();
        }
    };
    text += narrow(src);
    text.push_back('\x1f');
    text += narrow(title);
    CoTaskMemFree(src);
    CoTaskMemFree(title);
    if (text == c->last_state)
        return;
    c->last_state = text;
    g_state_cb(c->id, text.c_str());
}

static std::string base64(std::string const &bytes) {
    static const char digits[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    std::string out;
    out.reserve((bytes.size() + 2) / 3 * 4);
    size_t i = 0;
    for (; i + 2 < bytes.size(); i += 3) {
        const unsigned v = (unsigned char)bytes[i] << 16 | (unsigned char)bytes[i + 1] << 8 | (unsigned char)bytes[i + 2];
        out += digits[v >> 18];
        out += digits[(v >> 12) & 63];
        out += digits[(v >> 6) & 63];
        out += digits[v & 63];
    }
    if (i < bytes.size()) {
        const unsigned v = (unsigned char)bytes[i] << 16 | (i + 1 < bytes.size() ? (unsigned char)bytes[i + 1] << 8 : 0);
        out += digits[v >> 18];
        out += digits[(v >> 12) & 63];
        out += i + 1 < bytes.size() ? digits[(v >> 6) & 63] : '=';
        out += '=';
    }
    return out;
}

// The bytes of an HGLOBAL-backed stream.
static bool stream_bytes(IStream *stream, std::string &out) {
    HGLOBAL global{};
    STATSTG stat{};
    if (FAILED(GetHGlobalFromStream(stream, &global)) || FAILED(stream->Stat(&stat, STATFLAG_NONAME)))
        return false;
    const void *data = GlobalLock(global);
    if (!data)
        return false;
    out.assign(static_cast<const char *>(data), static_cast<size_t>(stat.cbSize.QuadPart));
    GlobalUnlock(global);
    return true;
}

// Whether the view is on screen, so a capture can complete. WebView2 stops rendering a view that
// is not shown (a background tab, a page under another), and CapturePreview on such a view
// never raises its completion handler: the request would stay unanswered and its caller would
// wait forever. A view counts as shown when it is laid out with a size, and nothing above it is
// collapsed; the controller's own IsVisible is the system-XAML build's equivalent.
static bool view_showing(WebViewCtx *c) {
#ifdef DAY_WINUI
    if (!c->control || !c->control.IsLoaded() || c->control.ActualWidth() <= 0 || c->control.ActualHeight() <= 0)
        return false;
    for (WUX::DependencyObject at = c->control; at; at = WUXM::VisualTreeHelper::GetParent(at)) {
        auto element = at.try_as<WUX::UIElement>();
        if (element && element.Visibility() != WUX::Visibility::Visible)
            return false;
    }
    return true;
#else
    BOOL visible = FALSE;
    return c->controller && SUCCEEDED(c->controller->get_IsVisible(&visible)) && visible;
#endif
}

// CapturePreview's PNG, scaled down to at most `width` pixels wide through WIC when wider.
static bool snapshot_png(IStream *capture, int width, std::string &out) {
    LARGE_INTEGER zero{};
    capture->Seek(zero, STREAM_SEEK_SET, nullptr);
    wrl::ComPtr<IWICImagingFactory> factory;
    wrl::ComPtr<IWICBitmapDecoder> decoder;
    wrl::ComPtr<IWICBitmapFrameDecode> frame;
    UINT w = 0, h = 0;
    if (FAILED(CoCreateInstance(CLSID_WICImagingFactory, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&factory))) ||
        FAILED(factory->CreateDecoderFromStream(capture, nullptr, WICDecodeMetadataCacheOnLoad, &decoder)) ||
        FAILED(decoder->GetFrame(0, &frame)) || FAILED(frame->GetSize(&w, &h)) || w == 0)
        return false;
    if (width <= 0 || w <= static_cast<UINT>(width))
        return stream_bytes(capture, out);
    const UINT th = (std::max)(1u, static_cast<UINT>(static_cast<unsigned long long>(h) * width / w));
    wrl::ComPtr<IWICBitmapScaler> scaler;
    wrl::ComPtr<IWICFormatConverter> converter;
    wrl::ComPtr<IStream> encoded;
    wrl::ComPtr<IWICBitmapEncoder> encoder;
    wrl::ComPtr<IWICBitmapFrameEncode> target;
    WICPixelFormatGUID format = GUID_WICPixelFormat32bppBGRA;
    return SUCCEEDED(factory->CreateBitmapScaler(&scaler)) &&
           SUCCEEDED(scaler->Initialize(frame.Get(), width, th, WICBitmapInterpolationModeFant)) &&
           SUCCEEDED(factory->CreateFormatConverter(&converter)) &&
           SUCCEEDED(converter->Initialize(scaler.Get(), GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, nullptr, 0.0, WICBitmapPaletteTypeCustom)) &&
           SUCCEEDED(CreateStreamOnHGlobal(nullptr, TRUE, &encoded)) &&
           SUCCEEDED(factory->CreateEncoder(GUID_ContainerFormatPng, nullptr, &encoder)) &&
           SUCCEEDED(encoder->Initialize(encoded.Get(), WICBitmapEncoderNoCache)) &&
           SUCCEEDED(encoder->CreateNewFrame(&target, nullptr)) && SUCCEEDED(target->Initialize(nullptr)) &&
           SUCCEEDED(target->SetSize(width, th)) && SUCCEEDED(target->SetPixelFormat(&format)) &&
           SUCCEEDED(target->WriteSource(converter.Get(), nullptr)) && SUCCEEDED(target->Commit()) &&
           SUCCEEDED(encoder->Commit()) && stream_bytes(encoded.Get(), out);
}

// A reply in the 0x1F-separated form the Rust front-end decodes. Only for engine failures; a
// script that merely throws is caught by the JS wrapper and arrives as an ordinary value.
static std::string eval_engine_error(const char *name, const char *message) {
    std::string s = "0";
    s.push_back('\x1f');
    s += name;
    s.push_back('\x1f');
    s += message;
    return s;
}

/// Deliver exactly one reply for `req`. Every path out of an eval must reach this, or the Rust
/// future waits forever: WebView2 releases pending handlers on `Close()`, so a dropped callback
/// is a stranded request rather than a late one.
static void eval_reply(uint64_t id, uint64_t req, std::string const &payload) {
    if (g_eval_cb)
        g_eval_cb(id, req, payload.c_str());
}

/// Decode a JSON string literal into the string it denotes: the fallback path's job.
///
/// `ExecuteScript` hands back the result as JSON, so the wrapper's string return arrives quoted
/// and escaped, and the front-end wants the string itself. `` is the separator the whole
/// protocol is built on, so `\u` decoding (surrogate pairs included) is required, not optional.
/// Returns false when the text is not a JSON string at all, which is an engine-level failure.
static bool json_string_to_utf8(std::wstring const &json, std::string &out) {
    if (json.size() < 2 || json.front() != L'"' || json.back() != L'"')
        return false;
    std::wstring w;
    w.reserve(json.size());
    for (size_t i = 1; i + 1 < json.size(); ++i) {
        wchar_t c = json[i];
        if (c != L'\\') {
            w.push_back(c);
            continue;
        }
        if (++i + 1 > json.size())
            return false;
        switch (json[i]) {
        case L'"': w.push_back(L'"'); break;
        case L'\\': w.push_back(L'\\'); break;
        case L'/': w.push_back(L'/'); break;
        case L'b': w.push_back(L'\b'); break;
        case L'f': w.push_back(L'\f'); break;
        case L'n': w.push_back(L'\n'); break;
        case L'r': w.push_back(L'\r'); break;
        case L't': w.push_back(L'\t'); break;
        case L'u': {
            if (i + 4 >= json.size())
                return false;
            unsigned v = 0;
            for (int k = 1; k <= 4; ++k) {
                wchar_t d = json[i + k];
                v <<= 4;
                if (d >= L'0' && d <= L'9') v |= unsigned(d - L'0');
                else if (d >= L'a' && d <= L'f') v |= unsigned(d - L'a' + 10);
                else if (d >= L'A' && d <= L'F') v |= unsigned(d - L'A' + 10);
                else return false;
            }
            i += 4;
            // A surrogate pair is two \u escapes; UTF-16 is wchar_t's own encoding on Windows, so
            // both halves are pushed as-is and the conversion below pairs them.
            w.push_back(static_cast<wchar_t>(v));
            break;
        }
        default:
            return false;
        }
    }
    out = day_webview::utf8(w);
    return true;
}

#ifndef DAY_WINUI
// System XAML only: sizing the hand-hosted visual and forwarding pointer input to it. WinUI's
// WebView2 control does both itself.
// Match the render visual + controller Bounds to the Border's current size. BoundsMode is
// UseRasterizationScale, so Bounds/visual are in DIPs and RasterizationScale carries the DPI. The
// visual follows the element's position/clipping/transforms automatically (it is a child of the
// element's own composition visual), so only the size needs syncing, and only on resize.
static void sync_size(WebViewCtx *c) {
    if (!c->controller || !c->placeholder)
        return;
    double w = c->placeholder.ActualWidth(), h = c->placeholder.ActualHeight();
    bool show = w > 0 && h > 0;
    if (c->rootVisual)
        c->rootVisual.Size({static_cast<float>(w), static_cast<float>(h)});
    RECT b{0, 0, static_cast<LONG>(std::lround(w)), static_cast<LONG>(std::lround(h))};
    try {
        c->controller->put_Bounds(b);
        c->controller->put_IsVisible(show ? TRUE : FALSE);
    } catch (...) {
    }
}

// Modifier/button state (COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS) for a pointer event.
static COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS vkeys_of(WUInput::PointerPointProperties const &props) {
    uint32_t v = COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE;
    if (props.IsLeftButtonPressed())
        v |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_LEFT_BUTTON;
    if (props.IsRightButtonPressed())
        v |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_RIGHT_BUTTON;
    if (props.IsMiddleButtonPressed())
        v |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_MIDDLE_BUTTON;
    if (GetKeyState(VK_CONTROL) < 0)
        v |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_CONTROL;
    if (GetKeyState(VK_SHIFT) < 0)
        v |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_SHIFT;
    return static_cast<COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS>(v);
}

// The pointer position relative to the Border, in DIPs, the WebView2's Bounds coordinate space.
static POINT point_of(WebViewCtx *c, WUXI::PointerRoutedEventArgs const &e) {
    auto pos = e.GetCurrentPoint(c->placeholder).Position();
    return POINT{static_cast<LONG>(std::lround(pos.X)), static_cast<LONG>(std::lround(pos.Y))};
}

// Wire the Border's XAML pointer events to the composition controller. This is the crux of windowless
// hosting: XAML's input site delivers pointer input to the Border, and we forward it to the browser.
static void wire_input(void *handle) {
    auto *c = find_ctx(handle);
    if (!c)
        return;
    auto &pl = c->placeholder;

    pl.PointerMoved([handle](WF::IInspectable const &, WUXI::PointerRoutedEventArgs const &e) {
        auto *cc = find_ctx(handle);
        if (!cc || !cc->compositionController)
            return;
        auto pp = e.GetCurrentPoint(cc->placeholder);
        cc->compositionController->SendMouseInput(COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE,
                                                  vkeys_of(pp.Properties()), 0, point_of(cc, e));
        e.Handled(true);
    });

    pl.PointerPressed([handle](WF::IInspectable const &, WUXI::PointerRoutedEventArgs const &e) {
        auto *cc = find_ctx(handle);
        if (!cc || !cc->compositionController)
            return;
        cc->placeholder.CapturePointer(e.Pointer()); // keep move/up during a drag outside the element
        if (cc->controller)
            cc->controller->MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
        auto pp = e.GetCurrentPoint(cc->placeholder);
        COREWEBVIEW2_MOUSE_EVENT_KIND kind;
        switch (pp.Properties().PointerUpdateKind()) {
        case WUInput::PointerUpdateKind::LeftButtonPressed:
            kind = COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN;
            break;
        case WUInput::PointerUpdateKind::RightButtonPressed:
            kind = COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN;
            break;
        case WUInput::PointerUpdateKind::MiddleButtonPressed:
            kind = COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN;
            break;
        default:
            return;
        }
        cc->compositionController->SendMouseInput(kind, vkeys_of(pp.Properties()), 0, point_of(cc, e));
        e.Handled(true);
    });

    pl.PointerReleased([handle](WF::IInspectable const &, WUXI::PointerRoutedEventArgs const &e) {
        auto *cc = find_ctx(handle);
        if (!cc || !cc->compositionController)
            return;
        auto pp = e.GetCurrentPoint(cc->placeholder);
        COREWEBVIEW2_MOUSE_EVENT_KIND kind;
        switch (pp.Properties().PointerUpdateKind()) {
        case WUInput::PointerUpdateKind::LeftButtonReleased:
            kind = COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP;
            break;
        case WUInput::PointerUpdateKind::RightButtonReleased:
            kind = COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP;
            break;
        case WUInput::PointerUpdateKind::MiddleButtonReleased:
            kind = COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP;
            break;
        default:
            kind = COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP;
            break;
        }
        cc->compositionController->SendMouseInput(kind, vkeys_of(pp.Properties()), 0, point_of(cc, e));
        cc->placeholder.ReleasePointerCapture(e.Pointer());
        e.Handled(true);
    });

    pl.PointerWheelChanged([handle](WF::IInspectable const &, WUXI::PointerRoutedEventArgs const &e) {
        auto *cc = find_ctx(handle);
        if (!cc || !cc->compositionController)
            return;
        auto props = e.GetCurrentPoint(cc->placeholder).Properties();
        auto kind = props.IsHorizontalMouseWheel() ? COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL
                                                    : COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL;
        cc->compositionController->SendMouseInput(kind, vkeys_of(props),
                                                  static_cast<UINT32>(props.MouseWheelDelta()),
                                                  point_of(cc, e));
        e.Handled(true); // don't let a parent ScrollViewer also scroll
    });

    pl.PointerExited([handle](WF::IInspectable const &, WUXI::PointerRoutedEventArgs const &e) {
        auto *cc = find_ctx(handle);
        if (!cc || !cc->compositionController)
            return;
        cc->compositionController->SendMouseInput(COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE,
                                                  COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE, 0,
                                                  POINT{0, 0});
    });
}

#endif // !DAY_WINUI

static void destroy_ctx(void *handle) {
    auto it = g_webviews.find(handle);
    if (it == g_webviews.end())
        return;
    WebViewCtx *c = it->second;
#ifdef DAY_WINUI
    if (c->control) {
        try {
            c->control.Close();
        } catch (...) {
        }
    }
#else
    if (c->placeholder) {
        try {
            WUXH::ElementCompositionPreview::SetElementChildVisual(c->placeholder, nullptr);
        } catch (...) {
        }
    }
#endif
    if (c->controller) {
        try {
            c->controller->Close();
        } catch (...) {
        }
    }
    g_webviews.erase(it);
    delete c;
    day_xaml_delete(handle);
}

static std::wstring user_data_folder() {
    wchar_t buf[MAX_PATH]{};
    DWORD n = GetTempPathW(MAX_PATH, buf);
    std::wstring p(buf, n);
    p += L"day-webview2";
    return p;
}

static void startup_failed(void *handle, const char *stage, HRESULT hr) {
    auto *c = find_ctx(handle);
    if (!c)
        return;
    char code[16];
    std::snprintf(code, sizeof(code), "0x%08lX", static_cast<unsigned long>(hr));
    c->startup_error = std::string("WebView2 ") + stage + " failed (" + code + ")";
    if (hr == HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND))
        c->startup_error += "; install the Microsoft Edge WebView2 Runtime";
    std::fprintf(stderr, "day-piece-webview: %s\n", c->startup_error.c_str());
    std::fflush(stderr);
    auto label = c->placeholder.Child().try_as<WUXC::TextBlock>();
#ifdef DAY_WINUI
    // Under WinUI the control has taken the label's place by now: put a label back.
    if (!label) {
        label = WUXC::TextBlock{};
        label.Margin(WUX::Thickness{8, 8, 8, 8});
        label.Opacity(0.6);
        c->placeholder.Child(label);
    }
#endif
    if (label) {
        label.Text(hs(c->startup_error.c_str()));
        label.TextWrapping(WUX::TextWrapping::Wrap);
    }
}

// The engine is up (both stacks): its background, the inline asset host, the link and
// navigation handlers, then the first navigation. Split out of the controller callback so the
// WinUI build, whose engine comes from the WebView2 control instead, runs the same wiring.
// WebView::javascript: ICoreWebView2Settings::IsScriptEnabled, which governs the next
// navigation's document scripts only. ExecuteScript still runs (WebView2 documents this), so the
// app's evaluation and page state keep working with page scripts off.
static void apply_scripts(WebViewCtx *c) {
    wrl::ComPtr<ICoreWebView2Settings> settings;
    if (c->webview && SUCCEEDED(c->webview->get_Settings(&settings)) && settings)
        settings->put_IsScriptEnabled(c->scripts ? TRUE : FALSE);
}

// WebView::zoom. The system-XAML host owns the controller and sets its ZoomFactor, the engine's
// own zoom, which persists across navigations. The WinUI control keeps its controller (and with
// it ZoomFactor) to itself, so that build sets CSS `zoom` on the page now and on each document
// from its creation on.
static void apply_zoom(WebViewCtx *c) {
#ifdef DAY_WINUI
    if (!c->webview)
        return;
    if (!c->zoom_script.empty()) {
        c->webview->RemoveScriptToExecuteOnDocumentCreated(c->zoom_script.c_str());
        c->zoom_script.clear();
    }
    // A whole percentage keeps the number out of the C locale's decimal separator.
    const long percent = std::lround(c->zoom * 100.0);
    const std::wstring value = percent == 100 ? std::wstring() : std::to_wstring(percent) + L"%";
    const std::wstring script =
        L"(()=>{const a=()=>{const e=document.documentElement;if(e)e.style.zoom='" + value +
        L"';};a();if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',a);})()";
    c->webview->ExecuteScript(script.c_str(), nullptr);
    if (percent == 100)
        return;
    void *handle = c->engine_handle;
    c->webview->AddScriptToExecuteOnDocumentCreated(
        script.c_str(),
        wrl::Callback<ICoreWebView2AddScriptToExecuteOnDocumentCreatedCompletedHandler>(
            [handle, percent](HRESULT hr, LPCWSTR id) -> HRESULT {
                auto *cc = find_ctx(handle);
                if (!cc || FAILED(hr) || !id || !cc->webview)
                    return S_OK;
                // The factor changed again while this script was being added: it is stale.
                if (std::lround(cc->zoom * 100.0) != percent || !cc->zoom_script.empty())
                    cc->webview->RemoveScriptToExecuteOnDocumentCreated(id);
                else
                    cc->zoom_script = id;
                return S_OK;
            })
            .Get());
#else
    if (c->controller)
        c->controller->put_ZoomFactor(c->zoom);
#endif
}

// The first navigation, once every page script has been added: a document created before
// AddScriptToExecuteOnDocumentCreated completes would run without it.
static void navigate_first(WebViewCtx *c) {
    if (c->webview && !c->pending_url.empty())
        c->webview->Navigate(c->pending_url.c_str());
}

// Ask WebView::request_filter about a request (RequestHooks::decide, which passes a provider's
// pages where the provider is the app's own site). False when the view has no filter.
static bool filter_blocks(WebViewCtx *cc, ICoreWebView2WebResourceRequestedEventArgs *args,
                          const std::wstring &url, const std::wstring &verb) {
    if (!cc || !cc->hooks || !g_filter_cb)
        return false;
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT context = COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL;
    args->get_ResourceContext(&context);
    // WebView2 names no frame: a Document request for the URL the top-level
    // NavigationStarting just announced is the main frame's.
    const bool main = context == COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT && url == cc->pending_top;
    const std::string u = to_utf8(winrt::hstring{url}), m = to_utf8(winrt::hstring{verb});
    return g_filter_cb(cc->hooks, u.c_str(), "", static_cast<int>(context), main, m.c_str());
}

// Answer a blocked request with an empty 403.
static void answer_blocked(ICoreWebView2Environment *environment, ICoreWebView2WebResourceRequestedEventArgs *args) {
    wrl::ComPtr<ICoreWebView2WebResourceResponse> blocked;
    if (environment && SUCCEEDED(environment->CreateWebResourceResponse(nullptr, 403, L"Blocked", L"", &blocked)) && blocked)
        args->put_Response(blocked.Get());
}

// WebView::request_filter, the page scripts and the page message channel, then the first page.
static void install_content(void *handle, WebViewCtx *c) {
    if (!c->webview || c->content_installed) {
        navigate_first(c);
        return;
    }
    c->content_installed = true;
    wrl::ComPtr<ICoreWebView2_2> wv2;
    wrl::ComPtr<ICoreWebView2Environment> environment;
    if (SUCCEEDED(c->webview.As(&wv2)) && wv2)
        wv2->get_Environment(&environment);
    wrl::ComPtr<ICoreWebView2_22> filters;
    if (c->hooks && environment && SUCCEEDED(c->webview.As(&filters)) && filters) {
        // Every request from the page, its frames and its workers. Each one crosses to this UI
        // thread and waits for the filter, so the filter must be quick.
        filters->AddWebResourceRequestedFilterWithRequestSourceKinds(
            L"*", COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL, COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL);
        EventRegistrationToken token{};
        c->webview->add_WebResourceRequested(wrl::Callback<ICoreWebView2WebResourceRequestedEventHandler>(
            [handle, environment](ICoreWebView2 *, ICoreWebView2WebResourceRequestedEventArgs *args) -> HRESULT {
                auto *cc = find_ctx(handle);
                if (!cc || !cc->hooks || !g_filter_cb)
                    return S_OK;
                wrl::ComPtr<ICoreWebView2WebResourceRequest> request;
                if (FAILED(args->get_Request(&request)) || !request)
                    return S_OK;
                LPWSTR uri = nullptr, method = nullptr;
                request->get_Uri(&uri);
                request->get_Method(&method);
                const std::wstring url = uri ? uri : L"";
                const std::wstring verb = method ? method : L"GET";
                CoTaskMemFree(uri);
                CoTaskMemFree(method);
                // The provider's own namespace goes through the resource handler, which asks the
                // filter itself before serving.
                if (url.rfind(L"https://day-resource.invalid/", 0) == 0)
                    return S_OK;
                if (filter_blocks(cc, args, url, verb))
                    answer_blocked(environment.Get(), args);
                return S_OK;
            }).Get(), &token);
        c->webview->add_NavigationStarting(wrl::Callback<ICoreWebView2NavigationStartingEventHandler>(
            [handle](ICoreWebView2 *, ICoreWebView2NavigationStartingEventArgs *args) -> HRESULT {
                auto *cc = find_ctx(handle);
                LPWSTR uri = nullptr;
                if (cc && SUCCEEDED(args->get_Uri(&uri)) && uri) {
                    cc->pending_top = uri;
                    CoTaskMemFree(uri);
                }
                return S_OK;
            }).Get(), &token);
    }
    // Page messages: the top-level document's `chrome.webview.postMessage` (iframes post to
    // their own CoreWebView2Frame, which this channel does not hear).
    EventRegistrationToken messageToken{};
    c->webview->add_WebMessageReceived(wrl::Callback<ICoreWebView2WebMessageReceivedEventHandler>(
        [handle](ICoreWebView2 *, ICoreWebView2WebMessageReceivedEventArgs *args) -> HRESULT {
            auto *cc = find_ctx(handle);
            if (!cc || !g_message_cb || !cc->id)
                return S_OK;
            LPWSTR text = nullptr, source = nullptr;
            if (FAILED(args->TryGetWebMessageAsString(&text)) || !text)
                return S_OK;
            args->get_Source(&source);
            const std::string message = to_utf8(winrt::hstring{text});
            const std::string from = source ? to_utf8(winrt::hstring{source}) : std::string();
            CoTaskMemFree(text);
            CoTaskMemFree(source);
            g_message_cb(cc->id, message.c_str(), from.c_str());
            return S_OK;
        }).Get(), &messageToken);
    if (c->page_scripts.empty()) {
        navigate_first(c);
        return;
    }
    auto remaining = std::make_shared<size_t>(c->page_scripts.size());
    for (const auto &script : c->page_scripts) {
        c->webview->AddScriptToExecuteOnDocumentCreated(
            script.c_str(),
            wrl::Callback<ICoreWebView2AddScriptToExecuteOnDocumentCreatedCompletedHandler>(
                [handle, remaining](HRESULT, LPCWSTR) -> HRESULT {
                    if (--*remaining == 0)
                        if (auto *cc = find_ctx(handle))
                            navigate_first(cc);
                    return S_OK;
                }).Get());
    }
}

static void on_engine_ready(void *handle, WebViewCtx *c2) {
    if (c2->transparent) {
#ifdef DAY_WINUI
        if (c2->control)
            c2->control.DefaultBackgroundColor(WUI::Color{0, 0, 0, 0});
#else
        wrl::ComPtr<ICoreWebView2Controller2> ctl2;
        if (SUCCEEDED(c2->controller.As(&ctl2)) && ctl2)
            ctl2->put_DefaultBackgroundColor(COREWEBVIEW2_COLOR{0, 0, 0, 0});
        if (auto tb = c2->placeholder.Child().try_as<WUXC::TextBlock>())
            tb.Text(L"");
#endif
    }

    if (c2->webview && c2->resource_provider) {
        // The environment builds the responses; both stacks reach it through the core object.
        wrl::ComPtr<ICoreWebView2_2> wv2;
        wrl::ComPtr<ICoreWebView2Environment> environment;
        if (SUCCEEDED(c2->webview.As(&wv2)) && wv2)
            wv2->get_Environment(&environment);
        wrl::ComPtr<ICoreWebView2_22> resources;
        if (!environment || FAILED(c2->webview.As(&resources))) {
            startup_failed(handle,"resource interception interface",E_NOINTERFACE);return;
        }
        resources->AddWebResourceRequestedFilterWithRequestSourceKinds(L"https://day-resource.invalid/*",COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_DOCUMENT);
        EventRegistrationToken token{};
        c2->webview->add_WebResourceRequested(wrl::Callback<ICoreWebView2WebResourceRequestedEventHandler>(
            [handle,environment](ICoreWebView2 *,ICoreWebView2WebResourceRequestedEventArgs *args)->HRESULT {
                auto c=find_ctx(handle);if(!c)return S_OK;
                wrl::ComPtr<ICoreWebView2WebResourceRequest> request;args->get_Request(&request);
                LPWSTR url=nullptr,method=nullptr,range=nullptr;
                request->get_Uri(&url);request->get_Method(&method);
                // A request filter's "*" filter raises this event for every request too.
                if(!url || std::wstring_view(url).rfind(L"https://day-resource.invalid/",0)!=0){CoTaskMemFree(url);CoTaskMemFree(method);return S_OK;}
                // The provider's pages go to the request filter first, as on every engine.
                if(filter_blocks(c,args,url,method?method:L"GET")){answer_blocked(environment.Get(),args);CoTaskMemFree(url);CoTaskMemFree(method);return S_OK;}
                wrl::ComPtr<ICoreWebView2HttpRequestHeaders> headers;request->get_Headers(&headers);
                if(headers)headers->GetHeader(L"Range",&range);
                auto pending=new ResourcePending();pending->environment=environment;pending->args=args;
#ifdef DAY_WINUI
                pending->dispatcher=c->placeholder.DispatcherQueue();
#else
                pending->dispatcher=winrt::Windows::System::DispatcherQueue::GetForCurrentThread();
#endif
                if(FAILED(args->GetDeferral(&pending->deferral)) || !pending->deferral){delete pending;CoTaskMemFree(url);CoTaskMemFree(method);CoTaskMemFree(range);return E_FAIL;}
                day_web_resource_start(c->resource_provider,to_string(url?url:L"").c_str(),to_string(method?method:L"GET").c_str(),to_string(range?range:L"").c_str(),pending,resource_done);
                CoTaskMemFree(url);CoTaskMemFree(method);CoTaskMemFree(range);return S_OK;
            }).Get(),&token);
    }
    if(c2->webview) {
        EventRegistrationToken token{};
        c2->webview->add_NewWindowRequested(wrl::Callback<ICoreWebView2NewWindowRequestedEventHandler>(
            [handle](ICoreWebView2 *,ICoreWebView2NewWindowRequestedEventArgs *args)->HRESULT {
                auto c=find_ctx(handle);if(!c||!c->tab_cb)return S_OK;
                args->put_Handled(TRUE); LPWSTR uri=nullptr;
                if(SUCCEEDED(args->get_Uri(&uri))&&uri) { auto url=to_utf8(winrt::hstring{uri}); c->tab_cb(c->id,url.c_str(),false);CoTaskMemFree(uri); }
                return S_OK;
            }).Get(),&token);
        wrl::ComPtr<ICoreWebView2_2> menuCore;
        wrl::ComPtr<ICoreWebView2Environment> environment;
        if(SUCCEEDED(c2->webview.As(&menuCore)))menuCore->get_Environment(&environment);
        wrl::ComPtr<ICoreWebView2_11> menus;
        wrl::ComPtr<ICoreWebView2Environment9> menuEnvironment;
        if(environment && SUCCEEDED(c2->webview.As(&menus)) && SUCCEEDED(environment->QueryInterface(IID_PPV_ARGS(&menuEnvironment)))) {
            menus->add_ContextMenuRequested(wrl::Callback<ICoreWebView2ContextMenuRequestedEventHandler>(
                [handle,menuEnvironment](ICoreWebView2 *,ICoreWebView2ContextMenuRequestedEventArgs *args)->HRESULT {
                    auto c=find_ctx(handle);if(!c||!c->tab_cb)return S_OK;
                    wrl::ComPtr<ICoreWebView2ContextMenuTarget> target;args->get_ContextMenuTarget(&target);
                    BOOL hasLink=FALSE;if(!target||FAILED(target->get_HasLinkUri(&hasLink))||!hasLink)return S_OK;
                    LPWSTR uri=nullptr;if(FAILED(target->get_LinkUri(&uri))||!uri)return S_OK;
                    auto url=to_utf8(winrt::hstring{uri});CoTaskMemFree(uri);
                    wrl::ComPtr<ICoreWebView2ContextMenuItemCollection> items;args->get_MenuItems(&items);
                    for(int background=0;background<2;++background) {
                        wrl::ComPtr<ICoreWebView2ContextMenuItem> item;
                        auto &label=background?c->tab_background:c->tab_foreground;
                        if(FAILED(menuEnvironment->CreateContextMenuItem(label.c_str(),nullptr,COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_COMMAND,&item)))continue;
                        EventRegistrationToken selected{};
                        item->add_CustomItemSelected(wrl::Callback<ICoreWebView2CustomItemSelectedEventHandler>(
                            [handle,url,background](ICoreWebView2ContextMenuItem *,IUnknown *)->HRESULT {auto c=find_ctx(handle);if(c&&c->tab_cb)c->tab_cb(c->id,url.c_str(),background!=0);return S_OK;}).Get(),&selected);
                        items->InsertValueAtIndex(background,item.Get());
                    }
                    return S_OK;
                }).Get(),&token);
        }
    }
    if (c2->webview && !c2->inline_prefix.empty()) {
        // Inline mode: map the exe-relative assets tree under the virtual
        // host before the first Navigate, then police top-level
        // navigations against the site prefix; leaving ones are
        // canceled and reported (the Rust side runs the LinkPolicy;
        // events are enqueue-only, so the verdict can't come back here).
        wrl::ComPtr<ICoreWebView2_3> wv3;
        if (!c2->resource_provider && SUCCEEDED(c2->webview->QueryInterface(IID_PPV_ARGS(&wv3))) &&
            wv3) {
            // The Rust side resolves the tree (lib-xaml.rs `inline_dir`),
            // since a dev run reads its assets from the project and a
            // packed app from beside the exe. The exe-relative path is
            // the fallback for a caller that passed nothing: mapping a
            // folder that does not exist leaves the host unmapped, and
            // the site's first navigation then fails DNS resolution.
            std::wstring assets = c2->inline_dir;
            if (assets.empty()) {
                wchar_t exe[MAX_PATH]{};
                GetModuleFileNameW(nullptr, exe, MAX_PATH);
                assets = exe;
                size_t slash = assets.find_last_of(L"\\/");
                if (slash != std::wstring::npos)
                    assets.resize(slash);
                assets += L"\\assets";
            }
            const HRESULT mapped = wv3->SetVirtualHostNameToFolderMapping(
                kDayAssetsHost, assets.c_str(),
                COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_ALLOW);
            if (FAILED(mapped)) {
                startup_failed(handle, "asset folder mapping", mapped);
                return;
            }
        } else if (!c2->resource_provider) {
            startup_failed(handle, "asset mapping interface", E_NOINTERFACE);
            return;
        }
        EventRegistrationToken navTok{};
        c2->webview->add_NavigationStarting(
            wrl::Callback<ICoreWebView2NavigationStartingEventHandler>(
                [handle](ICoreWebView2 *,
                         ICoreWebView2NavigationStartingEventArgs *args)
                    -> HRESULT {
                    auto *cn = find_ctx(handle);
                    if (!cn || cn->inline_prefix.empty())
                        return S_OK;
                    LPWSTR uri = nullptr;
                    if (FAILED(args->get_Uri(&uri)) || !uri)
                        return S_OK;
                    std::wstring u(uri);
                    CoTaskMemFree(uri);
                    const bool inside =
                        u.rfind(cn->inline_prefix, 0) == 0 ||
                        u == L"about:blank";
                    if (!inside) {
                        args->put_Cancel(TRUE);
                        if (cn->link_cb) {
                            std::string s = to_utf8(winrt::hstring{u});
                            cn->link_cb(cn->id, s.c_str());
                        }
                    }
                    return S_OK;
                })
                .Get(),
            &navTok);
        // window.open / target=_blank: external by definition, since no new
        // window exists in day's tree, so report and swallow.
        EventRegistrationToken winTok{};
        c2->webview->add_NewWindowRequested(
            wrl::Callback<ICoreWebView2NewWindowRequestedEventHandler>(
                [handle](ICoreWebView2 *,
                         ICoreWebView2NewWindowRequestedEventArgs *args)
                    -> HRESULT {
                    auto *cn = find_ctx(handle);
                    if (!cn || cn->tab_cb || cn->inline_prefix.empty())
                        return S_OK;
                    args->put_Handled(TRUE);
                    LPWSTR uri = nullptr;
                    if (SUCCEEDED(args->get_Uri(&uri)) && uri) {
                        std::string s =
                            to_utf8(winrt::hstring{std::wstring(uri)});
                        if (cn->link_cb)
                            cn->link_cb(cn->id, s.c_str());
                        CoTaskMemFree(uri);
                    }
                    return S_OK;
                })
                .Get(),
            &winTok);
    }
    if (c2->webview) {
        EventRegistrationToken startToken{};
        c2->webview->add_NavigationStarting(wrl::Callback<ICoreWebView2NavigationStartingEventHandler>([handle](ICoreWebView2 *,ICoreWebView2NavigationStartingEventArgs *)->HRESULT {if(auto *ctx=find_ctx(handle)){ctx->loading=true;ctx->progress=0.1;report_state(ctx);}return S_OK;}).Get(),&startToken);
        // Page state: the remaining milestones of a load, and every title, history and URL change
        // (SourceChanged covers same-document navigations too).
        EventRegistrationToken stateToken{};
        c2->webview->add_ContentLoading(wrl::Callback<ICoreWebView2ContentLoadingEventHandler>([handle](ICoreWebView2 *,ICoreWebView2ContentLoadingEventArgs *)->HRESULT {if(auto *ctx=find_ctx(handle)){ctx->progress=(std::max)(ctx->progress,0.5);report_state(ctx);}return S_OK;}).Get(),&stateToken);
        c2->webview->add_DocumentTitleChanged(wrl::Callback<ICoreWebView2DocumentTitleChangedEventHandler>([handle](ICoreWebView2 *,IUnknown *)->HRESULT {report_state(find_ctx(handle));return S_OK;}).Get(),&stateToken);
        c2->webview->add_HistoryChanged(wrl::Callback<ICoreWebView2HistoryChangedEventHandler>([handle](ICoreWebView2 *,IUnknown *)->HRESULT {report_state(find_ctx(handle));return S_OK;}).Get(),&stateToken);
        c2->webview->add_SourceChanged(wrl::Callback<ICoreWebView2SourceChangedEventHandler>([handle](ICoreWebView2 *,ICoreWebView2SourceChangedEventArgs *)->HRESULT {report_state(find_ctx(handle));return S_OK;}).Get(),&stateToken);
        wrl::ComPtr<ICoreWebView2_2> dom;
        if (SUCCEEDED(c2->webview.As(&dom)) && dom)
            dom->add_DOMContentLoaded(wrl::Callback<ICoreWebView2DOMContentLoadedEventHandler>([handle](ICoreWebView2 *,ICoreWebView2DOMContentLoadedEventArgs *)->HRESULT {if(auto *ctx=find_ctx(handle)){ctx->progress=(std::max)(ctx->progress,0.8);report_state(ctx);}return S_OK;}).Get(),&stateToken);
        // Report the settled URL back so the app's URL bar follows navigation.
        EventRegistrationToken tok{};
        c2->webview->add_NavigationCompleted(
            wrl::Callback<ICoreWebView2NavigationCompletedEventHandler>(
                [handle](ICoreWebView2 *wv,
                         ICoreWebView2NavigationCompletedEventArgs *args)
                    -> HRESULT {
                    auto *c3n = find_ctx(handle);
                    if (!c3n)
                        return S_OK;
                    c3n->loading=false;
                    LPWSTR src = nullptr;
                    if (SUCCEEDED(wv->get_Source(&src)) && src) {
                        std::string s = to_utf8(winrt::hstring{src});
                        BOOL success = FALSE;
                        args->get_IsSuccess(&success);
                        if (!success) {
                            COREWEBVIEW2_WEB_ERROR_STATUS error{};
                            args->get_WebErrorStatus(&error);
                            std::fprintf(stderr,
                                "day-piece-webview: navigation failed "
                                "(WebErrorStatus %d): %s\n",
                                static_cast<int>(error), s.c_str());
                            std::fflush(stderr);
                        }
                        if (c3n->cb)
                            c3n->cb(c3n->id, s.c_str());
                        CoTaskMemFree(src);
                    }
                    c3n->progress = 1.0;
                    report_state(c3n);
                    return S_OK;
                })
                .Get(),
            &tok);
    }
#ifndef DAY_WINUI
    wire_input(handle);
    sync_size(c2);
#endif
    // WebView::javascript and WebView::zoom, before the first navigation.
    apply_scripts(c2);
    if (c2->zoom != 1.0)
        apply_zoom(c2);
    install_content(handle, c2);
}

#ifdef DAY_WINUI
// WinUI 3: the engine lives in the platform's own WebView2 control, which owns hosting, input,
// DPI and focus. It is created in the placeholder, given an environment with the same user-data
// folder and browser arguments the system-XAML path uses, and once its core exists that core's
// COM object (the ICoreWebView2 the rest of this file drives) goes through on_engine_ready.
// Completion handlers hop to the UI thread through the control's DispatcherQueue, since the
// asynchronous operations promise nothing about which thread completes them.
static void on_ui(void *handle, std::function<void(WebViewCtx *)> fn) {
    auto *c = find_ctx(handle);
    if (!c || !c->placeholder)
        return;
    c->placeholder.DispatcherQueue().TryEnqueue([handle, fn] {
        if (auto *cc = find_ctx(handle))
            fn(cc);
    });
}

// WebView2Loader.dll, which the control's Microsoft.Web.WebView2.Core.dll loads by name and which
// is the app's to supply (it is not in the Windows App SDK). Loaded once, from beside the exe (a
// packed app) or else from the package the build used (a development build); a module already
// in the process satisfies the later load by name. Without it the control fails with
// 0x8007007E (module not found).
// The same holds for the control's WinRT core, Microsoft.Web.WebView2.Core.dll: the WebView2
// package ships it for the app to carry, and the class registrations name it by file name.
static void preload_one(const wchar_t *name, const wchar_t *fallback) {
    if (GetModuleHandleW(name))
        return;
    wchar_t exe[MAX_PATH]{};
    DWORD n = GetModuleFileNameW(nullptr, exe, MAX_PATH);
    if (n > 0 && n < MAX_PATH) {
        std::wstring local(exe);
        local.resize(local.find_last_of(L"\\/") + 1);
        local += name;
        if (LoadLibraryExW(local.c_str(), nullptr, 0))
            return;
    }
    if (fallback && !LoadLibraryExW(fallback, nullptr, 0))
        std::fprintf(stderr, "day-piece-webview: could not load %ls (0x%08lX)\n", name,
                     static_cast<unsigned long>(GetLastError()));
}

static void preload_webview2_loader() {
    static bool done = false;
    if (done)
        return;
    done = true;
#ifdef DAY_WEBVIEW2_LOADER_DLL
    preload_one(L"WebView2Loader.dll", DAY_WEBVIEW2_LOADER_DLL);
#else
    preload_one(L"WebView2Loader.dll", nullptr);
#endif
#ifdef DAY_WEBVIEW2_CORE_DLL
    preload_one(L"Microsoft.Web.WebView2.Core.dll", DAY_WEBVIEW2_CORE_DLL);
#else
    preload_one(L"Microsoft.Web.WebView2.Core.dll", nullptr);
#endif
}

static void create_webview2(void *handle) {
    namespace WV2 = winrt::Microsoft::Web::WebView2::Core;
    auto *c = find_ctx(handle);
    if (!c)
        return;
    preload_webview2_loader();
    try {
        WUXC::WebView2 control;
        control.HorizontalAlignment(WUX::HorizontalAlignment::Stretch);
        control.VerticalAlignment(WUX::VerticalAlignment::Stretch);
        c->control = control;
        c->placeholder.Child(control);
        WV2::CoreWebView2EnvironmentOptions options;
        options.AdditionalBrowserArguments(L"--disable-features=CalculateNativeWinOcclusion");
        auto env = WV2::CoreWebView2Environment::CreateWithOptionsAsync(
            L"", winrt::hstring{c->profile_directory.empty() ? user_data_folder() : c->profile_directory}, options);
        env.Completed([handle](auto const &op, WF::AsyncStatus status) {
            const HRESULT failed = status == WF::AsyncStatus::Completed ? S_OK : op.ErrorCode().value;
            WV2::CoreWebView2Environment environment{nullptr};
            if (SUCCEEDED(failed))
                environment = op.GetResults();
            on_ui(handle, [handle, failed, environment](WebViewCtx *cc) {
                if (FAILED(failed) || !environment) {
                    startup_failed(handle, "environment creation", FAILED(failed) ? failed : E_POINTER);
                    return;
                }
                auto controllerOptions = environment.CreateCoreWebView2ControllerOptions();
                controllerOptions.ProfileName(winrt::hstring{cc->profile_name});
                controllerOptions.IsInPrivateModeEnabled(cc->private_mode);
                auto ensure = cc->control.EnsureCoreWebView2Async(environment, controllerOptions);
                ensure.Completed([handle](auto const &op2, WF::AsyncStatus status2) {
                    const HRESULT failed2 =
                        status2 == WF::AsyncStatus::Completed ? S_OK : op2.ErrorCode().value;
                    on_ui(handle, [handle, failed2](WebViewCtx *c2) {
                        if (FAILED(failed2)) {
                            startup_failed(handle, "engine creation", failed2);
                            return;
                        }
                        auto core = c2->control.CoreWebView2();
                        HRESULT got = E_POINTER;
                        if (core) {
                            auto interop = core.try_as<ICoreWebView2Interop2>();
                            got = interop ? interop->GetComICoreWebView2(
                                                c2->webview.ReleaseAndGetAddressOf())
                                          : E_NOINTERFACE;
                        }
                        if (FAILED(got) || !c2->webview) {
                            startup_failed(handle, "engine interface", FAILED(got) ? got : E_POINTER);
                            return;
                        }
                        on_engine_ready(handle, c2);
                    });
                });
            });
        });
    } catch (winrt::hresult_error const &e) {
        startup_failed(handle, "control creation", e.code().value);
    }
}
#else
// Kick off async WebView2 creation: environment → composition controller → attach the render visual,
// wire input + NavigationCompleted, size, navigate. All callbacks run on the UI thread (WebView2 posts
// to the creating thread), so touching XAML / composition / g_webviews here is safe.
static void create_webview2(void *handle) {
    auto *c = find_ctx(handle);
    if (!c)
        return;
    std::wstring udf = c->profile_directory.empty() ? user_data_folder() : c->profile_directory;
    auto options = wrl::Make<CoreWebView2EnvironmentOptions>();
    if (options)
        options->put_AdditionalBrowserArguments(L"--disable-features=CalculateNativeWinOcclusion");
    const HRESULT started = CreateCoreWebView2EnvironmentWithOptions(
        nullptr, udf.c_str(), options.Get(),
        wrl::Callback<ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler>(
            [handle](HRESULT r, ICoreWebView2Environment *env) -> HRESULT {
                auto *cc = find_ctx(handle);
                if (!cc)
                    return S_OK;
                if (FAILED(r) || !env) {
                    startup_failed(handle, "environment creation", FAILED(r) ? r : E_POINTER);
                    return S_OK;
                }
                wrl::ComPtr<ICoreWebView2Environment3> env3;
                const HRESULT queried = env->QueryInterface(IID_PPV_ARGS(&env3));
                if (FAILED(queried) || !env3) {
                    startup_failed(handle, "composition interface",
                                   FAILED(queried) ? queried : E_NOINTERFACE);
                    return S_OK;
                }
                auto completion = wrl::Callback<ICoreWebView2CreateCoreWebView2CompositionControllerCompletedHandler>(
                        [handle](HRESULT r2, ICoreWebView2CompositionController *comp) -> HRESULT {
                            auto *c2 = find_ctx(handle);
                            if (!c2)
                                return S_OK;
                            if (FAILED(r2) || !comp) {
                                startup_failed(handle, "controller creation",
                                               FAILED(r2) ? r2 : E_POINTER);
                                return S_OK;
                            }
                            c2->compositionController = comp;
                            const HRESULT controller = c2->compositionController.As(&c2->controller);
                            if (FAILED(controller) || !c2->controller) {
                                startup_failed(handle, "controller interface",
                                               FAILED(controller) ? controller : E_NOINTERFACE);
                                return S_OK;
                            }
                            const HRESULT engine =
                                c2->controller->get_CoreWebView2(c2->webview.GetAddressOf());
                            if (FAILED(engine) || !c2->webview) {
                                startup_failed(handle, "engine creation", FAILED(engine) ? engine : E_POINTER);
                                return S_OK;
                            }

                            // Logical (DIP) bounds scaled by the window DPI, crisp at any scale.
                            wrl::ComPtr<ICoreWebView2Controller3> c3;
                            if (SUCCEEDED(c2->controller.As(&c3)) && c3) {
                                c3->put_BoundsMode(COREWEBVIEW2_BOUNDS_MODE_USE_RASTERIZATION_SCALE);
                                c3->put_ShouldDetectMonitorScaleChanges(FALSE);
                                c3->put_RasterizationScale(c2->scale);
                            }

                            // Splice the browser's render visual into the Border's XAML visual.
                            auto elemVisual =
                                WUXH::ElementCompositionPreview::GetElementVisual(c2->placeholder);
                            auto compositor = elemVisual.Compositor();
                            c2->rootVisual = compositor.CreateContainerVisual();
                            c2->compositionController->put_RootVisualTarget(
                                reinterpret_cast<::IUnknown *>(winrt::get_abi(c2->rootVisual)));
                            WUXH::ElementCompositionPreview::SetElementChildVisual(c2->placeholder,
                                                                                  c2->rootVisual);

                            // A transparent view: no default background under the page, and no
                            // URL label under the view (a startup failure never reaches here, so
                            // its diagnostic keeps the label).
                            on_engine_ready(handle, c2);
                            return S_OK;
                        })
                        ;
                wrl::ComPtr<ICoreWebView2Environment10> env10;
                HRESULT creating = env->QueryInterface(IID_PPV_ARGS(&env10));
                if (SUCCEEDED(creating)) {
                    wrl::ComPtr<ICoreWebView2ControllerOptions> controllerOptions;
                    creating = env10->CreateCoreWebView2ControllerOptions(&controllerOptions);
                    if (SUCCEEDED(creating)) {
                        controllerOptions->put_ProfileName(cc->profile_name.c_str());
                        controllerOptions->put_IsInPrivateModeEnabled(cc->private_mode);
                        creating = env10->CreateCoreWebView2CompositionControllerWithOptions(cc->parent,controllerOptions.Get(),completion.Get());
                    }
                }
                if (FAILED(creating))
                    startup_failed(handle, "controller request", creating);
                return S_OK;
            })
            .Get());
    if (FAILED(started))
        startup_failed(handle, "environment request", started);
}
#endif

extern "C" {

void *day_webview_xaml_new(const char *url, uint64_t id, void (*cb)(uint64_t, const char *),
                           const char *inline_root, const char *inline_start,
                           const char *inline_dir,
                           void (*link_cb)(uint64_t, const char *), bool transparent,
                           uint64_t session, const char *profile_key, const char *profile_directory, bool private_mode,
                           uint64_t resource_provider, const char *resource_prefix) {
    if (session) {
        auto known = g_sessions.find(session);
        if (known != g_sessions.end()) {
            auto *c = known->second;
            c->id = id;
            c->cb = cb;
            c->link_cb = link_cb;
            // Reported on request (`day_webview_xaml_report_state`), once the node's handler
            // exists: a report from here reaches a node that cannot hear it yet.
            c->last_state.clear();
            // A fresh Day handle owns this mount; the private handle and engine survive it.
            void *mounted = day_xaml_box(winrt::get_abi(c->placeholder));
            g_webviews[mounted] = c;
            if (c->controller) c->controller->put_IsVisible(TRUE);
            return mounted;
        }
    }
    // The boxed element day lays out: a transparent (hit-testable) Border carrying a faint URL label.
    // The browser's render visual is spliced in as the Border's child visual and covers the label;
    // if startup fails, the label displays the diagnostic instead of remaining just a URL.
    const bool inlined = inline_root && *inline_root;
    std::wstring first = hs(url ? url : "").c_str();
    std::wstring prefix;
    if (inlined) {
        // The bundled site browses under the virtual host mapped at controller creation.
        prefix = std::wstring(L"https://") + kDayAssetsHost + L"/" + hs(inline_root).c_str() + L"/";
        first = prefix + hs(inline_start ? inline_start : "index.html").c_str();
    }
    WUXC::Border placeholder;
    placeholder.Background(WUXM::SolidColorBrush(WUI::Color{0, 0, 0, 0})); // transparent but hit-testable
    WUXC::TextBlock label;
    label.Text(winrt::hstring{first});
    label.Margin(WUX::Thickness{8, 8, 8, 8});
    label.Opacity(0.6);
    placeholder.Child(label);
    void *handle = day_xaml_box(winrt::get_abi(placeholder));

    auto *c = new WebViewCtx{};
    c->parent = reinterpret_cast<HWND>(day_xaml_host_hwnd());
    c->placeholder = placeholder;
    c->id = id;
    c->cb = cb;
    c->pending_url = first;
    c->inline_prefix = prefix;
    // A provider's requests are intercepted either way; only its own site (`resource_prefix`
    // set) is policed like an inline one. `WebView::resources` leaves the prefix empty.
    c->resource_provider = resource_provider;
    if (resource_provider && resource_prefix && *resource_prefix)
        c->inline_prefix = hs(resource_prefix).c_str();
    c->inline_dir = inlined && inline_dir ? std::wstring(hs(inline_dir).c_str()) : std::wstring();
    c->link_cb = link_cb;
    c->transparent = transparent;
    c->session = session;
    c->engine_handle = handle;
    UINT dpi = c->parent ? GetDpiForWindow(c->parent) : 96;
    c->scale = (dpi ? dpi : 96) / 96.0;
    g_webviews[handle] = c;
    if (session) g_sessions[session] = c;

#ifndef DAY_WINUI
    // Keep the render visual + Bounds matched to the Border as it resizes. (WinUI's control
    // sizes itself: it stretches to fill the Border.)
    placeholder.SizeChanged([handle](WF::IInspectable const &, WUX::SizeChangedEventArgs const &) {
        if (auto *cc = find_ctx(handle))
            sync_size(cc);
    });
#endif
    c->profile_name = hs(profile_key).c_str();
    c->profile_directory = hs(profile_directory).c_str();
    c->private_mode = private_mode;
    create_webview2(handle);
    void *mounted = day_xaml_box(winrt::get_abi(placeholder));
    g_webviews[mounted] = c;
    return mounted;
}

void day_webview_xaml_tabs(void *handle,const char *foreground,const char *background,void (*callback)(uint64_t,const char *,bool)) {
    if(auto c=find_ctx(handle)) {c->tab_cb=callback;c->tab_foreground=hs(foreground).c_str();c->tab_background=hs(background).c_str();}
}

void day_webview_xaml_release(void *handle) {
    auto *c = find_ctx(handle);
    if (!c) return;
    g_webviews.erase(handle);
    if (c->session) {
        c->id = 0;
        c->cb = nullptr;
        c->link_cb = nullptr;
        if (c->controller) c->controller->put_IsVisible(FALSE);
    } else {
        destroy_ctx(c->engine_handle);
    }
}

void day_webview_xaml_load(void *handle, const char *url) {
    auto *c = find_ctx(handle);
    if (!c)
        return;
    std::wstring w = hs(url).c_str();
    if (c->webview)
        c->webview->Navigate(w.c_str());
    else
        c->pending_url = w;
}
void day_webview_xaml_back(void *handle) {
    auto *c = find_ctx(handle);
    if (c && c->webview) {
        BOOL can = FALSE;
        c->webview->get_CanGoBack(&can);
        if (can)
            c->webview->GoBack();
    }
}
void day_webview_xaml_forward(void *handle) {
    auto *c = find_ctx(handle);
    if (c && c->webview) {
        BOOL can = FALSE;
        c->webview->get_CanGoForward(&can);
        if (can)
            c->webview->GoForward();
    }
}
void day_webview_xaml_set_eval_cb(void (*cb)(uint64_t, uint64_t, const char *)) { g_eval_cb = cb; }
void day_webview_xaml_set_content_cbs(bool (*filter)(uint64_t, const char *, const char *, int, bool, const char *),
                                      void (*message)(uint64_t, const char *, const char *)) {
    g_filter_cb = filter;
    g_message_cb = message;
}
// WebView::request_filter and the page scripts, right after `day_webview_xaml_new`: kept for
// install_content while the engine starts. `scripts` is a JSON array of script sources. A
// retained session's engine keeps its scripts and takes the new node's hooks.
void day_webview_xaml_content(void *handle, uint64_t hooks, const char *scripts) {
    auto *c = find_ctx(handle);
    if (!c)
        return;
    c->hooks = hooks;
    if (c->content_installed || !scripts || !*scripts)
        return;
    try {
        auto array = winrt::Windows::Data::Json::JsonArray::Parse(hs(scripts));
        for (auto const &value : array)
            c->page_scripts.emplace_back(value.GetString().c_str());
    } catch (winrt::hresult_error const &) {
        std::fprintf(stderr, "day-piece-webview: page scripts were not valid JSON\n");
    }
}
// A page message's reply, as a web message to the top-level document.
void day_webview_xaml_post_json(void *handle, const char *json) {
    auto *c = find_ctx(handle);
    if (c && c->webview)
        c->webview->PostWebMessageAsJson(hs(json).c_str());
}
void day_webview_xaml_set_state_cb(void (*cb)(uint64_t, const char *)) { g_state_cb = cb; }

// Evaluate `script` (already wrapped by the Rust front-end) and reply exactly once.
//
// Two paths, and the better one is worth the probe: `ExecuteScriptWithResult` on ICoreWebView2_21
// reports failures at the engine level, so it catches the one case the JS wrapper structurally
// cannot: a syntax error, where the wrapper and the user's script compile as a single unit and
// the wrapper's own `try` never runs. The interface arrived in SDK 1.0.2277.86 (this crate pins
// 1.0.3719.77, so the header is always present) but the runtime is not guaranteed, so a failed
// query falls back to plain `ExecuteScript` + the wrapper's own error reporting.
//
// Handlers run on the creating UI thread, serially and never re-entrantly, so touching
// `g_eval_cb` from them needs no synchronization. They capture PODs only: `Close()` releases
// pending handlers, so a handler must never assume its web view still exists.
void day_webview_xaml_eval(void *handle, uint64_t req, const char *script) {
    auto *c = find_ctx(handle);
    if (!c || !c->webview) {
        // The WebView2 Runtime is absent, or the view is gone. Still a reply: the Rust future is
        // resolved only by one arriving.
        const char *why = c && !c->startup_error.empty() ? c->startup_error.c_str() : "no engine";
        eval_reply(c ? c->id : 0, req, eval_engine_error("Error", why));
        return;
    }
    const uint64_t id = c->id;
    if (std::string(script).rfind("day-web-data:",0)==0) {
        namespace JSON = winrt::Windows::Data::Json;
        try {
            const auto request=JSON::JsonObject::Parse(hs(script+13));
            const auto operation=to_utf8(request.GetNamedString(L"operation"));
            wrl::ComPtr<ICoreWebView2_2> web2;
            wrl::ComPtr<ICoreWebView2CookieManager> manager;
            if(FAILED(c->webview.As(&web2)) || FAILED(web2->get_CookieManager(&manager))) {
                eval_reply(id,req,eval_engine_error("StorageError","cookies unavailable"));return;
            }
            if(operation=="navigation") {
                LPWSTR url=nullptr,title=nullptr;BOOL back=FALSE,forward=FALSE;
                c->webview->get_Source(&url);c->webview->get_DocumentTitle(&title);c->webview->get_CanGoBack(&back);c->webview->get_CanGoForward(&forward);
                JSON::JsonObject state;state.Insert(L"url",JSON::JsonValue::CreateStringValue(url?url:L""));state.Insert(L"title",JSON::JsonValue::CreateStringValue(title?title:L""));
                state.Insert(L"can_go_back",JSON::JsonValue::CreateBooleanValue(back));state.Insert(L"can_go_forward",JSON::JsonValue::CreateBooleanValue(forward));state.Insert(L"loading",JSON::JsonValue::CreateBooleanValue(c->loading));
                CoTaskMemFree(url);CoTaskMemFree(title);eval_reply(id,req,"1\x1f"+to_utf8(state.Stringify()));return;
            }
            if(operation=="snapshot") {
                if(!view_showing(c)){eval_reply(id,req,eval_engine_error("SnapshotError","view not on screen"));return;}
                const int width=static_cast<int>(request.GetNamedNumber(L"width",0));
                wrl::ComPtr<IStream> capture;
                HRESULT hr=CreateStreamOnHGlobal(nullptr,TRUE,&capture);
                if(SUCCEEDED(hr))hr=c->webview->CapturePreview(COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,capture.Get(),
                    wrl::Callback<ICoreWebView2CapturePreviewCompletedHandler>([id,req,capture,width](HRESULT done)->HRESULT {
                        std::string png;
                        if(FAILED(done)||!snapshot_png(capture.Get(),width,png)){eval_reply(id,req,eval_engine_error("SnapshotError","capture failed"));return S_OK;}
                        eval_reply(id,req,"1\x1f\""+base64(png)+"\"");return S_OK;
                    }).Get());
                if(FAILED(hr))eval_reply(id,req,eval_engine_error("SnapshotError","capture refused"));
                return;
            }
            if(operation=="cookies") {
                const auto url=request.GetNamedString(L"url");
                HRESULT hr=manager->GetCookies(url.c_str(),wrl::Callback<ICoreWebView2GetCookiesCompletedHandler>([id,req](HRESULT hr,ICoreWebView2CookieList *cookies)->HRESULT {
                    if(FAILED(hr)||!cookies){eval_reply(id,req,eval_engine_error("StorageError","cookie read failed"));return S_OK;}
                    UINT count=0;cookies->get_Count(&count);std::wstring header;
                    for(UINT i=0;i<count;++i){wrl::ComPtr<ICoreWebView2Cookie> cookie;cookies->GetValueAtIndex(i,&cookie);LPWSTR name=nullptr,value=nullptr;cookie->get_Name(&name);cookie->get_Value(&value);if(i)header+=L"; ";if(name)header+=name;header+=L"=";if(value)header+=value;CoTaskMemFree(name);CoTaskMemFree(value);}
                    const auto json=JSON::JsonValue::CreateStringValue(winrt::hstring{header}).Stringify();eval_reply(id,req,"1\x1f"+to_utf8(json));return S_OK;
                }).Get());
                if(FAILED(hr))eval_reply(id,req,eval_engine_error("StorageError","cookie read failed"));return;
            }
            if(operation=="clear") {
                wrl::ComPtr<ICoreWebView2_13> web13;wrl::ComPtr<ICoreWebView2Profile> profile;wrl::ComPtr<ICoreWebView2Profile2> profile2;
                if(FAILED(c->webview.As(&web13))||FAILED(web13->get_Profile(&profile))||FAILED(profile.As(&profile2))){eval_reply(id,req,eval_engine_error("StorageError","profile clearing unavailable"));return;}
                const auto hr=profile2->ClearBrowsingDataAll(wrl::Callback<ICoreWebView2ClearBrowsingDataCompletedHandler>([id,req](HRESULT hr)->HRESULT {eval_reply(id,req,SUCCEEDED(hr)?"1\x1ftrue":eval_engine_error("StorageError","clear failed"));return S_OK;}).Get());
                if(FAILED(hr))eval_reply(id,req,eval_engine_error("StorageError","clear failed"));return;
            }
            if(operation=="set-cookie") {
                const auto url=winrt::Windows::Foundation::Uri(request.GetNamedString(L"url"));
                auto raw=to_utf8(request.GetNamedString(L"cookie"));std::istringstream parts(raw);std::string part;std::getline(parts,part,';');auto equal=part.find('=');
                if(equal==std::string::npos){eval_reply(id,req,eval_engine_error("StorageError","invalid cookie"));return;}
                const auto sourcePath=to_utf8(url.Path());const auto slash=sourcePath.rfind('/');
                std::string name=part.substr(0,equal),value=part.substr(equal+1),domain=to_utf8(url.Host()),path=slash!=std::string::npos && slash>0?sourcePath.substr(0,slash):"/";bool secure=false,httpOnly=false;double expiry=-1;
                while(std::getline(parts,part,';')) {auto start=part.find_first_not_of(" ");if(start!=std::string::npos)part=part.substr(start);auto equal=part.find('=');auto key=part.substr(0,equal);for(auto &ch:key)ch=static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));auto val=equal==std::string::npos?std::string():part.substr(equal+1);
                    if(key=="domain")domain=val;else if(key=="path")path=val;else if(key=="secure")secure=true;else if(key=="httponly")httpOnly=true;else if(key=="max-age")expiry=(std::max)(0.0,static_cast<double>(std::time(nullptr))+std::stod(val));else if(key=="expires"&&expiry<0){std::tm time{};std::istringstream date(val);date>>std::get_time(&time,"%a, %d %b %Y %H:%M:%S GMT");if(!date.fail())expiry=static_cast<double>(_mkgmtime(&time));}
                }
                wrl::ComPtr<ICoreWebView2Cookie> cookie;HRESULT hr=manager->CreateCookie(hs(name.c_str()).c_str(),hs(value.c_str()).c_str(),hs(domain.c_str()).c_str(),hs(path.c_str()).c_str(),&cookie);
                if(SUCCEEDED(hr)){cookie->put_IsSecure(secure);cookie->put_IsHttpOnly(httpOnly);if(expiry>=0)cookie->put_Expires(expiry);hr=manager->AddOrUpdateCookie(cookie.Get());}
                eval_reply(id,req,SUCCEEDED(hr)?"1\x1ftrue":eval_engine_error("StorageError","cookie rejected"));return;
            }
            eval_reply(id,req,eval_engine_error("StorageError","unknown operation"));
        } catch(...) {eval_reply(id,req,eval_engine_error("StorageError","invalid request"));}
        return;
    }
    const std::wstring js = hs(script).c_str();

    wrl::ComPtr<ICoreWebView2_21> wv21;
    if (SUCCEEDED(c->webview.As(&wv21)) && wv21) {
        HRESULT hr = wv21->ExecuteScriptWithResult(
            js.c_str(),
            wrl::Callback<ICoreWebView2ExecuteScriptWithResultCompletedHandler>(
                [id, req](HRESULT code, ICoreWebView2ExecuteScriptResult *res) -> HRESULT {
                    if (FAILED(code) || !res) {
                        eval_reply(id, req, eval_engine_error("Error", "no result (page discarded)"));
                        return S_OK;
                    }
                    BOOL ok = FALSE;
                    res->get_Succeeded(&ok);
                    if (ok) {
                        // The wrapper always evaluates to a string, so ask for it directly rather
                        // than re-parsing ResultAsJson, the same shape Qt's QVariant and
                        // WebKit's NSString hand back.
                        LPWSTR str = nullptr;
                        BOOL is_string = FALSE;
                        if (SUCCEEDED(res->TryGetResultAsString(&str, &is_string)) && is_string &&
                            str) {
                            const std::string utf8 = day_webview::utf8(str);
                            CoTaskMemFree(str);
                            eval_reply(id, req, utf8);
                            return S_OK;
                        }
                        if (str)
                            CoTaskMemFree(str);
                        eval_reply(id, req,
                                   eval_engine_error("Error", "result was not a string"));
                        return S_OK;
                    }
                    // Engine-level failure, which means a syntax error, since anything the script
                    // threw at run time was caught by the wrapper and returned as a value.
                    // Documented trap: get_Exception returns S_OK even when it yields nothing, so
                    // the out-pointer is what decides, not the HRESULT.
                    wrl::ComPtr<ICoreWebView2ScriptException> ex;
                    res->get_Exception(&ex);
                    if (!ex) {
                        eval_reply(id, req, eval_engine_error("Error", "script failed"));
                        return S_OK;
                    }
                    LPWSTR name = nullptr, message = nullptr;
                    ex->get_Name(&name);
                    ex->get_Message(&message);
                    auto narrow = [](LPWSTR w, const char *fallback) {
                        if (!w)
                            return std::string(fallback);
                        const std::string s = day_webview::utf8(w);
                        return s.empty() ? std::string(fallback) : s;
                    };
                    const std::string n = narrow(name, "SyntaxError");
                    const std::string m = narrow(message, "script failed");
                    if (name)
                        CoTaskMemFree(name);
                    if (message)
                        CoTaskMemFree(message);
                    eval_reply(id, req, eval_engine_error(n.c_str(), m.c_str()));
                    return S_OK;
                })
                .Get());
        if (SUCCEEDED(hr))
            return;
        // The call itself was refused; fall through to the older path rather than stranding it.
    }

    HRESULT hr = c->webview->ExecuteScript(
        js.c_str(),
        wrl::Callback<ICoreWebView2ExecuteScriptCompletedHandler>(
            [id, req](HRESULT code, LPCWSTR json) -> HRESULT {
                if (FAILED(code) || !json) {
                    eval_reply(id, req, eval_engine_error("Error", "no result (page discarded)"));
                    return S_OK;
                }
                // This path returns the result as JSON, so the wrapper's string arrives quoted
                // and escaped, including the `` separators the protocol rides on.
                std::string inner;
                if (!json_string_to_utf8(json, inner)) {
                    eval_reply(id, req, eval_engine_error("Error", "result was not a string"));
                    return S_OK;
                }
                eval_reply(id, req, inner);
                return S_OK;
            })
            .Get());
    if (FAILED(hr))
        eval_reply(id, req, eval_engine_error("Error", "evaluation was refused"));
}

void day_webview_xaml_stop(void *handle) {
    auto *c = find_ctx(handle);
    if (c && c->webview)
        c->webview->Stop();
}
void day_webview_xaml_force_reload(void *handle) {
    if (auto *c = find_ctx(handle); c && c->webview) {
        c->webview->CallDevToolsProtocolMethod(L"Page.reload", L"{\"ignoreCache\":true}",
            wrl::Callback<ICoreWebView2CallDevToolsProtocolMethodCompletedHandler>(
                [](HRESULT, LPCWSTR)->HRESULT { return S_OK; }).Get());
    }
}
void day_webview_xaml_report_state(void *handle) {
    if (auto *c = find_ctx(handle)) {
        c->last_state.clear();
        report_state(c);
    }
}
void day_webview_xaml_reload(void *handle) {
    auto *c = find_ctx(handle);
    if (c && c->webview)
        c->webview->Reload();
}

// The node's WebView::zoom and WebView::javascript, right after `day_webview_xaml_new`: kept for
// on_engine_ready while the engine starts, or applied at once to a retained session's engine.
void day_webview_xaml_settings(void *handle, double zoom, bool javascript) {
    auto *c = find_ctx(handle);
    if (!c)
        return;
    const bool zoom_changed = c->zoom != zoom;
    c->zoom = zoom;
    c->scripts = javascript;
    if (!c->webview)
        return;
    apply_scripts(c);
    if (zoom_changed)
        apply_zoom(c);
}
void day_webview_xaml_set_zoom(void *handle, double zoom) {
    auto *c = find_ctx(handle);
    if (!c)
        return;
    c->zoom = zoom;
    if (c->webview)
        apply_zoom(c);
}
// A running page cannot gain or lose its scripts: change the setting, then reload.
void day_webview_xaml_set_javascript(void *handle, bool enabled) {
    auto *c = find_ctx(handle);
    if (!c || c->scripts == enabled)
        return;
    c->scripts = enabled;
    if (c->webview) {
        apply_scripts(c);
        c->webview->Reload();
    }
}

} // extern "C"
