// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// The web-view piece's own Qt shim behind a flat C ABI. When Qt6WebEngineWidgets is available
// (build.rs probes pkg-config and defines DAY_WEBVIEW_QT_ENGINE) this wraps a real QWebEngineView
// and forwards `loadFinished` to a C callback so a bound text field follows navigation. When it is
// not (e.g. MSYS2/MINGW64, which does not package Qt6 WebEngine because Chromium won't build with
// MinGW GCC), it degrades to a QLabel showing the URL, so windows-qt still builds, launches and
// screenshots (mirrors day-piece-webview's xaml EdgeHTML degrade). The C ABI is identical either
// way, so lib-qt.rs is unchanged. The callback's `const char*` is only valid for the call (Rust
// copies it).

#include <QUrl>
#include <QVBoxLayout>
#include <QWidget>

#include <cstdint>
#include <map>
#include <string>

// A JavaScript-evaluation reply, in the 0x1F-separated form the Rust front-end decodes
// (docs/webview-eval.md). Only used for engine failures; a script that merely throws is caught by
// the front-end's JS wrapper and arrives as an ordinary result string.
static std::string day_webview_eval_error(const char *msg) {
    std::string s = "0";
    s.push_back('\x1f');
    s += "QtWebEngine";
    s.push_back('\x1f');
    s += msg;
    return s;
}

// Set once from Rust. Qt has no error channel on runJavaScript (a throw, a syntax error and a
// null result all arrive as the same invalid QVariant), which is why the front-end wraps every
// script in try/catch before it gets here.
static void (*g_eval_cb)(uint64_t, uint64_t, const char *) = nullptr;

#ifdef DAY_WEBVIEW_QT_ENGINE

#include <QWebEnginePage>
#include <QWebEngineSettings>
#include <QWebEngineFullScreenRequest>
#include <QWebEngineView>
#include <QWebEngineProfile>
#include <QWebEngineUrlScheme>
#include <QWebEngineUrlSchemeHandler>
#include <QWebEngineUrlRequestJob>
#include <QBuffer>
#include <QPointer>
#include <QCoreApplication>
#include "resources.h"
static const bool resourceSchemeRegistered = [] {
    QWebEngineUrlScheme scheme("day-resource");
    scheme.setSyntax(QWebEngineUrlScheme::Syntax::Host);
    auto flags = QWebEngineUrlScheme::SecureScheme | QWebEngineUrlScheme::CorsEnabled;
#if QT_VERSION >= QT_VERSION_CHECK(6, 6, 0)
    flags |= QWebEngineUrlScheme::FetchApiAllowed;
#endif
    scheme.setFlags(flags);
    QWebEngineUrlScheme::registerScheme(scheme);
    return true;
}();
class ResourceHandler : public QWebEngineUrlSchemeHandler {
public:
    uint64_t provider;
    ResourceHandler(uint64_t p, QObject *parent): QWebEngineUrlSchemeHandler(parent), provider(p) {}
    void requestStarted(QWebEngineUrlRequestJob *job) override {
        auto pending = new QPointer<QWebEngineUrlRequestJob>(job);
        day_web_resource_start(provider, job->requestUrl().toEncoded().constData(), job->requestMethod().constData(),
            job->requestHeaders().value("Range").constData(), pending,
            [](void *context, DayResourceResponse *response) {
                QMetaObject::invokeMethod(QCoreApplication::instance(), [context,response] {
                    auto pending=static_cast<QPointer<QWebEngineUrlRequestJob> *>(context);
                    auto job=pending->data(); delete pending;
                    if (job) {
                        auto status=day_web_resource_status(response);
                        if (status >= 400) job->fail(status==404 ? QWebEngineUrlRequestJob::UrlNotFound : QWebEngineUrlRequestJob::RequestDenied);
                        else {
                            auto buffer=new QBuffer(job);
                            buffer->setData(reinterpret_cast<const char *>(day_web_resource_data(response)), day_web_resource_len(response));
                            buffer->open(QIODevice::ReadOnly);
#if QT_VERSION >= QT_VERSION_CHECK(6, 6, 0)
                            QMultiMap<QByteArray,QByteArray> headers;
                            for (const auto &line : QByteArray(day_web_resource_headers(response)).split('\n')) {
                                auto colon=line.indexOf(':'); if (colon>0) headers.insert(line.left(colon),line.mid(colon+1).trimmed());
                            }
                            job->setAdditionalResponseHeaders(headers);
#endif
                            job->reply(QByteArray(day_web_resource_mime(response)),buffer);
                        }
                    }
                    day_web_resource_free(response);
                }, Qt::QueuedConnection);
            });
    }
};


// Session id -> the retained engine view. Qt is the one backend whose `release` DELETES the handle
// (day_qt_delete -> deleteLater), so a second pointer to the container would dangle. What is
// retained instead is the QWebEngineView inside it: ~DayWebView re-parents it out before ~QWidget
// deletes its children, and the next container adopts it. The page, history and JS context live in
// the QWebEnginePage the view owns, so re-parenting is lossless.
static std::map<uint64_t, QWebEngineView *> g_sessions;

// Inline mode's link policy (docs/webview.md): a page that polices main-frame navigations
// against the bundled site's (scheme, path-prefix). Anything leaving the site is canceled and
// reported through `link_cb`; the Rust side runs the app's LinkPolicy (events are enqueue-only,
// so the decision cannot come back through this override). Remote views keep the default page.
class DayWebPage : public QWebEnginePage {
public:
    uint64_t id = 0;
    QString pathPrefix; // "/day/assets/<root>/"
    void (*linkCb)(uint64_t, const char *) = nullptr;

    using QWebEnginePage::QWebEnginePage;

protected:
    bool acceptNavigationRequest(const QUrl &url, NavigationType type, bool isMainFrame) override {
        (void)type;
        if (pathPrefix.isEmpty() || !isMainFrame)
            return true;
        const bool inside = (pathPrefix.startsWith("day-resource:") && url.toString().startsWith(pathPrefix)) || (url.scheme() == QStringLiteral("qrc") &&
                             url.path().startsWith(pathPrefix)) ||
                            url.toString() == QStringLiteral("about:blank");
        if (inside)
            return true;
        if (linkCb) {
            const QByteArray bytes = url.toString().toUtf8();
            linkCb(id, bytes.constData());
        }
        return false;
    }
};

class DayWebView : public QWidget {
public:
    QWebEngineView *view = nullptr;
    uint64_t id = 0;
    uint64_t session = 0;
    ~DayWebView() override {
        // Runs before ~QWidget deletes children, the only window in which the engine view can be
        // rescued from the container's destruction.
        if (session != 0 && view)
            view->setParent(nullptr);
    }
    void load(const QString &url) {
        if (view && !url.isEmpty())
            view->load(QUrl::fromUserInput(url));
    }
};

// (Re)point a view's navigation reports at the node currently showing it. A retained view outlives
// the node that first realized it, so the old connection would report to a torn-down node.
static void day_webview_connect_url(QWebEngineView *v, uint64_t id,
                                    void (*cb)(uint64_t, const char *)) {
    QObject::disconnect(v, &QWebEngineView::loadFinished, nullptr, nullptr);
    QObject::connect(v, &QWebEngineView::loadFinished, [v, id, cb](bool ok) {
        if (!ok)
            return;
        // Same-URL reloads must also report ready; urlChanged fires too early
        // and does not run again when the document's URL is unchanged.
        QByteArray bytes = v->url().toString().toUtf8();
        cb(id, bytes.constData());
    });
}

extern "C" {

void *day_webview_new(const char *url, uint64_t id, void (*cb)(uint64_t, const char *),
                      uint64_t session, const char *inline_path_prefix,
                      void (*link_cb)(uint64_t, const char *)) {
    DayWebView *w = new DayWebView();
    w->id = id;
    w->session = session;
    QVBoxLayout *lay = new QVBoxLayout(w);
    lay->setContentsMargins(0, 0, 0, 0);

    auto known = session != 0 ? g_sessions.find(session) : g_sessions.end();
    if (known != g_sessions.end()) {
        // Re-attach the retained engine: addWidget re-parents it. No load() here, so the page
        // comes back as it was left. An inline page's link reports must follow the node now
        // showing it, like the url reports below.
        QWebEngineView *v = known->second;
        // dynamic_cast, not qobject_cast: the shim compiles without moc, so DayWebPage carries
        // no Q_OBJECT metadata; plain RTTI is what identifies an inline page here.
        if (DayWebPage *p = dynamic_cast<DayWebPage *>(v->page()))
            p->id = id;
        day_webview_connect_url(v, id, cb);
        lay->addWidget(v);
        w->view = v;
        return w;
    }

    QWebEngineView *v = new QWebEngineView();
    const QString prefix = QString::fromUtf8(inline_path_prefix ? inline_path_prefix : "");
    if (prefix.startsWith("day-resource:")) {
        auto profile=new QWebEngineProfile();
        auto page=new DayWebPage(profile,v);
        QObject::connect(page,&QObject::destroyed,profile,&QObject::deleteLater);
        profile->installUrlSchemeHandler("day-resource",new ResourceHandler(QUrl(prefix).host().mid(1).toULongLong(),profile));
        page->id=id;page->pathPrefix=prefix;page->linkCb=link_cb;
        page->settings()->setUnknownUrlSchemePolicy(QWebEngineSettings::AllowAllUnknownUrlSchemes);
        v->setPage(page);
    }
    if (!prefix.isEmpty() && !prefix.startsWith("day-resource:")) {
        DayWebPage *page = new DayWebPage(v);
        page->id = id;
        page->pathPrefix = prefix;
        page->linkCb = link_cb;
        // Bundled JavaScript can send app-scheme links without a mouse gesture. Let them
        // reach acceptNavigationRequest, which cancels external main-frame navigation and
        // hands it to Day's LinkPolicy instead of launching a system handler here.
        page->settings()->setUnknownUrlSchemePolicy(QWebEngineSettings::AllowAllUnknownUrlSchemes);
        v->setPage(page);
    }
    v->settings()->setAttribute(QWebEngineSettings::FullScreenSupportEnabled, true);
    QObject::connect(v->page(), &QWebEnginePage::fullScreenRequested, v,
        [v, previous = Qt::WindowStates()](QWebEngineFullScreenRequest request) mutable {
            auto window = v->window();
            if (request.toggleOn()) {
                previous = window->windowState();
                window->showFullScreen();
            } else {
                window->setWindowState(previous);
            }
            request.accept();
        });
    day_webview_connect_url(v, id, cb);
    lay->addWidget(v);
    w->view = v;
    if (session != 0)
        g_sessions[session] = v;
    w->load(QString::fromUtf8(url));
    return w;
}

void day_webview_set_eval_cb(void (*cb)(uint64_t, uint64_t, const char *)) { g_eval_cb = cb; }

void day_webview_eval(void *w, uint64_t req, const char *script) {
    DayWebView *self = static_cast<DayWebView *>(w);
    const uint64_t id = self->id;
    if (!self->view) {
        if (g_eval_cb) {
            const std::string e = day_webview_eval_error("no engine");
            g_eval_cb(id, req, e.c_str());
        }
        return;
    }
    // The main world (the default), matching WKWebView's `evaluateJavaScript`, so a script sees the
    // page's own globals on every backend alike.
    //
    // Capture PODs only. Qt guarantees this callback runs even while the page is being destroyed,
    // and touching the page or view from there is undefined behavior; the guarantee is what keeps
    // the Rust future from leaking, and the constraint is the price of it.
    self->view->page()->runJavaScript(QString::fromUtf8(script), [id, req](const QVariant &v) {
        if (!g_eval_cb)
            return;
        if (!v.isValid()) {
            const std::string e = day_webview_eval_error("no result (page discarded)");
            g_eval_cb(id, req, e.c_str());
            return;
        }
        const QByteArray bytes = v.toString().toUtf8();
        g_eval_cb(id, req, bytes.constData());
    });
}

void day_webview_load(void *w, const char *url) {
    static_cast<DayWebView *>(w)->load(QString::fromUtf8(url));
}
void day_webview_back(void *w) {
    if (QWebEngineView *v = static_cast<DayWebView *>(w)->view)
        v->back();
}
void day_webview_forward(void *w) {
    if (QWebEngineView *v = static_cast<DayWebView *>(w)->view)
        v->forward();
}
void day_webview_stop(void *w) {
    if (QWebEngineView *v = static_cast<DayWebView *>(w)->view)
        v->stop();
}
void day_webview_reload(void *w) {
    if (QWebEngineView *v = static_cast<DayWebView *>(w)->view)
        v->reload();
}
// Draw no background of the view's own (WebView::transparent): the page composites over what is
// behind the widget. The container QWidget paints nothing by default, so the page is the only
// layer that needed telling.
void day_webview_set_transparent(void *w) {
    if (QWebEngineView *v = static_cast<DayWebView *>(w)->view)
        v->page()->setBackgroundColor(Qt::transparent);
}

} // extern "C"

#elif defined(_WIN32)
#include "lib-qt-windows.inc"
#else // no Qt6WebEngineWidgets: degrade to a URL label (QtWidgets only, already linked by day-qt-sys)

#include <QLabel>

class DayWebView : public QWidget {
public:
    QLabel *label = nullptr;
    uint64_t id = 0;
    void load(const QString &url) {
        if (label)
            label->setText(url);
    }
};

extern "C" {

void *day_webview_new(const char *url, uint64_t id, void (*cb)(uint64_t, const char *),
                      uint64_t session, const char *inline_path_prefix,
                      void (*link_cb)(uint64_t, const char *)) {
    (void)cb;                 // no navigation to report without a real engine
    (void)session;            // nothing to retain either
    (void)inline_path_prefix; // and no engine to police; the label shows the qrc URL
    (void)link_cb;
    DayWebView *w = new DayWebView();
    w->id = id;
    QVBoxLayout *lay = new QVBoxLayout(w);
    lay->setContentsMargins(0, 0, 0, 0);
    QLabel *l = new QLabel();
    l->setText(QString::fromUtf8(url));
    l->setAlignment(Qt::AlignTop | Qt::AlignLeft);
    l->setTextInteractionFlags(Qt::TextSelectableByMouse);
    lay->addWidget(l);
    w->label = l;
    return w;
}
void day_webview_set_transparent(void *w) {
    (void)w; // a label paints no background of its own already
}

void day_webview_load(void *w, const char *url) {
    static_cast<DayWebView *>(w)->load(QString::fromUtf8(url));
}
void day_webview_back(void *) {}
void day_webview_forward(void *) {}
void day_webview_stop(void *) {}
void day_webview_reload(void *) {}

void day_webview_set_eval_cb(void (*cb)(uint64_t, uint64_t, const char *)) { g_eval_cb = cb; }

// No engine to evaluate in, but the reply is still mandatory: the Rust future is only resolved by a
// callback, so staying silent here would hang the caller forever on windows-qt.
void day_webview_eval(void *w, uint64_t req, const char *script) {
    (void)script;
    if (g_eval_cb) {
        const std::string e = day_webview_eval_error("no Qt6 WebEngine in this build");
        g_eval_cb(static_cast<DayWebView *>(w)->id, req, e.c_str());
    }
}

} // extern "C"

#endif
