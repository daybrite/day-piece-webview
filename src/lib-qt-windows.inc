// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
// MSYS2 has no Qt WebEngine. Use WebView2 in a native QWidget child instead.
#include <QDirIterator>
#include <QFile>
#include <QFileInfo>
#include <QLabel>
#include <QPointer>
#include <QResizeEvent>
#include <QShowEvent>
#include <QTemporaryDir>
#include <QTimer>
extern "C" {
bool day_webview_win_create(intptr_t, uint64_t, const char *, const char *);
void day_webview_win_bounds(uint64_t, double, double);
void day_webview_win_release(uint64_t);
void day_webview_win_command(uint64_t, uint32_t, uint64_t, const char *);
}
class DayWebView : public QWidget {
public:
    uint64_t id;
    QString url, prefix;
    QTemporaryDir assets;
    bool started = false;
    DayWebView(uint64_t node, QString address, QString root) : id(node), url(address), prefix(root) {
        setAttribute(Qt::WA_NativeWindow);
        if (!root.isEmpty() && !root.startsWith("http://day-resource.")) {
            QDirIterator it(":/day/assets", QDir::Files, QDirIterator::Subdirectories);
            while (it.hasNext()) {
                QString source=it.next(), dest=assets.path()+source.mid(QString(":/day/assets").size());
                QDir().mkpath(QFileInfo(dest).absolutePath());
                QFile::copy(source,dest);
            }
            url=QUrl::fromLocalFile(assets.path()+QUrl(address).path().mid(QString("/day/assets").size())).toString();
            prefix=QUrl::fromLocalFile(assets.path()+root.mid(QString("/day/assets").size())).toString();
        }
    }
    ~DayWebView() override { day_webview_win_release(id); }
    void showEvent(QShowEvent *e) override {
        QWidget::showEvent(e);
        if (!started) {
            started=true;
            // Wry waits for WebView2 initialization by pumping Win32 messages. Never enter that
            // nested loop from showEvent: Day may still be realizing/reparenting this subtree.
            // The context cancels startup if the page is removed before the next Qt turn.
            QTimer::singleShot(0, this, [this] {
                const auto node = id;
                QPointer<DayWebView> alive(this);
                const auto address = url.toUtf8(), root = prefix.toUtf8();
                const bool ok = day_webview_win_create(static_cast<intptr_t>(winId()), node,
                                                       address.constData(), root.constData());
                // The nested initialization loop can also deliver a close/navigation event.
                if (!alive) {
                    day_webview_win_release(node);
                    return;
                }
                if (!ok) {
                    auto *layout=new QVBoxLayout(this);
                    layout->addWidget(new QLabel("WebView2 could not start. Install Microsoft Edge WebView2 Runtime.",this));
                }
                day_webview_win_bounds(node, width(), height());
            });
        }
        day_webview_win_bounds(id,width(),height());
    }
    void resizeEvent(QResizeEvent *e) override { QWidget::resizeEvent(e);day_webview_win_bounds(id,width(),height()); }
};
extern "C" {
void *day_webview_new(const char *url,uint64_t id,void (*)(uint64_t,const char *),uint64_t,const char *prefix,void (*)(uint64_t,const char *)) {
    return new DayWebView(id,QString::fromUtf8(url),QString::fromUtf8(prefix));
}
void day_webview_set_transparent(void *) {}
void day_webview_set_eval_cb(void (*cb)(uint64_t,uint64_t,const char *)) {g_eval_cb=cb;}
void day_webview_load(void *w,const char *url) {auto *v=static_cast<DayWebView *>(w);v->url=QString::fromUtf8(url);day_webview_win_command(v->id,0,0,url);}
void day_webview_back(void *w) {day_webview_win_command(static_cast<DayWebView *>(w)->id,1,0,"");}
void day_webview_forward(void *w) {day_webview_win_command(static_cast<DayWebView *>(w)->id,2,0,"");}
void day_webview_stop(void *w) {day_webview_win_command(static_cast<DayWebView *>(w)->id,3,0,"");}
void day_webview_reload(void *w) {day_webview_win_command(static_cast<DayWebView *>(w)->id,4,0,"");}
void day_webview_eval(void *w,uint64_t req,const char *script) {day_webview_win_command(static_cast<DayWebView *>(w)->id,5,req,script);}
}
