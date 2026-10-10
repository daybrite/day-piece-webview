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
#include <memory>

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
#include <QWebEngineNewWindowRequest>
#include <QWebEngineContextMenuRequest>
#include <QContextMenuEvent>
#include <QMenu>
#include <QWebEngineHistory>
#include <QWebEngineSettings>
#include <QWebEngineFullScreenRequest>
#include <QWebEngineView>
#include <QWebEngineProfile>
#include <QWebEngineCookieStore>
#include <QNetworkCookie>
#include <QJsonDocument>
#include <QJsonObject>
#include <QJsonArray>
#include <QDir>
#include <QStandardPaths>
#include <QDateTime>
#include <QTimer>
#include <QSet>
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
        day_web_resource_start(job->requestUrl().host().mid(1).toULongLong(), job->requestUrl().toEncoded().constData(), job->requestMethod().constData(),
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


struct ProfileState {
    QString key, directory; bool privateMode;
    QWebEngineProfile *profile;
    QList<QNetworkCookie> cookies;
    QSet<QWebEngineView *> views;
};
static std::map<QString, ProfileState *> profiles;
static QWebEngineProfile *makeProfile(ProfileState *state) {
    auto profile = state->privateMode ? new QWebEngineProfile(QCoreApplication::instance())
        : new QWebEngineProfile(state->key, QCoreApplication::instance());
    if (!state->privateMode) {
        const auto root = state->directory.isEmpty() ? QStandardPaths::writableLocation(QStandardPaths::AppDataLocation) + "/day-web-profiles" : state->directory;
        profile->setPersistentStoragePath(root + "/" + state->key + "/data");
        profile->setCachePath(root + "/" + state->key + "/cache");
        profile->setPersistentCookiesPolicy(QWebEngineProfile::ForcePersistentCookies);
    }
    profile->installUrlSchemeHandler("day-resource",new ResourceHandler(0,profile));
    QObject::connect(profile->cookieStore(), &QWebEngineCookieStore::cookieAdded, profile, [state](const QNetworkCookie &cookie) {
        for (auto it=state->cookies.begin();it!=state->cookies.end();) { if(it->hasSameIdentifier(cookie))it=state->cookies.erase(it);else ++it; }
        state->cookies.append(cookie);
    });
    QObject::connect(profile->cookieStore(), &QWebEngineCookieStore::cookieRemoved, profile, [state](const QNetworkCookie &cookie) {
        for (auto it=state->cookies.begin();it!=state->cookies.end();) { if(it->hasSameIdentifier(cookie))it=state->cookies.erase(it);else ++it; }
    });
    profile->cookieStore()->loadAllCookies();
    return profile;
}
static ProfileState *profileState(const QString &key, const QString &directory, bool privateMode) {
    auto known=profiles.find(key);if(known!=profiles.end())return known->second;
    auto state=new ProfileState{key,directory,privateMode,nullptr,{}, {}};
    state->profile=makeProfile(state);profiles[key]=state;
    return state;
}
static ProfileState *profileState(QWebEngineProfile *profile) {
    for (auto &entry:profiles) if(entry.second->profile==profile)return entry.second;
    return nullptr;
}
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

class TabWebEngineView : public QWebEngineView {
public:
    void (*tabCb)(uint64_t, const char *, bool) = nullptr;
    uint64_t tabId = 0;
    QString foregroundLabel, backgroundLabel;
    void connectTabs() {
        QObject::disconnect(page(), &QWebEnginePage::newWindowRequested, this, nullptr);
        QObject::connect(page(), &QWebEnginePage::newWindowRequested, this,
            [this](QWebEngineNewWindowRequest &request) {
                if (!tabCb || !request.isUserInitiated() || request.requestedUrl().isEmpty()) return;
                auto url=request.requestedUrl().toEncoded();
                tabCb(tabId,url.constData(),request.destination()==QWebEngineNewWindowRequest::InNewBackgroundTab);
            });
    }
protected:
    void contextMenuEvent(QContextMenuEvent *event) override {
        if (!tabCb) { QWebEngineView::contextMenuEvent(event); return; }
        auto request = lastContextMenuRequest();
        auto menu = createStandardContextMenu();
        if (request && !request->linkUrl().isEmpty()) {
            const auto url = request->linkUrl().toEncoded();
            auto first = menu->actions().isEmpty() ? nullptr : menu->actions().first();
            auto foreground = new QAction(foregroundLabel, menu);
            auto background = new QAction(backgroundLabel, menu);
            QObject::connect(foreground, &QAction::triggered, this, [this,url] { tabCb(tabId,url.constData(),false); });
            QObject::connect(background, &QAction::triggered, this, [this,url] { tabCb(tabId,url.constData(),true); });
            menu->insertAction(first,foreground); menu->insertAction(first,background);
        }
        menu->exec(event->globalPos()); delete menu;
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
                      uint64_t session, const char *profile_key, const char *profile_directory, bool private_mode, const char *inline_path_prefix,
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

    QWebEngineView *v = new TabWebEngineView();
    const QString prefix = QString::fromUtf8(inline_path_prefix ? inline_path_prefix : "");
    auto state=profileState(QString::fromUtf8(profile_key),QString::fromUtf8(profile_directory),private_mode);
    state->views.insert(v);
    QObject::connect(v,&QObject::destroyed,[state,v]{state->views.remove(v);});
    auto page=new DayWebPage(state->profile,v);
    page->id=id;page->pathPrefix=prefix;page->linkCb=link_cb;
    page->settings()->setUnknownUrlSchemePolicy(QWebEngineSettings::AllowAllUnknownUrlSchemes);
    v->setPage(page);
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

void day_webview_tabs(void *w, const char *foreground, const char *background,
                      void (*callback)(uint64_t,const char *,bool)) {
    auto self=static_cast<DayWebView *>(w);
    auto view=dynamic_cast<TabWebEngineView *>(self->view);
    if (!view) return;
    view->tabCb=callback; view->tabId=self->id;
    view->foregroundLabel=QString::fromUtf8(foreground);
    view->backgroundLabel=QString::fromUtf8(background);
    view->connectTabs();
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
    const QByteArray request(script);
    if (request.startsWith("day-web-data:")) {
        auto data=QJsonDocument::fromJson(request.mid(13)).object();
        auto state=profileState(self->view->page()->profile());
        auto reply=[id,req](const QByteArray &json) {if(g_eval_cb){const auto payload=QByteArray("1\x1f")+json;g_eval_cb(id,req,payload.constData());}};
        if (!state) {if(g_eval_cb)g_eval_cb(id,req,day_webview_eval_error("profile unavailable").c_str());return;}
        const auto operation=data["operation"].toString();
        if(operation=="navigation") {
            QJsonObject info{{"url",self->view->url().toString()},{"title",self->view->title()},{"can_go_back",self->view->page()->history()->canGoBack()},{"can_go_forward",self->view->page()->history()->canGoForward()},{"loading",self->view->page()->isLoading()}};
            reply(QJsonDocument(info).toJson(QJsonDocument::Compact));return;
        }
        if (operation=="cookies") {
            const QUrl url(data["url"].toString());QStringList header;
            for (const auto &cookie:state->cookies) {
                const auto domain=cookie.domain().toLower();const auto bare=domain.startsWith('.')?domain.mid(1):domain;
                const auto path=cookie.path().isEmpty()?QString("/"):cookie.path();
                if ((url.host().toLower()==bare || (domain.startsWith('.') && url.host().toLower().endsWith("."+bare)))
                    && (!cookie.isSecure() || url.scheme()=="https")
                    && (url.path()==path || (url.path().startsWith(path) && (path.endsWith('/') || url.path().mid(path.size(),1)=="/")))
                    && (cookie.isSessionCookie() || cookie.expirationDate()>QDateTime::currentDateTimeUtc()))
                    header.append(QString::fromUtf8(cookie.name())+"="+QString::fromUtf8(cookie.value()));
            }
            auto json=QJsonDocument(QJsonArray{header.join("; ")}).toJson(QJsonDocument::Compact);reply(json.mid(1,json.size()-2));return;
        }
        if(operation=="set-cookie") {
            const auto cookies=QNetworkCookie::parseCookies(data["cookie"].toString().toUtf8());
            if(cookies.size()!=1){if(g_eval_cb)g_eval_cb(id,req,day_webview_eval_error("invalid Set-Cookie header").c_str());return;}
            const auto cookie=cookies.first();const QUrl origin(data["url"].toString());
            auto jar=state->profile->cookieStore();auto completed=std::make_shared<bool>(false);
            auto connections=std::make_shared<QList<QMetaObject::Connection>>();
            auto finish=[completed,connections,reply,id,req](bool accepted) {
                if(*completed)return;*completed=true;
                for(const auto &connection:*connections)QObject::disconnect(connection);
                if(accepted)reply("true");else if(g_eval_cb)g_eval_cb(id,req,day_webview_eval_error("cookie was not accepted").c_str());
            };
            connections->append(QObject::connect(jar,&QWebEngineCookieStore::cookieAdded,jar,[cookie,finish](const QNetworkCookie &added) {if(added.name()==cookie.name() && added.value()==cookie.value())finish(true);}));
            connections->append(QObject::connect(jar,&QWebEngineCookieStore::cookieRemoved,jar,[cookie,finish](const QNetworkCookie &removed) {if(removed.name()==cookie.name())finish(true);}));
            jar->setCookie(cookie,origin);
            QTimer::singleShot(3000,jar,[finish]{finish(false);});return;
        }
        if(operation=="clear") {
            // Qt has no public clear-all-site-storage API. Release EVERY page in this profile,
            // then its engine profile, before deleting its owned data/cache directories.
            struct PageInfo {QPointer<QWebEngineView> view;uint64_t id;QString prefix;void (*link)(uint64_t,const char *);};
            QList<PageInfo> pages;
            QList<QPointer<QWebEnginePage>> oldPages;
            for(auto v:state->views) {
                auto old=v->page();auto day=dynamic_cast<DayWebPage *>(old);
                pages.append({v,day?day->id:0,day?day->pathPrefix:QString(),day?day->linkCb:nullptr});
                oldPages.append(old);
            }
            const auto directory=state->profile->persistentStoragePath();const auto cache=state->profile->cachePath();
            auto finish=[state,pages,directory,cache,reply,id,req] {
                auto profile=state->profile;
                QObject::connect(profile,&QObject::destroyed,QCoreApplication::instance(),[state,pages,directory,cache,reply,id,req] {
                    // Let Chromium's profile destruction unwind before touching its disk files.
                    auto cleanup=new QTimer(QCoreApplication::instance());
                    auto attempts=std::make_shared<int>(0);
                    QObject::connect(cleanup,&QTimer::timeout,QCoreApplication::instance(),[state,pages,directory,cache,reply,id,req,cleanup,attempts] {
                        state->cookies.clear();bool removed=true;
                        if(!state->privateMode){removed=(!QDir(directory).exists() || QDir(directory).removeRecursively());removed=(!QDir(cache).exists() || QDir(cache).removeRecursively()) && removed; }
                        if(!removed && ++*attempts < 50)return;
                        cleanup->stop();cleanup->deleteLater();
                        state->profile=makeProfile(state);
                        for(const auto &info:pages)if(info.view){QPointer<QWebEnginePage> old=info.view->page();auto page=new DayWebPage(state->profile,info.view);page->id=info.id;page->pathPrefix=info.prefix;page->linkCb=info.link;page->settings()->setUnknownUrlSchemePolicy(QWebEngineSettings::AllowAllUnknownUrlSchemes);info.view->setPage(page);if(auto tabs=dynamic_cast<TabWebEngineView *>(info.view.data()))tabs->connectTabs();if(old)old->deleteLater();}
                        if(removed)reply("true");else if(g_eval_cb)g_eval_cb(id,req,day_webview_eval_error("could not remove profile data").c_str());
                    });
                    cleanup->start(100);
                });
                profile->deleteLater();
            };
            // setPage schedules owned pages for deletion. Wait for the last one instead of
            // deleting them twice or destroying their profile while a page still references it.
            auto pending=std::make_shared<int>(oldPages.size());
            for(auto old:oldPages)if(old)QObject::connect(old,&QObject::destroyed,QCoreApplication::instance(),[pending,finish] {if(--*pending==0)finish();});
            auto temporary=new QWebEngineProfile(QCoreApplication::instance());
            auto temporaryPages=std::make_shared<int>(pages.size());
            for(const auto &info:pages)if(info.view) {
                auto page=new QWebEnginePage(temporary,info.view);
                QObject::connect(page,&QObject::destroyed,QCoreApplication::instance(),[temporary,temporaryPages] {if(--*temporaryPages==0)temporary->deleteLater();});
                info.view->setPage(page);
            }
            for(auto old:oldPages)if(old)old->deleteLater();
            if(oldPages.isEmpty())finish();
            return;
        }
        if(g_eval_cb)g_eval_cb(id,req,day_webview_eval_error("unknown storage operation").c_str());return;
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
        // Use history directly: the view action can remain disabled after local navigation.
        v->history()->back();
}
void day_webview_forward(void *w) {
    if (QWebEngineView *v = static_cast<DayWebView *>(w)->view)
        v->history()->forward();
}
void day_webview_stop(void *w) {
    if (QWebEngineView *v = static_cast<DayWebView *>(w)->view)
        v->stop();
}
void day_webview_force_reload(void *w) {
    if (auto *v = static_cast<DayWebView *>(w)->view)
        v->page()->triggerAction(QWebEnginePage::ReloadAndBypassCache);
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
                      uint64_t session, const char *profile_key, const char *profile_directory, bool private_mode, const char *inline_path_prefix,
                      void (*link_cb)(uint64_t, const char *)) {
    (void)cb;                 // no navigation to report without a real engine
    (void)profile_key; (void)profile_directory; (void)private_mode;
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
void day_webview_force_reload(void *) {}

void day_webview_tabs(void *, const char *, const char *, void (*)(uint64_t,const char *,bool)) {}
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
