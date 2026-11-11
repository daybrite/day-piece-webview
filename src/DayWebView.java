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


    // ---- browser duties (docs/webview.md): dialogs, sign-in, files, downloads, failures -------

    /** A question waiting for the app: the engine's callback, kept until promptAnswer. */
    private static final Map<Long, Object> PROMPTS = new java.util.HashMap<>();
    private static final Map<Long, WebView> PROMPT_OWNERS = new java.util.HashMap<>();
    private static long nextPrompt = 1;
    /** Views whose renderer is gone: WebView forbids every call on them but destroy(). */
    private static final java.util.Set<WebView> DEAD = java.util.Collections.newSetFromMap(new WeakHashMap<WebView, Boolean>());
    /** Hosts the user chose to visit despite their certificate (trust_host), until the app exits. */
    private static final java.util.Set<String> TRUSTED = java.util.Collections.synchronizedSet(new java.util.HashSet<String>());
    /** The address each view's main frame last asked for, written on WebView's IO thread. */
    private static final Map<WebView, String> MAIN_URL = java.util.Collections.synchronizedMap(new WeakHashMap<WebView, String>());
    /** Views that browse the web (not a bundled site): they pinch to zoom. */
    private static final java.util.Set<WebView> BROWSING = java.util.Collections.newSetFromMap(new WeakHashMap<WebView, Boolean>());
    private static final android.os.Handler MAIN = new android.os.Handler(android.os.Looper.getMainLooper());

    /** A prompt() waiting for its answer, with the text OK returns when the app sends none. */
    private static final class PromptPending {
        final android.webkit.JsPromptResult result; final String fallback;
        PromptPending(android.webkit.JsPromptResult result, String fallback) { this.result = result; this.fallback = fallback; }
    }
    /** An input type=file waiting for its files. */
    private static final class FileChoice {
        final ValueCallback<android.net.Uri[]> callback;
        FileChoice(ValueCallback<android.net.Uri[]> callback) { this.callback = callback; }
    }

    /** Report a question on the Custom channel (num -11) and keep its callback; 0 when the view has no node. */
    private static long ask(WebView web, Object pending, JSONObject report) {
        Long node = IDS.get(web);
        if (node == null) return 0;
        long id = nextPrompt++;
        try { report.put("id", id); } catch (JSONException e) { return 0; }
        PROMPTS.put(id, pending);
        PROMPT_OWNERS.put(id, web);
        DayBridge.nativeOnEvent(node, 12, -11.0, report.toString());
        return id;
    }
    private static JSONObject report(String kind) {
        JSONObject report = new JSONObject();
        try { report.put("kind", kind); } catch (JSONException ignored) {}
        return report;
    }
    /** Report a one-way notice on the Custom channel (num -12). */
    private static void notice(WebView web, JSONObject notice) {
        Long node = IDS.get(web);
        if (node != null) DayBridge.nativeOnEvent(node, 12, -12.0, notice.toString());
    }
    private static boolean dialog(WebView web, String kind, String url, String message, String fallback, Object pending) {
        JSONObject report = report("dialog");
        try {
            report.put("dialog", kind); report.put("message", message == null ? "" : message);
            report.put("default_text", fallback == null ? "" : fallback); report.put("url", url == null ? "" : url);
        } catch (JSONException e) { return false; }
        return ask(web, pending, report) != 0;
    }

    /** Give a question its default answer: what the engine does when nobody answers. */
    private static void cancel(Object pending) {
        if (pending instanceof PromptPending) ((PromptPending) pending).result.cancel();
        else if (pending instanceof android.webkit.JsResult) ((android.webkit.JsResult) pending).cancel();
        else if (pending instanceof android.webkit.HttpAuthHandler) ((android.webkit.HttpAuthHandler) pending).cancel();
        else if (pending instanceof FileChoice) ((FileChoice) pending).callback.onReceiveValue(null);
        else if (pending instanceof Download) ACTIVE.remove(((Download) pending).id);
    }
    /** A view is going away: nothing will answer what it still asks. */
    private static void cancelPrompts(WebView web) {
        java.util.List<Long> mine = new java.util.ArrayList<>();
        for (Map.Entry<Long, WebView> entry : PROMPT_OWNERS.entrySet()) if (entry.getValue() == web) mine.add(entry.getKey());
        for (Long id : mine) {
            PROMPT_OWNERS.remove(id);
            Object pending = PROMPTS.remove(id);
            // A download the app has not placed yet is dropped; one already running goes on.
            if (pending != null) cancel(pending);
        }
    }

    /** WebPatch::PromptAnswer: the app's answer to a reported question (browsing::Answer). */
    public static void promptAnswer(View view, long id, String answerJson) {
        Object pending = PROMPTS.remove(id);
        PROMPT_OWNERS.remove(id);
        if (pending == null) return;
        JSONObject answer;
        try { answer = new JSONObject(answerJson); } catch (JSONException e) { cancel(pending); return; }
        if (!answer.optBoolean("ok")) { cancel(pending); return; }
        if (pending instanceof PromptPending) {
            PromptPending prompt = (PromptPending) pending;
            prompt.result.confirm(answer.isNull("text") ? prompt.fallback : answer.optString("text"));
        } else if (pending instanceof android.webkit.JsResult) {
            ((android.webkit.JsResult) pending).confirm();
        } else if (pending instanceof android.webkit.HttpAuthHandler) {
            ((android.webkit.HttpAuthHandler) pending).proceed(answer.optString("user"), answer.optString("password"));
        } else if (pending instanceof FileChoice) {
            org.json.JSONArray paths = answer.optJSONArray("paths");
            java.util.List<android.net.Uri> files = new java.util.ArrayList<>();
            for (int i = 0; paths != null && i < paths.length(); i++) files.add(android.net.Uri.fromFile(new java.io.File(paths.optString(i))));
            ((FileChoice) pending).callback.onReceiveValue(files.isEmpty() ? null : files.toArray(new android.net.Uri[0]));
        } else if (pending instanceof Download) {
            org.json.JSONArray paths = answer.optJSONArray("paths");
            Download download = (Download) pending;
            String path = paths == null || paths.length() == 0 ? null : paths.optString(0);
            String name = answer.isNull("name") ? null : answer.optString("name");
            if (path == null && name == null) { ACTIVE.remove(download.id); return; }
            download.start(path, name);
        }
    }

    /** trust_host: load pages from this host despite its certificate. */
    public static void trustHost(String host) { if (host != null) TRUSTED.add(host.toLowerCase(java.util.Locale.ROOT)); }

    /** Whether a web engine loads this address itself (browsing::engine_scheme). */
    private static boolean engineScheme(String url) {
        int colon = url == null ? -1 : url.indexOf(':');
        if (colon <= 0) return true;
        switch (url.substring(0, colon).toLowerCase(java.util.Locale.ROOT)) {
            case "http": case "https": case "about": case "blob": case "data": case "file":
            case "javascript": case "view-source": case "chrome": return true;
            default: return false;
        }
    }

    /**
     * Start what an intent: address names. Returns the web page to load instead when no app
     * takes it and the address carries a fallback, else "". The intent may only reach
     * activities that declare themselves browsable, as in Chrome: a page must not be able to
     * start an app's private activity.
     */
    public static String openIntent(String url) {
        try {
            android.content.Intent intent = android.content.Intent.parseUri(url, android.content.Intent.URI_INTENT_SCHEME);
            intent.addCategory(android.content.Intent.CATEGORY_BROWSABLE);
            intent.setComponent(null);
            intent.setSelector(null);
            intent.addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK);
            try { DayBridge.ctx.startActivity(intent); return ""; }
            catch (android.content.ActivityNotFoundException missing) {
                String fallback = intent.getStringExtra("browser_fallback_url");
                if (fallback != null && (fallback.startsWith("http://") || fallback.startsWith("https://"))) return fallback;
                String app = intent.getPackage();
                if (app != null) {
                    DayBridge.ctx.startActivity(new android.content.Intent(android.content.Intent.ACTION_VIEW,
                        android.net.Uri.parse("market://details?id=" + app)).addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK));
                }
            }
        } catch (Exception ignored) {}
        return "";
    }

    /** A main-frame load failed: say why in the front-end's terms (browsing::LoadErrorKind). */
    private static void loadFailed(WebView web, String url, String kind, int code, String text) {
        JSONObject notice = report("load-error");
        try {
            notice.put("url", url == null ? "" : url); notice.put("error", kind);
            notice.put("code", code); notice.put("text", text == null ? "" : text);
        } catch (JSONException e) { return; }
        notice(web, notice);
    }
    private static String errorKind(int code, String text) {
        String name = text == null ? "" : text;
        if (name.contains("INTERNET_DISCONNECTED") || name.contains("NETWORK_CHANGED")) return "offline";
        if (name.contains("NAME_NOT_RESOLVED")) return "host-not-found";
        if (name.contains("TIMED_OUT")) return "timeout";
        if (name.contains("CERT_")) return "certificate";
        if (name.contains("SSL_") || name.contains("_SSL")) return "secure";
        if (name.contains("UNKNOWN_URL_SCHEME")) return "unsupported";
        if (name.contains("CONNECTION_") || name.contains("ADDRESS_UNREACHABLE") || name.contains("EMPTY_RESPONSE")) return "connection";
        switch (code) {
            case WebViewClient.ERROR_HOST_LOOKUP: return "host-not-found";
            case WebViewClient.ERROR_TIMEOUT: return "timeout";
            case WebViewClient.ERROR_CONNECT: case WebViewClient.ERROR_IO: return "connection";
            case WebViewClient.ERROR_FAILED_SSL_HANDSHAKE: return "secure";
            case WebViewClient.ERROR_UNSUPPORTED_SCHEME: return "unsupported";
            default: return "other";
        }
    }

    // ---- downloads (WebView::on_download) -------------------------------------------------------

    /** Progress to the front-end's callback (browsing::download_event), on the UI thread. kind: 0 progress, 1 finished, 2 failed. */
    private static native void downloadEvent(long id, int kind, long received, long total, String message);
    private static void downloadReport(final long id, final int kind, final long received, final long total, final String message) {
        MAIN.post(() -> downloadEvent(id, kind, received, total, message == null ? "" : message));
    }
    private static final Map<Long, Download> ACTIVE = new java.util.concurrent.ConcurrentHashMap<>();

    /** One file being saved. WebView only announces a download; fetching it is this class's work. */
    private static final class Download {
        final long id; final String url, userAgent, referer, mime; final WebView web;
        /** The view's resource provider, which answers the app's own namespace (0 for none). */
        final long provider;
        final CookieManager cookies;
        volatile boolean canceled;
        volatile java.net.HttpURLConnection connection;
        // Where the bytes go: a path the app chose, or the system's Downloads collection.
        java.io.File file; android.net.Uri media; String name;
        // A blob: page streams its bytes here through the DownloadSink below, naming the
        // transfer by `token`.
        java.io.OutputStream blob; long received; String token;
        /** The document of `web` that offered the file. A blob: address means something only
         *  there, so a blob transfer starts only while that document is still the page. */
        final int document;
        Download(long id, WebView web, String url, String userAgent, String mime, long provider) {
            this.id = id; this.web = web; this.url = url; this.userAgent = userAgent; this.provider = provider;
            this.mime = mime == null ? "" : mime;
            this.referer = web.getUrl();
            this.document = document(web);
            this.cookies = WebViewFeature.isFeatureSupported(WebViewFeature.MULTI_PROFILE)
                ? WebViewCompat.getProfile(web).getCookieManager() : CookieManager.getInstance();
        }
        /** Open the destination. Android 10 and later write straight into the Downloads
         *  collection, where the entry stays hidden until `commit`; earlier systems write into
         *  the app's own downloads folder and register the finished file. */
        java.io.OutputStream open() throws java.io.IOException {
            if (file != null) return new java.io.FileOutputStream(file);
            if (android.os.Build.VERSION.SDK_INT >= 29) {
                android.content.ContentValues values = new android.content.ContentValues();
                values.put(android.provider.MediaStore.Downloads.DISPLAY_NAME, name);
                if (!mime.isEmpty()) values.put(android.provider.MediaStore.Downloads.MIME_TYPE, mime);
                values.put(android.provider.MediaStore.Downloads.IS_PENDING, 1);
                android.content.ContentResolver resolver = DayBridge.ctx.getContentResolver();
                media = resolver.insert(android.provider.MediaStore.Downloads.EXTERNAL_CONTENT_URI, values);
                if (media == null) throw new java.io.IOException("the Downloads collection refused the file");
                java.io.OutputStream out = resolver.openOutputStream(media);
                if (out == null) throw new java.io.IOException("the Downloads collection refused the file");
                return out;
            }
            java.io.File dir = DayBridge.ctx.getExternalFilesDir(android.os.Environment.DIRECTORY_DOWNLOADS);
            if (dir == null) dir = new java.io.File(DayBridge.ctx.getFilesDir(), "Download");
            dir.mkdirs();
            int dot = name.lastIndexOf('.');
            String stem = dot > 0 ? name.substring(0, dot) : name, extension = dot > 0 ? name.substring(dot) : "";
            file = new java.io.File(dir, name);
            for (int n = 2; file.exists(); n++) file = new java.io.File(dir, stem + " (" + n + ")" + extension);
            return new java.io.FileOutputStream(file);
        }
        /** The file is whole: make it visible, and say where it is. */
        @SuppressWarnings("deprecation") // addCompletedDownload is how Android 7 to 9 list a file in Downloads
        String commit() {
            if (media != null) {
                android.content.ContentValues values = new android.content.ContentValues();
                values.put(android.provider.MediaStore.Downloads.IS_PENDING, 0);
                DayBridge.ctx.getContentResolver().update(media, values, null, null);
                return media.toString();
            }
            if (name != null && android.os.Build.VERSION.SDK_INT < 29) {
                try {
                    android.app.DownloadManager manager = (android.app.DownloadManager) DayBridge.ctx.getSystemService(android.content.Context.DOWNLOAD_SERVICE);
                    long entry = manager.addCompletedDownload(file.getName(), file.getName(), true,
                        mime.isEmpty() ? "application/octet-stream" : mime, file.getAbsolutePath(), Math.max(received, 1), false);
                    android.net.Uri shared = manager.getUriForDownloadedFile(entry);
                    if (shared != null) return shared.toString();
                } catch (Exception ignored) {}
            }
            return file.getAbsolutePath();
        }
        void finish() {
            ACTIVE.remove(id);
            // The size as it ended: the last progress report may be older than the last bytes.
            downloadReport(id, 0, received, received, "");
            downloadReport(id, 1, received, received, commit());
        }
        void fail(String message) {
            ACTIVE.remove(id);
            if (media != null) { try { DayBridge.ctx.getContentResolver().delete(media, null, null); } catch (Exception ignored) {} }
            else if (file != null) file.delete();
            downloadReport(id, 2, 0, -1, canceled ? "canceled" : message);
        }
        /** Save to `path`, or to the user's Downloads as `name` when `path` is null. */
        void start(String path, String name) {
            if (path != null) file = new java.io.File(path);
            this.name = name;
            if (url.startsWith("blob:")) { startBlob(); return; }
            new Thread(() -> {
                try {
                    if (url.startsWith("data:")) saveData(); else fetch();
                    finish();
                } catch (Exception error) {
                    fail(error.getMessage() == null ? error.getClass().getSimpleName() : error.getMessage());
                }
            }, "day-webview-download").start();
        }
        private void saveData() throws java.io.IOException {
            int comma = url.indexOf(',');
            if (comma < 0) throw new java.io.IOException("malformed data: address");
            String head = url.substring(5, comma);
            byte[] bytes = DayWebContentType.percentDecode(url.substring(comma + 1));
            if (head.toLowerCase(java.util.Locale.ROOT).endsWith(";base64")) {
                try { bytes = android.util.Base64.decode(bytes, android.util.Base64.DEFAULT); }
                catch (IllegalArgumentException error) { throw new java.io.IOException("malformed base64 in a data: address"); }
            }
            try (java.io.OutputStream out = open()) { out.write(bytes); }
            received = bytes.length;
        }
        /** GET the file with the page's cookies. HttpURLConnection will not follow a redirect
         *  between http and https, so each hop is made here, with that host's own cookies. */
        private void fetch() throws java.io.IOException {
            if (provider != 0 && "day-resource.invalid".equals(android.net.Uri.parse(url).getHost())) {
                // The app's own namespace is no server: its provider answers, as it does the view.
                android.webkit.WebResourceResponse response = resource(provider, url, "GET", "");
                if (response == null || response.getStatusCode() >= 400) throw new java.io.IOException("HTTP " + (response == null ? 404 : response.getStatusCode()));
                try (java.io.InputStream in = response.getData(); java.io.OutputStream out = open()) {
                    byte[] buffer = new byte[65536];
                    for (int n; (n = in.read(buffer)) > 0; ) { out.write(buffer, 0, n); received += n; }
                }
                return;
            }
            String next = url;
            for (int hop = 0; ; hop++) {
                if (hop > 10) throw new java.io.IOException("too many redirects");
                java.net.HttpURLConnection c = (java.net.HttpURLConnection) new java.net.URL(next).openConnection();
                connection = c;
                c.setInstanceFollowRedirects(false);
                c.setConnectTimeout(30000); c.setReadTimeout(60000);
                if (userAgent != null) c.setRequestProperty("User-Agent", userAgent);
                // Each hop hears only what a browser's default policy would tell it.
                String from = DayWebContentType.referrer(referer, next);
                if (from != null) c.setRequestProperty("Referer", from);
                String cookie = cookies.getCookie(next);
                if (cookie != null && !cookie.isEmpty()) c.setRequestProperty("Cookie", cookie);
                int status = c.getResponseCode();
                if (status >= 300 && status < 400 && c.getHeaderField("Location") != null) {
                    next = new java.net.URL(new java.net.URL(next), c.getHeaderField("Location")).toString();
                    c.disconnect();
                    continue;
                }
                if (status >= 400) throw new java.io.IOException("HTTP " + status);
                long total = c.getContentLengthLong();
                try (java.io.InputStream in = c.getInputStream(); java.io.OutputStream out = open()) {
                    byte[] buffer = new byte[65536];
                    long last = 0;
                    for (int n; (n = in.read(buffer)) > 0; ) {
                        if (canceled) throw new java.io.IOException("canceled");
                        out.write(buffer, 0, n);
                        received += n;
                        long now = android.os.SystemClock.uptimeMillis();
                        if (now - last >= 100) { last = now; downloadReport(id, 0, received, total, ""); }
                    }
                } finally { c.disconnect(); }
                if (canceled) throw new java.io.IOException("canceled");
                return;
            }
        }
        /** End a blob transfer once: whole (`error` null) or failed. Safe from any thread, and
         *  a second call finds nothing to end. */
        void endBlob(String error) {
            synchronized (this) {
                if (blob == null) return;
                try { blob.close(); } catch (Exception ignored) {}
                blob = null;
                if (token != null) BLOBS.remove(token);
                if (error == null && !canceled) finish(); else fail(error == null ? "canceled" : error);
            }
        }
        /** A blob: address exists only inside its page, so the page reads it and hands the bytes over. */
        private void startBlob() {
            // The answer may come after the page moved on. The new document did not offer this
            // file and is never handed its transfer.
            if (document != document(web)) { fail("the page was left"); return; }
            try { blob = open(); }
            catch (java.io.IOException error) { fail(error.getMessage()); return; }
            byte[] random = new byte[16];
            RANDOM.nextBytes(random);
            this.token = new java.math.BigInteger(1, random).toString(16);
            BLOBS.put(this.token, this);
            String token = JSONObject.quote(this.token), address = JSONObject.quote(url);
            web.evaluateJavascript("(async()=>{const t=" + token + ";try{const b=await(await fetch(" + address + ")).blob();"
                + "for(let o=0;o<b.size;o+=524288){const u=new Uint8Array(await b.slice(o,o+524288).arrayBuffer());let s='';"
                + "for(let i=0;i<u.length;i+=32768)s+=String.fromCharCode.apply(null,u.subarray(i,i+32768));"
                + "__dayDownloads.chunk(t,btoa(s),b.size);}__dayDownloads.done(t);}catch(e){__dayDownloads.fail(t,String(e));}})()", null);
        }
    }

    /** open_download: show a saved file in the app that handles its type. */
    public static boolean openDownload(String location) {
        try {
            if (location == null || !location.startsWith("content:")) return false;
            android.net.Uri uri = android.net.Uri.parse(location);
            String type = DayBridge.ctx.getContentResolver().getType(uri);
            android.content.Intent view = new android.content.Intent(android.content.Intent.ACTION_VIEW);
            view.setDataAndType(uri, type == null ? "*/*" : type);
            view.addFlags(android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION | android.content.Intent.FLAG_ACTIVITY_NEW_TASK);
            DayBridge.ctx.startActivity(view);
            return true;
        } catch (Exception error) { return false; }
    }
    /** remove_download: delete a saved file. */
    public static void removeDownload(String location) {
        try {
            if (location == null) return;
            if (location.startsWith("content:")) DayBridge.ctx.getContentResolver().delete(android.net.Uri.parse(location), null, null);
            else new java.io.File(location).delete();
        } catch (Exception ignored) {}
    }

    /**
     * Links name their files (`<a download="report.pdf">`), which WebView's DownloadListener
     * never hears: it gets the address and the server's headers. A page script notes the name
     * with the view's DownloadSink as the link is clicked.
     */
    private static final String LINK_NAME_SCRIPT = "(()=>{if(window.__dayDownloadNames)return;window.__dayDownloadNames=true;"
        + "const note=a=>{try{if(a&&a.hasAttribute&&a.hasAttribute('download')&&a.href)__dayDownloads.name(a.href,a.getAttribute('download')||'');}catch(e){}};"
        + "document.addEventListener('click',e=>{note(e.composedPath().find(n=>n&&n.tagName==='A'));},true);"
        + "const click=HTMLAnchorElement.prototype.click;HTMLAnchorElement.prototype.click=function(){note(this);return click.apply(this,arguments);};})()";

    /** What a page hands a blob: download's bytes to. It accepts bytes only for a download the app placed. */
    private static final class DownloadSink {
        /** The one view whose pages this object is exposed to. Weak, since SINKS holds this
         *  object by that view as its weak key and must not keep the view alive through it. */
        final java.lang.ref.WeakReference<WebView> owner;
        /** The names this view's links gave their files, by address. */
        final Map<String, String> names = new java.util.concurrent.ConcurrentHashMap<>();
        DownloadSink(WebView owner) { this.owner = new java.lang.ref.WeakReference<>(owner); }
        @android.webkit.JavascriptInterface public void name(String address, String name) {
            if (address == null || name == null || name.isEmpty()) return;
            if (names.size() > 64) names.clear();
            names.put(address, name);
        }
        /** The transfer `token` names, if it is this view's and still open. A token is a random
         *  number made for one transfer and told only to the page that holds the blob, so
         *  another page, or another view, has nothing to name someone else's file with. */
        private Download find(String token) {
            Download d = token == null ? null : BLOBS.get(token);
            return d != null && d.web == owner.get() && d.blob != null ? d : null;
        }
        @android.webkit.JavascriptInterface public void chunk(String token, String base64, double total) {
            Download d = find(token);
            if (d == null) return;
            synchronized (d) {
                if (d.blob == null) return;
                try {
                    byte[] bytes = android.util.Base64.decode(base64, android.util.Base64.DEFAULT);
                    d.blob.write(bytes); d.received += bytes.length;
                    downloadReport(d.id, 0, d.received, (long) total, "");
                } catch (Exception error) { d.endBlob(error.getMessage() == null ? "write failed" : error.getMessage()); }
            }
        }
        @android.webkit.JavascriptInterface public void done(String token) {
            Download d = find(token);
            if (d != null) d.endBlob(null);
        }
        @android.webkit.JavascriptInterface public void fail(String token, String message) {
            Download d = find(token);
            if (d != null) d.endBlob(message == null ? "the page could not read the file" : message);
        }
    }
    /** Each download-taking view's sink, for the names its links gave. */
    private static final Map<WebView, DownloadSink> SINKS = java.util.Collections.synchronizedMap(new WeakHashMap<WebView, DownloadSink>());
    /** Blob transfers in flight, by their token. */
    private static final Map<String, Download> BLOBS = new java.util.concurrent.ConcurrentHashMap<>();
    private static final java.security.SecureRandom RANDOM = new java.security.SecureRandom();

    /** How many documents each view has left: the count names the one it shows now. */
    private static final Map<WebView, Integer> DOCUMENTS = new WeakHashMap<>();
    private static int document(WebView web) {
        Integer document = DOCUMENTS.get(web);
        return document == null ? 0 : document;
    }

    /** A view's page is gone (a new document, a lost renderer, the view released): no more of
     *  a blob can arrive, so its transfers end here rather than stay open for good, and a
     *  blob download still awaiting the app's answer no longer belongs to the page. */
    private static void endBlobs(WebView web, String reason) {
        Integer document = DOCUMENTS.get(web);
        DOCUMENTS.put(web, document == null ? 1 : document + 1);
        for (Download d : new java.util.ArrayList<>(BLOBS.values())) if (d.web == web) d.endBlob(reason);
    }

    /** cancel_download: stop a download in flight; its callback hears "canceled". */
    public static void cancelDownload(long id) {
        final Download d = ACTIVE.get(id);
        if (d == null) return;
        d.canceled = true;
        // A blob transfer has no connection to close and may never hear from its page again:
        // it ends here.
        d.endBlob("canceled");
        // Closing a socket is network work, which Android refuses on the UI thread.
        new Thread(() -> { java.net.HttpURLConnection c = d.connection; if (c != null) c.disconnect(); }).start();
    }

    /**
     * The name a download suggests: the server's (Content-Disposition, RFC 6266), else the
     * address's last path segment, else Android's guess from the type. URLUtil.guessFileName
     * alone would swap the extension a server gave for the one it maps the type to
     * ("tide-table.txt" sent as application/octet-stream becomes "tide-table.bin").
     */
    static String downloadName(String url, String disposition, String mime) {
        if (disposition != null) {
            java.util.regex.Matcher extended = java.util.regex.Pattern.compile("filename\\*\\s*=\\s*([^']*)'[^']*'([^;]+)", java.util.regex.Pattern.CASE_INSENSITIVE).matcher(disposition);
            if (extended.find()) {
                try { return java.net.URLDecoder.decode(extended.group(2).trim().replace("+", "%2B"), extended.group(1).isEmpty() ? "UTF-8" : extended.group(1)); }
                catch (Exception ignored) {}
            }
            java.util.regex.Matcher plain = java.util.regex.Pattern.compile("filename\\s*=\\s*(\"([^\"]*)\"|[^;]+)", java.util.regex.Pattern.CASE_INSENSITIVE).matcher(disposition);
            if (plain.find()) {
                String name = plain.group(2) != null ? plain.group(2) : plain.group(1);
                if (name != null && !name.trim().isEmpty()) return name.trim();
            }
        }
        if (url != null && !url.startsWith("data:") && !url.startsWith("blob:")) {
            String path = android.net.Uri.parse(url).getLastPathSegment();
            if (path != null && path.contains(".")) return path;
        }
        return android.webkit.URLUtil.guessFileName(url, disposition, mime);
    }

    /** WebView offers a file it will not show: ask the app where it goes. */
    private static void offerDownload(WebView web, long provider, String url, String userAgent, String disposition, String mime, long length) {
        if (url == null) return;
        JSONObject report = report("download");
        try {
            report.put("url", url); DownloadSink sink = SINKS.get(web);
            String linked = sink == null ? null : sink.names.remove(url);
            report.put("name", linked != null ? linked : downloadName(url, disposition, mime));
            report.put("mime", mime == null ? "" : mime); report.put("size", length);
        } catch (JSONException e) { return; }
        // The download's id is the question's id: the app's answer and cancel_download name it.
        Long node = IDS.get(web);
        if (node == null) return;
        long id = nextPrompt++;
        Download download = new Download(id, web, url, userAgent, mime, provider);
        try { report.put("id", id); } catch (JSONException e) { return; }
        ACTIVE.put(id, download);
        PROMPTS.put(id, download);
        PROMPT_OWNERS.put(id, web);
        DayBridge.nativeOnEvent(node, 12, -11.0, report.toString());
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

    /** What each view was asked to tell sites it is (WebView::user_agent), where not the engine's own. */
    private static final Map<WebView, String> USER_AGENTS = new WeakHashMap<>();

    /**
     * Chrome's user agent, from WebView's: the same string without the two marks of an
     * embedded view, "; wv" in the device part and "Version/4.0" before Chrome's own version.
     */
    static String browserAgent(String engine) {
        return engine == null ? null : engine.replace("; wv)", ")").replace(" Version/4.0 ", " ");
    }

    /** The view's user agent outside desktop mode; null is the engine's own. */
    private static String userAgent(WebView web) {
        String asked = USER_AGENTS.get(web);
        if (asked == null || asked.isEmpty()) return null;
        if (asked.equals("\u0001")) return browserAgent(android.webkit.WebSettings.getDefaultUserAgent(web.getContext()));
        return asked;
    }

    /** Apply WebView::desktop_mode; true when it changed what the view asks sites for. */
    private static boolean applyDesktop(WebView web, boolean desktop) {
        android.webkit.WebSettings settings = web.getSettings();
        boolean changed = settings.getUseWideViewPort() != desktop;
        // The desktop user agent, and a desktop-width layout viewport for pages that declare no
        // viewport of their own, shown zoomed out to fit, which is what Chrome's desktop mode
        // does. A null agent restores the engine's own.
        settings.setUserAgentString(desktop ? desktopAgent(android.webkit.WebSettings.getDefaultUserAgent(web.getContext())) : userAgent(web));
        settings.setUseWideViewPort(desktop);
        settings.setLoadWithOverviewMode(desktop);
        // A page laid out wider than the screen has to be pinched to be read.
        settings.setBuiltInZoomControls(desktop || BROWSING.contains(web));
        settings.setDisplayZoomControls(false);
        return changed;
    }

    /** WebView::desktop_mode changed: apply it and reload, so the site answers the new agent. */
    public static void setDesktopMode(View view, boolean desktop) {
        if (view instanceof WebView && !DEAD.contains(view) && applyDesktop((WebView) view, desktop)) {
            ((WebView) view).reload();
        }
    }

    /**
     * WebView::javascript changed: apply it and reload, since a page cannot gain or lose its
     * scripts while it runs. Android has no separate world for the app's own scripts, so with
     * page scripts off {@code evaluateJavascript} runs nothing either; {@link #evalJs} says so.
     */
    public static void setJavaScript(View view, boolean enabled) {
        if (!(view instanceof WebView) || DEAD.contains(view)) return;
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
        // alert(), confirm() and prompt() go to the app (WebView::on_dialog), which answers
        // through promptAnswer. Leaving a page (beforeunload) keeps WebView's own dialog.
        @Override public boolean onJsAlert(WebView view, String url, String message, android.webkit.JsResult result) {
            return dialog(view, "alert", url, message, "", result);
        }
        @Override public boolean onJsConfirm(WebView view, String url, String message, android.webkit.JsResult result) {
            return dialog(view, "confirm", url, message, "", result);
        }
        @Override public boolean onJsPrompt(WebView view, String url, String message, String fallback, android.webkit.JsPromptResult result) {
            return dialog(view, "prompt", url, message, fallback, new PromptPending(result, fallback == null ? "" : fallback));
        }
        // input type=file: WebView has no chooser of its own. The front-end shows Day's picker.
        @Override public boolean onShowFileChooser(WebView view, ValueCallback<android.net.Uri[]> callback, FileChooserParams params) {
            JSONObject report = report("files");
            try {
                org.json.JSONArray accept = new org.json.JSONArray();
                String[] types = params.getAcceptTypes();
                if (types != null) for (String type : types) if (type != null && !type.isEmpty()) accept.put(type);
                report.put("accept", accept);
                report.put("multiple", params.getMode() == FileChooserParams.MODE_OPEN_MULTIPLE);
            } catch (JSONException e) { return false; }
            return ask(view, new FileChoice(callback), report) != 0;
        }
        @Override public boolean onCreateWindow(WebView source, boolean dialog, boolean userGesture, android.os.Message result) {
            if (!userGesture || !TAB_LABELS.containsKey(source)) return false;
            WebView pending=new WebView(source.getContext());
            final boolean[] delivered={false};
            pending.setWebViewClient(new WebViewClient() {
                // A lost renderer takes every view with it, this short-lived one included; an
                // unhandled loss ends the app.
                @Override public boolean onRenderProcessGone(WebView view, android.webkit.RenderProcessGoneDetail detail) { return true; }
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
        cancelPrompts(web);MAIN_URL.remove(web);BROWSING.remove(web);endBlobs(web,"the page was closed");
        if(SINKS.remove(web)!=null) web.removeJavascriptInterface("__dayDownloads");
        DOCUMENTS.remove(web);USER_AGENTS.remove(web);
        if(DEAD.remove(web)) {
            // A view without a renderer takes no call but destroy().
            if(web.getParent() instanceof android.view.ViewGroup)((android.view.ViewGroup)web.getParent()).removeView(web);
            web.destroy();return;
        }
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
            double zoom, boolean desktop, boolean javascript, long hooks, String content, boolean mediaHandler,
            int duties, String userAgent) {
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
        // WebView::user_agent, before the first load and before desktop mode replaces it.
        if (userAgent != null && !userAgent.isEmpty()) {
            USER_AGENTS.put(web, userAgent);
            web.getSettings().setUserAgentString(userAgent(web));
        }
        final boolean askAuth = (duties & 1) != 0;
        if ((duties & 2) != 0) {
            // WebView::on_download: the app places each file, and blob: pages hand their bytes over.
            DownloadSink sink = new DownloadSink(web);
            SINKS.put(web, sink);
            web.addJavascriptInterface(sink, "__dayDownloads");
            if (WebViewFeature.isFeatureSupported(WebViewFeature.DOCUMENT_START_SCRIPT)) {
                WebViewCompat.addDocumentStartJavaScript(web, LINK_NAME_SCRIPT, java.util.Collections.singleton("*"));
            }
            web.setDownloadListener((address, agent, disposition, mime, length) -> offerDownload(web, provider, address, agent, disposition, mime, length));
        }
        if (inlinePrefix == null || inlinePrefix.isEmpty()) {
            // A view browsing the web pinches to zoom, as a browser does, where the page allows
            // it. An app's own bundled pages stay at the scale they were designed for.
            BROWSING.add(web);
            web.getSettings().setSupportZoom(true);
            web.getSettings().setBuiltInZoomControls(true);
            web.getSettings().setDisplayZoomControls(false);
        }
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
                if (request.isForMainFrame()) MAIN_URL.put(view, request.getUrl().toString());
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
                endBlobs(view, "the page was left");
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
                if(SINKS.containsKey(view)) view.evaluateJavascript(LINK_NAME_SCRIPT,null);
                // kind 12 = a piece-defined Custom event (§8.2's open channel): the front-end's
                // cx.on reads the text payload as the URL. (No longer hijacking kind 1 = TextChanged.)
                DayBridge.nativeOnEvent(id, 12, 0.0, finishedUrl);
            }

            // A failed main-frame load (WebView::on_load_error). WebView shows its own error
            // page in the view either way.
            @Override
            public void onReceivedError(WebView view, android.webkit.WebResourceRequest request, android.webkit.WebResourceError error) {
                if (!request.isForMainFrame()) return;
                String text = error.getDescription() == null ? "" : error.getDescription().toString();
                loadFailed(view, request.getUrl().toString(), errorKind(error.getErrorCode(), text), error.getErrorCode(), text);
            }
            // A certificate the system does not trust: stop, unless the user chose to visit this
            // host anyway (trust_host). Only the page's own address is worth an error page; a
            // subresource just fails to load.
            @Override
            public void onReceivedSslError(WebView view, android.webkit.SslErrorHandler handler, android.net.http.SslError error) {
                String address = error.getUrl();
                String host = address == null ? null : android.net.Uri.parse(address).getHost();
                if (host != null && TRUSTED.contains(host.toLowerCase(java.util.Locale.ROOT))) { handler.proceed(); return; }
                handler.cancel();
                if (address != null && address.equals(MAIN_URL.get(view))) {
                    loadFailed(view, address, "certificate", error.getPrimaryError(), error.toString());
                }
            }
            // HTTP Basic and Digest sign-in (WebView::on_auth); WebView cancels by default.
            @Override
            public void onReceivedHttpAuthRequest(WebView view, android.webkit.HttpAuthHandler handler, String host, String realm) {
                if (!askAuth) { handler.cancel(); return; }
                JSONObject report = report("auth");
                try {
                    report.put("host", host == null ? "" : host); report.put("realm", realm == null ? "" : realm);
                    // WebView has no count; it does say whether the stored answer was already refused.
                    report.put("failures", handler.useHttpAuthUsernamePassword() ? 0 : 1);
                } catch (JSONException e) { handler.cancel(); return; }
                if (ask(view, handler, report) == 0) handler.cancel();
            }
            // The renderer process is gone (WebView::on_renderer_gone). Returning false here
            // ends the app; returning true leaves a view that accepts no call but destroy().
            @Override
            public boolean onRenderProcessGone(WebView view, android.webkit.RenderProcessGoneDetail detail) {
                DEAD.add(view);
                cancelPrompts(view);
                endBlobs(view, "the page's process ended");
                JSONObject notice = report("renderer-gone");
                try { notice.put("crashed", detail.didCrash()); } catch (JSONException ignored) {}
                notice(view, notice);
                return true;
            }
            @Override
            @SuppressWarnings("deprecation") // the String overload runs on every API level
            public boolean shouldOverrideUrlLoading(WebView view, String target) {
                if(TAB_LABELS.containsKey(view) && target.startsWith("day-tab://")) {
                    android.net.Uri request=android.net.Uri.parse(target);
                    newTab(view,request.getQueryParameter("url"),"1".equals(request.getQueryParameter("background")));return true;
                }
                // mailto:, tel:, an app's own scheme, intent: are another program's to open.
                // Cancel and report (num -1), and the Rust side runs the app's LinkPolicy.
                if (!inline && !engineScheme(target)) {
                    DayBridge.nativeOnEvent(id, 12, -1.0, target);
                    return true;
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
        if (DEAD.contains(view)) {
            DayBridge.nativeOnEvent(nodeId, 12, req, evalError("the page's process is gone"));
            return;
        }
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
        if (DEAD.contains(web)) return;
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
