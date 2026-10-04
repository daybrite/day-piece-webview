// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// The day-piece-webview crate's Android backend, bundled here and folded into the app's Gradle
// build via [package.metadata.day.android], with no edits to day-android. It uses only
// day-android's public Java surface: DayBridge.ctx (the Context) and DayBridge.nativeOnEvent (the
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
        return new android.webkit.WebResourceResponse(mime,"UTF-8",status,"Resource",headers,new java.io.ByteArrayInputStream(body));
    }
    private static native android.webkit.WebResourceResponse resource(long provider, String url, String method, String range);

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

    public static boolean supportsProfiles() {
        return WebViewFeature.isFeatureSupported(WebViewFeature.MULTI_PROFILE);
    }
    public static boolean supportsDataClearing() {
        return WebViewFeature.isFeatureSupported(WebViewFeature.DELETE_BROWSING_DATA);
    }

    public static View makeWebView(long id, String url, String inlinePrefix, long provider, String profile) {
        WebView web = new WebView(DayBridge.ctx);
        if (profile != null && !profile.isEmpty()) {
            if (!WebViewFeature.isFeatureSupported(WebViewFeature.MULTI_PROFILE)) {
                web.destroy(); throw new IllegalStateException("Named browser profiles require a newer Android System WebView");
            }
            WebViewCompat.setProfile(web, profile);
        }
        web.getSettings().setJavaScriptEnabled(true);
        web.getSettings().setDomStorageEnabled(true);
        FullscreenClient chrome = new FullscreenClient(web);
        web.setWebChromeClient(chrome);
        web.addOnAttachStateChangeListener(new View.OnAttachStateChangeListener() {
            public void onViewAttachedToWindow(View view) {}
            public void onViewDetachedFromWindow(View view) { chrome.onHideCustomView(); }
        });
        final boolean inline = inlinePrefix != null && !inlinePrefix.isEmpty();
        web.setWebViewClient(new WebViewClient() {
            @Override
            public android.webkit.WebResourceResponse shouldInterceptRequest(WebView view, android.webkit.WebResourceRequest request) {
                if (provider == 0 || !"day-resource.invalid".equals(request.getUrl().getHost())) return null;
                // WebView calls this on a worker thread; binary bytes never cross JavaScript.
                String range = request.getRequestHeaders().get("Range");
                if (range == null) range = request.getRequestHeaders().get("range");
                return resource(provider, request.getUrl().toString(), request.getMethod(), range == null ? "" : range);
            }
            @Override
            public void onPageFinished(WebView view, String finishedUrl) {
                restoreReloadCacheMode(view);
                // kind 12 = a piece-defined Custom event (§8.2's open channel): the front-end's
                // cx.on reads the text payload as the URL. (No longer hijacking kind 1 = TextChanged.)
                DayBridge.nativeOnEvent(id, 12, 0.0, finishedUrl);
            }

            @Override
            @SuppressWarnings("deprecation") // the String overload runs on every API level
            public boolean shouldOverrideUrlLoading(WebView view, String target) {
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
