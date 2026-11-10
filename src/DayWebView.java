// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// The day-piece-webview crate's Android backend, bundled here and folded into the app's Gradle
// build via [package.metadata.day.android], with no edits to day-toolkit-material. It uses only
// day-toolkit-material's public Java surface: DayBridge.ctx (the Context) and DayBridge.nativeOnEvent (the
// event trampoline). The piece also declares its INTERNET permission in Cargo.toml, which
// `day build` merges into the app manifest, so a WebView-using app needs no manual manifest edit.
// See docs/extending.md.
package dev.daybrite.day.piece.webview;

import android.view.View;
import android.webkit.ValueCallback;
import android.webkit.WebView;
import android.webkit.WebViewClient;

import java.util.Map;
import java.util.WeakHashMap;

import org.json.JSONException;
import org.json.JSONTokener;

import dev.daybrite.day.bridge.DayBridge;

/** Wraps android.webkit.WebView, reporting the finished URL back via the open Custom-event kind (12). */
import androidx.webkit.WebViewCompat;
import androidx.webkit.WebViewFeature;
import androidx.webkit.WebStorageCompat;
import android.webkit.CookieManager;
import android.webkit.WebStorage;
import org.json.JSONObject;

public final class DayWebView {
    private DayWebView() {}
    private static android.webkit.WebResourceResponse response(String mime, int status, String headerLines, byte[] body) {
        Map<String,String> headers=new java.util.HashMap<>();
        for(String line:headerLines.split("\n")){int colon=line.indexOf(':');if(colon>0)headers.put(line.substring(0,colon),line.substring(colon+1).trim());}
        DayWebContentType type = DayWebContentType.parse(mime);
        return new android.webkit.WebResourceResponse(type.mime,type.encoding,status,"Resource",headers,new java.io.ByteArrayInputStream(body));
    }
    private static native android.webkit.WebResourceResponse resource(long provider, String url, String method, String range);
    /**
     * WebView::request_filter: whether to block a request (true). Rust infers the resource kind
     * from {@code accept} and the URL, tracks the page, and reports what it blocks. Called on
     * WebView's IO workers, never the UI thread.
     */
    private static native boolean decide(long hooks, String url, String accept, boolean mainFrame, String method);

    // ---- content blocking and page scripts (docs/webview.md) ----------------------------------

    /** The empty 403 a blocked request receives. */
    private static android.webkit.WebResourceResponse blockedResponse() {
        return new android.webkit.WebResourceResponse("text/plain", "utf-8", 403, "Blocked",
            new java.util.HashMap<String, String>(), new java.io.ByteArrayInputStream(new byte[0]));
    }

    /** Ask the view's request filter about {@code request}; null lets it load. Never throws. */
    private static android.webkit.WebResourceResponse filter(long hooks, android.webkit.WebResourceRequest request) {
        if (hooks == 0) return null;
        try {
            Map<String, String> headers = request.getRequestHeaders();
            String accept = headers == null ? null : headers.get("Accept");
            if (accept == null && headers != null) accept = headers.get("accept");
            return decide(hooks, request.getUrl().toString(), accept == null ? "" : accept,
                request.isForMainFrame(), request.getMethod()) ? blockedResponse() : null;
        } catch (Throwable error) {
            return null; // a failing filter must not fail the page
        }
    }

    /**
     * Service-worker requests have no view: WebView sends them to one process-wide client. They
     * go through the filter of the newest view that has one (in practice every view shares the
     * app's one filter).
     */
    private static volatile long serviceWorkerHooks;
    private static boolean serviceWorkerClientInstalled;
    private static void filterServiceWorkers(long hooks) {
        serviceWorkerHooks = hooks;
        if (serviceWorkerClientInstalled
                || !WebViewFeature.isFeatureSupported(WebViewFeature.SERVICE_WORKER_BASIC_USAGE)
                || !WebViewFeature.isFeatureSupported(WebViewFeature.SERVICE_WORKER_SHOULD_INTERCEPT_REQUEST)) return;
        serviceWorkerClientInstalled = true;
        androidx.webkit.ServiceWorkerControllerCompat.getInstance().setServiceWorkerClient(
            new androidx.webkit.ServiceWorkerClientCompat() {
                @Override
                public android.webkit.WebResourceResponse shouldInterceptRequest(android.webkit.WebResourceRequest request) {
                    return filter(serviceWorkerHooks, request);
                }
            });
    }

    /** A page message awaiting the app's reply: where the reply goes and the shim's request id. */
    private static final class PendingMessage {
        final WebView web; final androidx.webkit.JavaScriptReplyProxy proxy; final long shimId;
        PendingMessage(WebView web, androidx.webkit.JavaScriptReplyProxy proxy, long shimId) { this.web = web; this.proxy = proxy; this.shimId = shimId; }
    }
    private static final Map<Long, PendingMessage> MESSAGES = new java.util.HashMap<>();
    private static long nextMessage = 1;

    /** Report one page message on the Custom channel (num -8) and keep its reply target. */
    private static void reportMessage(WebView web, String name, String data, String origin, boolean mainFrame,
            androidx.webkit.JavaScriptReplyProxy proxy, boolean envelope) {
        Long id = IDS.get(web);
        if (id == null || data == null) return;
        long shimId = -1;
        String payload = data;
        if (envelope) {
            // The fallback bridge's {"dayMessage":name,"id":n,"payload":text}.
            try {
                JSONObject message = new JSONObject(data);
                name = message.getString("dayMessage");
                shimId = message.getLong("id");
                payload = message.optString("payload", "");
            } catch (JSONException e) { return; }
        } else if (data.length() > 0 && data.charAt(0) == '\u0002') {
            // The shim's `\u0002<id>\u0002<text>`; anything else is a raw postMessage.
            int end = data.indexOf('\u0002', 1);
            if (end < 0) return;
            try { shimId = Long.parseLong(data.substring(1, end)); } catch (NumberFormatException e) { return; }
            payload = data.substring(end + 1);
        }
        long messageId = nextMessage++;
        MESSAGES.put(messageId, new PendingMessage(web, proxy, shimId));
        try {
            JSONObject report = new JSONObject();
            report.put("id", messageId); report.put("name", name); report.put("payload", payload);
            report.put("origin", origin == null ? "" : origin); report.put("main_frame", mainFrame);
            DayBridge.nativeOnEvent(id, 12, -8.0, report.toString());
        } catch (JSONException e) { MESSAGES.remove(messageId); }
    }

    /** WebPatch::ScriptReply: settle the page's promise with the handler's reply (or null). */
    public static void scriptReply(View view, long messageId, String reply, boolean hasReply) {
        PendingMessage pending = MESSAGES.remove(messageId);
        if (pending == null) return;
        if (pending.proxy != null) {
            if (pending.shimId >= 0) {
                pending.proxy.postMessage("\u0002" + pending.shimId + "\u0002" + (hasReply ? "1" + reply : "0"));
            } else if (hasReply) {
                pending.proxy.postMessage(reply);
            }
        } else if (pending.shimId >= 0) {
            pending.web.evaluateJavascript("window.__dayMessageReply&&window.__dayMessageReply(" + pending.shimId + ","
                + (hasReply ? JSONObject.quote(reply) : "null") + ")", null);
        }
    }

    /**
     * The fallback page bridge for WebViews without web message listeners: an object every frame
     * of every page in this view can call, whose caller's origin WebView does not reveal. Replies
     * reach the top-level document only.
     */
    private static final class MessageBridge {
        private final WebView web;
        MessageBridge(WebView web) { this.web = web; }
        @android.webkit.JavascriptInterface
        public void post(final String envelope) {
            // Called on a WebView binder thread: report from the UI thread.
            web.post(() -> reportMessage(web, "", envelope, "", false, null, true));
        }
    }

    /**
     * Install WebView::user_script, ::user_style and ::on_script_message before the first load:
     * {@code content} is the Rust front-end's JSON ({@code scripts}: document-start sources for
     * every frame, {@code messages}: handler names, {@code shim} / {@code fallbackShim}: the
     * dayMessage shim for listeners or for the fallback bridge).
     */
    private static final Map<WebView, String[]> PAGE_START_SCRIPTS = new WeakHashMap<>();
    private static void installContent(final WebView web, String content) throws JSONException {
        if (content == null || content.isEmpty()) return;
        JSONObject json = new JSONObject(content);
        org.json.JSONArray names = json.optJSONArray("messages");
        java.util.List<String> scripts = new java.util.ArrayList<>();
        if (names != null && names.length() > 0) {
            if (WebViewFeature.isFeatureSupported(WebViewFeature.WEB_MESSAGE_LISTENER)) {
                for (int i = 0; i < names.length(); i++) {
                    final String name = names.getString(i);
                    WebViewCompat.addWebMessageListener(web, name, java.util.Collections.singleton("*"),
                        (view, message, sourceOrigin, isMainFrame, replyProxy) ->
                            reportMessage(view, name, message.getData(), sourceOrigin == null ? "" : sourceOrigin.toString(),
                                isMainFrame, replyProxy, false));
                }
                scripts.add(json.optString("shim"));
            } else {
                web.addJavascriptInterface(new MessageBridge(web), "__dayMessageHost");
                scripts.add(json.optString("fallbackShim"));
            }
        }
        org.json.JSONArray sources = json.optJSONArray("scripts");
        if (sources != null) for (int i = 0; i < sources.length(); i++) scripts.add(sources.getString(i));
        if (scripts.isEmpty()) return;
        if (WebViewFeature.isFeatureSupported(WebViewFeature.DOCUMENT_START_SCRIPT)) {
            for (String script : scripts) {
                WebViewCompat.addDocumentStartJavaScript(web, script, java.util.Collections.singleton("*"));
            }
        } else {
            // Older WebViews: run them as each main-frame page starts (no iframes, and possibly
            // after the page's first scripts). user_script_support() says Emulated for this.
            PAGE_START_SCRIPTS.put(web, scripts.toArray(new String[0]));
        }
    }
    private static void runPageStartScripts(WebView web) {
        String[] scripts = PAGE_START_SCRIPTS.get(web);
        if (scripts != null) for (String script : scripts) web.evaluateJavascript(script, null);
    }
    /** Whether document-start scripts run natively (user_script_support). */
    public static boolean supportsDocumentStartScripts() {
        return WebViewFeature.isFeatureSupported(WebViewFeature.DOCUMENT_START_SCRIPT);
    }

    // ---- camera and microphone for pages (WebChromeClient.onPermissionRequest) ----------------

    private static final Map<Long, android.webkit.PermissionRequest> MEDIA = new java.util.HashMap<>();
    private static long nextMedia = 1;
    private static final Map<WebView, Boolean> MEDIA_HANDLER = new WeakHashMap<>();

    /** A page asked for capture devices: ask the app's handler, or apply the default. */
    private static void mediaRequested(WebView web, android.webkit.PermissionRequest request) {
        boolean camera = false, microphone = false;
        for (String resource : request.getResources()) {
            if (android.webkit.PermissionRequest.RESOURCE_VIDEO_CAPTURE.equals(resource)) camera = true;
            else if (android.webkit.PermissionRequest.RESOURCE_AUDIO_CAPTURE.equals(resource)) microphone = true;
        }
        // Protected media IDs, MIDI and anything newer are not this piece's to grant.
        if (!camera && !microphone) { request.deny(); return; }
        Long id = IDS.get(web);
        if (id != null && Boolean.TRUE.equals(MEDIA_HANDLER.get(web))) {
            long mediaId = nextMedia++;
            MEDIA.put(mediaId, request);
            try {
                JSONObject report = new JSONObject();
                report.put("id", mediaId); report.put("origin", request.getOrigin().toString());
                report.put("camera", camera); report.put("microphone", microphone);
                DayBridge.nativeOnEvent(id, 12, -10.0, report.toString());
            } catch (JSONException e) { MEDIA.remove(mediaId); request.deny(); }
            return;
        }
        grantWithAppPermissions(request, camera, microphone);
    }

    /** WebPatch::MediaDecision: the app's answer to a reported request. */
    public static void mediaDecision(View view, long mediaId, boolean allow) {
        android.webkit.PermissionRequest request = MEDIA.remove(mediaId);
        if (request == null) return;
        if (!allow) { request.deny(); return; }
        boolean camera = false, microphone = false;
        for (String resource : request.getResources()) {
            if (android.webkit.PermissionRequest.RESOURCE_VIDEO_CAPTURE.equals(resource)) camera = true;
            else if (android.webkit.PermissionRequest.RESOURCE_AUDIO_CAPTURE.equals(resource)) microphone = true;
        }
        grantWithAppPermissions(request, camera, microphone);
    }

    /**
     * Grant what the app itself may use: a permission it holds, or one it declares and the user
     * allows when the system asks now. A permission the app does not declare is never granted.
     */
    private static void grantWithAppPermissions(final android.webkit.PermissionRequest request, final boolean camera, final boolean microphone) {
        java.util.List<String> missing = new java.util.ArrayList<>();
        if (camera && !held(android.Manifest.permission.CAMERA) && declared(android.Manifest.permission.CAMERA)) missing.add(android.Manifest.permission.CAMERA);
        if (microphone && !held(android.Manifest.permission.RECORD_AUDIO) && declared(android.Manifest.permission.RECORD_AUDIO)) missing.add(android.Manifest.permission.RECORD_AUDIO);
        Runnable answer = () -> {
            java.util.List<String> grant = new java.util.ArrayList<>();
            if (camera && held(android.Manifest.permission.CAMERA)) grant.add(android.webkit.PermissionRequest.RESOURCE_VIDEO_CAPTURE);
            if (microphone && held(android.Manifest.permission.RECORD_AUDIO)) grant.add(android.webkit.PermissionRequest.RESOURCE_AUDIO_CAPTURE);
            if (grant.isEmpty()) request.deny(); else request.grant(grant.toArray(new String[0]));
        };
        if (missing.isEmpty() || !(DayBridge.ctx instanceof androidx.fragment.app.FragmentActivity)) { answer.run(); return; }
        try {
            androidx.fragment.app.FragmentManager fm = ((androidx.fragment.app.FragmentActivity) DayBridge.ctx).getSupportFragmentManager();
            MediaPermissions fragment = (MediaPermissions) fm.findFragmentByTag(MediaPermissions.TAG);
            if (fragment == null) {
                fragment = new MediaPermissions();
                // commitNow runs onCreate, so the launcher is registered below.
                fm.beginTransaction().add(fragment, MediaPermissions.TAG).commitNow();
            }
            fragment.ask(missing.toArray(new String[0]), answer);
        } catch (Exception e) {
            answer.run();
        }
    }
    private static boolean held(String permission) {
        return DayBridge.ctx.checkSelfPermission(permission) == android.content.pm.PackageManager.PERMISSION_GRANTED;
    }
    private static boolean declared(String permission) {
        try {
            android.content.pm.PackageInfo info = DayBridge.ctx.getPackageManager().getPackageInfo(
                DayBridge.ctx.getPackageName(), android.content.pm.PackageManager.GET_PERMISSIONS);
            if (info.requestedPermissions != null) for (String p : info.requestedPermissions) if (permission.equals(p)) return true;
        } catch (Exception ignored) {}
        return false;
    }

    /** A headless fragment owning the runtime permission prompt's result (no Activity override needed). */
    public static final class MediaPermissions extends androidx.fragment.app.Fragment {
        static final String TAG = "dev.daybrite.day.piece.webview.media";
        private static final java.util.ArrayDeque<Runnable> WAITING = new java.util.ArrayDeque<>();
        private androidx.activity.result.ActivityResultLauncher<String[]> launcher;
        @Override public void onCreate(android.os.Bundle saved) {
            super.onCreate(saved);
            launcher = registerForActivityResult(new androidx.activity.result.contract.ActivityResultContracts.RequestMultiplePermissions(),
                result -> { Runnable next = WAITING.poll(); if (next != null) next.run(); });
        }
        void ask(String[] permissions, Runnable then) {
            if (launcher == null) { then.run(); return; }
            WAITING.add(then);
            try { launcher.launch(permissions); } catch (Exception e) { WAITING.remove(then); then.run(); }
        }
    }

    // The day node each live view reports to: evaluation replies need it, and `evalJs` receives
    // only the View (weak keys: a released view must not pin itself here).
    private static final Map<View, Long> IDS = new WeakHashMap<>();

    /** An engine-level evaluation failure, in the 0x1F envelope the Rust front-end decodes. */
    private static String evalError(String message) {
        return "0\u001FAndroidWebView\u001F" + message;
    }

    /**
     * API 30 flipped {@link android.webkit.WebSettings#setAllowFileAccess} to {@code false},
     * refusing even the app's {@code loadUrl("file://...")} with net::ERR_ACCESS_DENIED,
     * which broke every day app that renders a locally written document (a feed reader's
     * article file). Re-enable it only when the app itself asks for a {@code file://} URL, so
     * a WebView that never leaves http(s) keeps the modern lockdown. The dangerous switches
     * stay at their defaults regardless: scripts inside a file page still cannot read other
     * {@code file://} content (setAllowFileAccessFromFileURLs) or reach other origins from the
     * file scheme (setAllowUniversalAccessFromFileURLs), and web content cannot navigate a
     * WebView to {@code file://} at all; only the app's {@code loadUrl} can.
     */
    private static void allowFileUrl(WebView web, String url) {
        if (url != null && url.startsWith("file://")) {
            web.getSettings().setAllowFileAccess(true);
        }
    }

    /**
     * Zoom the page (WebView::zoom) through text zoom, {@code 100} being 100%: what Chrome's
     * "Text scaling" sets. It is the one WebView zoom that persists across navigations and
     * reflows the page, so a column of text stays readable at any size. {@code zoomBy} and
     * {@code setInitialScale} only magnify, and each new page starts over.
     */
    public static void setZoom(View view, double zoom) {
        if (view instanceof WebView) {
            ((WebView) view).getSettings().setTextZoom((int) Math.round(zoom * 100));
        }
    }

    /**
     * A desktop Chrome user agent at the engine's major version: Chrome's "Desktop site"
     * string. The Rust front-end's {@code desktop_user_agent} composes the same text.
     */
    static String desktopAgent(String mobile) {
        java.util.regex.Matcher chrome = java.util.regex.Pattern.compile("Chrome/(\\d+)").matcher(mobile == null ? "" : mobile);
        String major = chrome.find() ? chrome.group(1) : "120";
        return "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/" + major + ".0.0.0 Safari/537.36";
    }

    /** Apply WebView::desktop_mode; true when it changed what the view asks sites for. */
    private static boolean applyDesktop(WebView web, boolean desktop) {
        android.webkit.WebSettings settings = web.getSettings();
        boolean changed = settings.getUseWideViewPort() != desktop;
        // The desktop user agent, and a desktop-width layout viewport for pages that declare no
        // viewport of their own, shown zoomed out to fit, which is what Chrome's desktop mode
        // does. A null agent restores the engine's own.
        settings.setUserAgentString(desktop ? desktopAgent(android.webkit.WebSettings.getDefaultUserAgent(web.getContext())) : null);
        settings.setUseWideViewPort(desktop);
        settings.setLoadWithOverviewMode(desktop);
        // A page laid out wider than the screen has to be pinched to be read.
        settings.setBuiltInZoomControls(desktop);
        settings.setDisplayZoomControls(false);
        return changed;
    }

    /** WebView::desktop_mode changed: apply it and reload, so the site answers the new agent. */
    public static void setDesktopMode(View view, boolean desktop) {
        if (view instanceof WebView && applyDesktop((WebView) view, desktop)) {
            ((WebView) view).reload();
        }
    }

    /**
     * WebView::javascript changed: apply it and reload, since a page cannot gain or lose its
     * scripts while it runs. Android has no separate world for the app's own scripts, so with
     * page scripts off {@code evaluateJavascript} runs nothing either; {@link #evalJs} says so.
     */
    public static void setJavaScript(View view, boolean enabled) {
        if (!(view instanceof WebView)) return;
        WebView web = (WebView) view;
        if (web.getSettings().getJavaScriptEnabled() != enabled) {
            web.getSettings().setJavaScriptEnabled(enabled);
            web.reload();
        }
    }

    /** Draw no background of the view's own, so a transparent page shows the app behind it. */
    public static void setTransparent(View view) {
        if (view instanceof WebView) {
            view.setBackgroundColor(android.graphics.Color.TRANSPARENT);
        }
    }

    /** HTML fullscreen uses a temporary Activity overlay; restore flags on every exit. */
    private static final class FullscreenClient extends android.webkit.WebChromeClient {
        private final WebView web;
        private android.view.ViewGroup decor;
        private View custom;
        private CustomViewCallback callback;
        private int previousFlags;
        FullscreenClient(WebView web) { this.web = web; }
        @Override public void onReceivedTitle(WebView view, String title) { reportState(view); }
        @Override public void onProgressChanged(WebView view, int progress) { reportState(view); }
        // getUserMedia: camera and microphone follow the app's own permissions (docs/webview.md).
        @Override public void onPermissionRequest(android.webkit.PermissionRequest request) { mediaRequested(web, request); }
        @Override public void onPermissionRequestCanceled(android.webkit.PermissionRequest request) { MEDIA.values().remove(request); }
        @Override public boolean onCreateWindow(WebView source, boolean dialog, boolean userGesture, android.os.Message result) {
            if (!userGesture || !TAB_LABELS.containsKey(source)) return false;
            WebView pending=new WebView(source.getContext());
            final boolean[] delivered={false};
            pending.setWebViewClient(new WebViewClient() {
                @Override @SuppressWarnings("deprecation") public boolean shouldOverrideUrlLoading(WebView view,String url) {
                    if(!delivered[0]) { delivered[0]=true; newTab(source,url,false); pending.post(pending::destroy); }
                    return true;
                }
            });
            ((WebView.WebViewTransport)result.obj).setWebView(pending); result.sendToTarget();
            source.postDelayed(() -> {if(!delivered[0]) {delivered[0]=true;pending.destroy();}},10000);
            return true;
        }
        @Override @SuppressWarnings("deprecation")
        public void onShowCustomView(View view, CustomViewCallback done) {
            if (custom != null) { done.onCustomViewHidden(); return; }
            android.content.Context context = web.getContext();
            while (context instanceof android.content.ContextWrapper && !(context instanceof android.app.Activity)) {
                context = ((android.content.ContextWrapper) context).getBaseContext();
            }
            if (!(context instanceof android.app.Activity)) { done.onCustomViewHidden(); return; }
            decor = (android.view.ViewGroup) ((android.app.Activity) context).getWindow().getDecorView();
            previousFlags = decor.getSystemUiVisibility();
            custom = view;
            callback = done;
            decor.addView(view, new android.view.ViewGroup.LayoutParams(-1, -1));
            decor.setSystemUiVisibility(previousFlags | View.SYSTEM_UI_FLAG_FULLSCREEN
                | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION | View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY);
            view.setFocusableInTouchMode(true);
            view.requestFocus();
            view.setOnKeyListener((v, key, event) -> {
                if ((key == android.view.KeyEvent.KEYCODE_BACK || key == android.view.KeyEvent.KEYCODE_ESCAPE)
                        && event.getAction() == android.view.KeyEvent.ACTION_UP) {
                    onHideCustomView(); return true;
                }
                return false;
            });
        }
        @Override @SuppressWarnings("deprecation")
        public void onHideCustomView() {
            if (custom == null) return;
            decor.removeView(custom);
            decor.setSystemUiVisibility(previousFlags);
            custom = null;
            CustomViewCallback done = callback;
            callback = null;
            done.onCustomViewHidden();
            web.requestFocus();
        }
    }

    // Live page state (WebView::page_state). Android exposes no loading flag, so the client's
    // onPageStarted/onPageFinished pair keeps one; the last report per view suppresses repeats.
    private static final Map<WebView, Boolean> LOADING = new WeakHashMap<>();
    private static final Map<WebView, String> LAST_STATE = new WeakHashMap<>();
    /** Report the view's current state on the Custom channel (num -5), unless unchanged. */
    private static void reportState(WebView web) {
        Long id = IDS.get(web);
        if (id == null) return;
        boolean loading = Boolean.TRUE.equals(LOADING.get(web));
        String url = web.getUrl(), title = web.getTitle();
        String text = (loading ? "1" : "0") + (web.canGoBack() ? "1" : "0") + (web.canGoForward() ? "1" : "0")
            + "\u001F" + (loading ? String.format(java.util.Locale.ROOT, "%.2f", web.getProgress() / 100.0) : "1")
            + "\u001F" + (url == null ? "" : url) + "\u001F" + (title == null ? "" : title);
        if (text.equals(LAST_STATE.get(web))) return;
        LAST_STATE.put(web, text);
        DayBridge.nativeOnEvent(id, 12, -5.0, text);
    }
    /**
     * Report where the page is scrolled (WebView::scroll_state) on the Custom channel (num -6), as
     * {@code offset␟content height␟viewport height␟dragging}, in density-independent points: on
     * every scroll change, and when a finger lifts so the binding learns the drag ended.
     */
    public static void reportScroll(View view) {
        final WebView web = (WebView) view;
        final boolean[] dragging = {false};
        web.setOnTouchListener((v, event) -> {
            int action = event.getActionMasked();
            if (action == android.view.MotionEvent.ACTION_DOWN || action == android.view.MotionEvent.ACTION_MOVE) {
                dragging[0] = true;
            } else if (action == android.view.MotionEvent.ACTION_UP || action == android.view.MotionEvent.ACTION_CANCEL) {
                dragging[0] = false;
                sendScroll(web, false);
            }
            return false; // the page still handles the touch
        });
        web.setOnScrollChangeListener((v, x, y, oldX, oldY) -> sendScroll(web, dragging[0]));
    }
    @SuppressWarnings("deprecation") // getScale: the one way to turn CSS pixels into view pixels
    private static void sendScroll(WebView web, boolean dragging) {
        Long id = IDS.get(web);
        if (id == null) return;
        float density = web.getResources().getDisplayMetrics().density;
        double offset = web.getScrollY() / density;
        double content = web.getContentHeight() * web.getScale() / density;
        double viewport = web.getHeight() / density;
        String text = String.format(java.util.Locale.ROOT, "%.1f\u001F%.1f\u001F%.1f\u001F%s",
            offset, content, viewport, dragging ? "1" : "0");
        DayBridge.nativeOnEvent(id, 12, -6.0, text);
    }
    /** Encode what the view shows as PNG at most {@code width} pixels wide, base64, no wrapping. */
    private static String snapshot(WebView web, int width) {
        int w = web.getWidth(), h = web.getHeight();
        if (w <= 0 || h <= 0) throw new IllegalStateException("the view has no size");
        float scale = width > 0 && w > width ? (float) width / w : 1f;
        android.graphics.Bitmap bitmap = android.graphics.Bitmap.createBitmap(
            Math.max(1, Math.round(w * scale)), Math.max(1, Math.round(h * scale)), android.graphics.Bitmap.Config.ARGB_8888);
        android.graphics.Canvas canvas = new android.graphics.Canvas(bitmap);
        canvas.scale(scale, scale);
        // The visible part of the page: draw() renders from the current scroll position.
        canvas.translate(-web.getScrollX(), -web.getScrollY());
        web.draw(canvas);
        java.io.ByteArrayOutputStream png = new java.io.ByteArrayOutputStream();
        bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, png);
        bitmap.recycle();
        return android.util.Base64.encodeToString(png.toByteArray(), android.util.Base64.NO_WRAP);
    }

    private static final java.util.WeakHashMap<WebView,String[]> TAB_LABELS=new java.util.WeakHashMap<>();
    private static void newTab(WebView view,String url,boolean background) {
        Long id=IDS.get(view); if(id==null || url==null || url.isEmpty()) return;
        try { org.json.JSONObject request=new org.json.JSONObject(); request.put("url",url); request.put("background",background); DayBridge.nativeOnEvent(id,12,-4.0,request.toString()); }
        catch(org.json.JSONException ignored) {}
    }
    private static final String TAB_MODIFIERS="(() => {if(window.__dayTabModifiers)return;window.__dayTabModifiers=true;const click=e=>{const a=e.composedPath().find(n=>n?.tagName==='A'&&n.href);if(!a||!(e.metaKey||e.ctrlKey||e.button===1))return;e.preventDefault();e.stopImmediatePropagation();location.href='day-tab://open?url='+encodeURIComponent(a.href)+'&background='+(e.shiftKey?'0':'1');};addEventListener('click',click,true);addEventListener('auxclick',click,true);})();";
    public static void configureTabs(View view,String foreground,String background) {
        if(!(view instanceof WebView)) return;
        WebView web=(WebView)view;
        TAB_LABELS.put(web,new String[]{foreground,background});
        web.getSettings().setSupportMultipleWindows(true);
        if(WebViewFeature.isFeatureSupported(WebViewFeature.DOCUMENT_START_SCRIPT)) {
            WebViewCompat.addDocumentStartJavaScript(web,TAB_MODIFIERS,java.util.Collections.singleton("*"));
        }
        web.evaluateJavascript(TAB_MODIFIERS,null);

        web.setOnLongClickListener(v -> {
            WebView.HitTestResult hit=web.getHitTestResult();
            if(hit==null || (hit.getType()!=WebView.HitTestResult.SRC_ANCHOR_TYPE && hit.getType()!=WebView.HitTestResult.SRC_IMAGE_ANCHOR_TYPE)) return false;
            String url=hit.getExtra(); if(url==null) return false;
            android.widget.PopupMenu menu=new android.widget.PopupMenu(web.getContext(),web);
            menu.getMenu().add(foreground).setOnMenuItemClickListener(item -> {newTab(web,url,false);return true;});
            menu.getMenu().add(background).setOnMenuItemClickListener(item -> {newTab(web,url,true);return true;});
            menu.show();return true;
        });
    }

    public static void release(View view) {
        if(!(view instanceof WebView))return;
        WebView web=(WebView)view;
        IDS.remove(web);TAB_LABELS.remove(web);LOADING.remove(web);LAST_STATE.remove(web);
        MEDIA_HANDLER.remove(web);PAGE_START_SCRIPTS.remove(web);
        MESSAGES.values().removeIf(pending -> pending.web == web);
        web.stopLoading();web.setWebChromeClient(null);web.setWebViewClient(null);
        if(web.getParent() instanceof android.view.ViewGroup)((android.view.ViewGroup)web.getParent()).removeView(web);
        web.destroy();
    }

    public static boolean supportsProfiles() {
        return WebViewFeature.isFeatureSupported(WebViewFeature.MULTI_PROFILE);
    }
    public static boolean supportsDataClearing() {
        return WebViewFeature.isFeatureSupported(WebViewFeature.DELETE_BROWSING_DATA);
    }

    public static View makeWebView(long id, String url, String inlinePrefix, long provider, String profile,
            double zoom, boolean desktop, boolean javascript, long hooks, String content, boolean mediaHandler) {
        WebView web = new WebView(DayBridge.ctx);
        if (profile != null && !profile.isEmpty()) {
            if (!WebViewFeature.isFeatureSupported(WebViewFeature.MULTI_PROFILE)) {
                web.destroy(); throw new IllegalStateException("Named browser profiles require a newer Android System WebView");
            }
            WebViewCompat.setProfile(web, profile);
        }
        // WebView::javascript, WebView::zoom and WebView::desktop_mode, before the first load.
        web.getSettings().setJavaScriptEnabled(javascript);
        web.getSettings().setDomStorageEnabled(true);
        if (zoom != 1.0) setZoom(web, zoom);
        if (desktop) applyDesktop(web, true);
        FullscreenClient chrome = new FullscreenClient(web);
        web.setWebChromeClient(chrome);
        if (mediaHandler) MEDIA_HANDLER.put(web, true);
        if (hooks != 0) filterServiceWorkers(hooks);
        // Scripts and message listeners apply to pages that start loading after this.
        try { installContent(web, content); }
        catch (JSONException e) { android.util.Log.w("day-piece-webview", "page scripts: " + e); }
        web.addOnAttachStateChangeListener(new View.OnAttachStateChangeListener() {
            public void onViewAttachedToWindow(View view) {}
            public void onViewDetachedFromWindow(View view) { chrome.onHideCustomView(); }
        });
        final boolean inline = inlinePrefix != null && !inlinePrefix.isEmpty();
        web.setWebViewClient(new WebViewClient() {
            @Override
            public android.webkit.WebResourceResponse shouldInterceptRequest(WebView view, android.webkit.WebResourceRequest request) {
                if (provider == 0 || !"day-resource.invalid".equals(request.getUrl().getHost())) return filter(hooks, request);
                // The provider's namespace: the filter decides first, and exempts it itself where
                // the provider's pages are the app's own site (RequestHooks::filters_internal).
                android.webkit.WebResourceResponse blocked = filter(hooks, request);
                if (blocked != null) return blocked;
                // WebView calls this on a worker thread; binary bytes never cross JavaScript.
                String range = request.getRequestHeaders().get("Range");
                if (range == null) range = request.getRequestHeaders().get("range");
                return resource(provider, request.getUrl().toString(), request.getMethod(), range == null ? "" : range);
            }
            @Override
            public void onPageStarted(WebView view, String startedUrl, android.graphics.Bitmap favicon) {
                LOADING.put(view, true);
                reportState(view);
                runPageStartScripts(view);
            }
            @Override
            public void doUpdateVisitedHistory(WebView view, String visitedUrl, boolean reload) {
                // Also the one callback for same-document changes (pushState, fragments).
                reportState(view);
            }
            @Override
            public void onPageFinished(WebView view, String finishedUrl) {
                LOADING.put(view, false);
                reportState(view);
                restoreReloadCacheMode(view);
                if(TAB_LABELS.containsKey(view)) view.evaluateJavascript(TAB_MODIFIERS,null);
                // kind 12 = a piece-defined Custom event (§8.2's open channel): the front-end's
                // cx.on reads the text payload as the URL. (No longer hijacking kind 1 = TextChanged.)
                DayBridge.nativeOnEvent(id, 12, 0.0, finishedUrl);
            }

            @Override
            @SuppressWarnings("deprecation") // the String overload runs on every API level
            public boolean shouldOverrideUrlLoading(WebView view, String target) {
                if(TAB_LABELS.containsKey(view) && target.startsWith("day-tab://")) {
                    android.net.Uri request=android.net.Uri.parse(target);
                    newTab(view,request.getQueryParameter("url"),"1".equals(request.getQueryParameter("background")));return true;
                }
                if (!inline || target.startsWith(inlinePrefix)) {
                    return false; // in-site (or remote mode): let the WebView navigate
                }
                // Inline mode leaving the site: cancel and report (num -1 = the link report);
                // the Rust side runs the app's LinkPolicy, system browser by default.
                DayBridge.nativeOnEvent(id, 12, -1.0, target);
                return true;
            }
        });
        if (url != null && !url.isEmpty()) {
            allowFileUrl(web, url);
            web.loadUrl(url);
        }
        IDS.put(web, id);
        return web;
    }

    /**
     * Evaluate the (already-wrapped) script and reply on the kind-12 channel keyed by {@code req}
     * (docs/webview-eval.md). The wrapper makes the result a JS string, which
     * {@code evaluateJavascript} hands back JSON-serialized, with one outer quoted layer to strip.
     * Android has no error channel: a throw and {@code undefined} both arrive as the literal
     * {@code "null"}, and a failed JSON write as the empty string; both map to engine errors
     * (the wrapper already catches script-level throws before they get that far). Delivery is
     * at most once (a destroyed WebView drops the callback), which is why the console's
     * awaiting future must never be the only owner of critical work.
     */
    public static void evalJs(View view, double req, String script) {
        if (!(view instanceof WebView)) {
            return;
        }
        Long id = IDS.get(view);
        if (id == null) {
            return;
        }
        final long nodeId = id;
        if (script.startsWith("day-web-data:")) {
            WebView web = (WebView)view;
            try {
                JSONObject request = new JSONObject(script.substring(13));
                CookieManager cookies = WebViewFeature.isFeatureSupported(WebViewFeature.MULTI_PROFILE)
                    ? WebViewCompat.getProfile(web).getCookieManager() : CookieManager.getInstance();
                WebStorage storage = WebViewFeature.isFeatureSupported(WebViewFeature.MULTI_PROFILE)
                    ? WebViewCompat.getProfile(web).getWebStorage() : WebStorage.getInstance();
                String operation = request.optString("operation");
                if (operation.equals("navigation")) {
                    JSONObject state = new JSONObject();
                    state.put("url", web.getUrl() == null ? "" : web.getUrl());
                    state.put("title", web.getTitle() == null ? "" : web.getTitle());
                    state.put("can_go_back", web.canGoBack()); state.put("can_go_forward", web.canGoForward());
                    state.put("loading", web.getProgress() < 100);
                    DayBridge.nativeOnEvent(nodeId,12,req,"1\u001F"+state.toString());
                } else if (operation.equals("snapshot")) {
                    DayBridge.nativeOnEvent(nodeId,12,req,"1\u001F"+JSONObject.quote(snapshot(web, request.optInt("width"))));
                } else if (operation.equals("cookies")) {
                    String header = cookies.getCookie(request.optString("url"));
                    DayBridge.nativeOnEvent(nodeId,12,req,"1\u001F"+JSONObject.quote(header == null ? "" : header));
                } else if (operation.equals("set-cookie")) {
                    cookies.setCookie(request.optString("url"),request.optString("cookie"), ok -> {
                        cookies.flush();
                        DayBridge.nativeOnEvent(nodeId,12,req,ok ? "1\u001Ftrue" : evalError("cookie rejected"));
                    });
                } else if (operation.equals("clear")) {
                    if (!WebViewFeature.isFeatureSupported(WebViewFeature.DELETE_BROWSING_DATA)) {
                        DayBridge.nativeOnEvent(nodeId,12,req,evalError("Complete data clearing requires a newer Android System WebView"));
                    } else {
                        WebStorageCompat.deleteBrowsingData(storage, () -> DayBridge.nativeOnEvent(nodeId,12,req,"1\u001Ftrue"));
                    }
                } else { DayBridge.nativeOnEvent(nodeId,12,req,evalError("unknown storage operation")); }
            } catch(Exception error) { DayBridge.nativeOnEvent(nodeId,12,req,evalError(error.getClass().getSimpleName())); }
            return;
        }
        if (!((WebView) view).getSettings().getJavaScriptEnabled()) {
            // WebView::javascript(false): evaluateJavascript would run nothing and answer null.
            DayBridge.nativeOnEvent(nodeId, 12, req, "0\u001FScriptsDisabled\u001Fpage scripts are off in this view");
            return;
        }
        ((WebView) view).evaluateJavascript(script, new ValueCallback<String>() {
            @Override
            public void onReceiveValue(String value) {
                String payload;
                if (value == null || value.isEmpty()) {
                    payload = evalError("empty result (JSON write failed)");
                } else if (value.equals("null")) {
                    payload = evalError("no result");
                } else {
                    try {
                        Object v = new JSONTokener(value).nextValue();
                        payload = v instanceof String ? (String) v
                                : evalError("non-string result");
                    } catch (JSONException e) {
                        payload = evalError("unparsable result");
                    }
                }
                DayBridge.nativeOnEvent(nodeId, 12, req, payload);
            }
        });
    }

    /** Imperative commands: 0=load, 1=back, 2=forward, 3=stop, 4=reload. */
    private static final java.util.WeakHashMap<WebView, Integer> reloadCacheModes = new java.util.WeakHashMap<>();
    private static void restoreReloadCacheMode(WebView web) {
        Integer mode = reloadCacheModes.remove(web);
        if (mode != null) web.getSettings().setCacheMode(mode);
    }
    public static void webCommand(View view, int code, String url) {
        if (!(view instanceof WebView)) {
            return;
        }
        WebView web = (WebView) view;
        switch (code) {
            case 0:
                if (url != null && !url.isEmpty()) {
                    allowFileUrl(web, url);
                    web.loadUrl(url);
                }
                break;
            case 1:
                if (web.canGoBack()) {
                    web.goBack();
                }
                break;
            case 2:
                if (web.canGoForward()) {
                    web.goForward();
                }
                break;
            case 3:
                restoreReloadCacheMode(web);
                web.stopLoading();
                break;
            case 4:
                web.reload();
                break;
            case 5:
                if (!reloadCacheModes.containsKey(web)) reloadCacheModes.put(web, web.getSettings().getCacheMode());
                web.getSettings().setCacheMode(android.webkit.WebSettings.LOAD_NO_CACHE);
                web.reload();
                break;
            default:
                break;
        }
    }
}
