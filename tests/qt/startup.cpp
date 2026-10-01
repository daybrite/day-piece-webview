// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// The Windows Qt host's startup lifecycle (docs/platforms.md): the QWidget in
// src/lib-qt-windows.inc defers WebView2 creation past showEvent to the next Qt turn, starts the
// engine once with the latest URL, and reconciles a close that lands before or during that
// deferred startup. The widget is compiled here as-is against a fake `day_webview_win_*` bridge
// that records every call, under the offscreen platform, so none of this needs Windows.

#include <QApplication>
#include <QEventLoop>
#include <QLabel>
#include <QTimer>
#include <QUrl>
#include <QVBoxLayout>
#include <QWidget>

#include <cstdint>
#include <functional>
#include <iostream>
#include <string>
#include <vector>

// lib-qt-shim.cpp defines this before including the Windows host; the host's
// day_webview_set_eval_cb stores into it.
static void (*g_eval_cb)(uint64_t, uint64_t, const char *) = nullptr;

#include "../../src/lib-qt-windows.inc"

// ---------------------------------------------------------------------------------------------
// The fake bridge: one record per call, in order, plus a hook that runs inside `create` to model
// what the real one can do: pump Win32 messages, and so deliver a close while initializing.
// ---------------------------------------------------------------------------------------------

struct Call {
    std::string kind; // create / bounds / release / command
    uint64_t node = 0;
    std::string text;
    double w = 0, h = 0;
};
static std::vector<Call> calls;
static std::function<void()> during_create;
static bool create_result = true;

extern "C" {
bool day_webview_win_create(intptr_t hwnd, uint64_t node, const char *url, const char *) {
    calls.push_back({"create", node, url ? url : "", hwnd != 0 ? 1.0 : 0.0, 0});
    if (during_create) {
        auto hook = std::move(during_create);
        during_create = nullptr;
        hook();
    }
    return create_result;
}
void day_webview_win_bounds(uint64_t node, double w, double h) {
    calls.push_back({"bounds", node, "", w, h});
}
void day_webview_win_release(uint64_t node) { calls.push_back({"release", node, "", 0, 0}); }
void day_webview_win_command(uint64_t node, uint32_t cmd, uint64_t, const char *arg) {
    calls.push_back({"command", node, std::to_string(cmd) + ":" + (arg ? arg : ""), 0, 0});
}
}

static int count(const std::string &kind) {
    int n = 0;
    for (const auto &c : calls)
        n += c.kind == kind;
    return n;
}
static const Call *first(const std::string &kind) {
    for (const auto &c : calls)
        if (c.kind == kind)
            return &c;
    return nullptr;
}
static int index_of(const std::string &kind) {
    for (size_t i = 0; i < calls.size(); i++)
        if (calls[i].kind == kind)
            return int(i);
    return -1;
}

/// Run the event loop for a moment: long enough for a zero-delay single shot to fire.
static void spin(int ms = 50) {
    QEventLoop loop;
    QTimer::singleShot(ms, &loop, &QEventLoop::quit);
    loop.exec();
}

static DayWebView *make(uint64_t id, const char *url) {
    // An empty prefix skips the bundled-asset extraction; the host only has an engine to start.
    return static_cast<DayWebView *>(day_webview_new(url, id, nullptr, 0, "", nullptr));
}

static int failures = 0;
static void check(bool ok, const char *what) {
    if (!ok) {
        failures++;
        std::cerr << "FAIL: " << what << "\n";
        for (const auto &c : calls)
            std::cerr << "  " << c.kind << " node=" << c.node << " " << c.text << " " << c.w
                      << "x" << c.h << "\n";
    }
}

// Showing the widget schedules the engine for the next turn: nothing is created inside
// showEvent, and the create carries the native window plus a bounds update after it.
static void deferred_startup() {
    calls.clear();
    auto *v = make(1, "https://example.invalid/a");
    v->resize(320, 240);
    v->show();
    check(count("create") == 0, "showEvent must not create the engine synchronously");
    check(count("bounds") >= 1, "showEvent forwards the bounds right away");
    spin();
    check(count("create") == 1, "the deferred startup creates the engine once");
    const Call *c = first("create");
    check(c && c->node == 1 && c->text == "https://example.invalid/a" && c->w == 1.0,
          "the engine is created for this node, URL and native window");
    int i = index_of("create");
    bool after = false;
    for (size_t k = i + 1; k < calls.size(); k++)
        after |= calls[k].kind == "bounds" && calls[k].w == 320 && calls[k].h == 240;
    check(after, "the bounds are sent again once the engine exists");
    delete v;
    check(calls.back().kind == "release" && calls.back().node == 1,
          "closing the started widget releases its engine");
}

// A navigation that lands between showEvent and the deferred turn: the engine starts on the
// latest URL, and the load command still reaches the bridge for a browser already running.
static void latest_url() {
    calls.clear();
    auto *v = make(2, "https://example.invalid/first");
    v->show();
    day_webview_load(v, "https://example.invalid/second");
    spin();
    const Call *c = first("create");
    check(c && c->text == "https://example.invalid/second", "startup uses the latest URL");
    check(count("command") == 1 && first("command")->text == "0:https://example.invalid/second",
          "the load command is forwarded as well");
    delete v;
}

// Show/hide/show before the turn, and a later re-show, start the engine exactly once.
static void single_initialization() {
    calls.clear();
    auto *v = make(3, "https://example.invalid/once");
    v->show();
    v->hide();
    v->show();
    spin();
    check(count("create") == 1, "repeated shows before the turn create the engine once");
    v->hide();
    v->show();
    spin();
    check(count("create") == 1, "a later re-show does not create a second engine");
    delete v;
}

// Closed before its turn: the queued startup is cancelled; the bridge sees only the release.
static void closed_before_startup() {
    calls.clear();
    auto *v = make(4, "https://example.invalid/early");
    v->show();
    delete v;
    spin();
    check(count("create") == 0, "a widget closed before its turn never starts the engine");
    check(count("release") == 1 && first("release")->node == 4,
          "the release still reaches the bridge for the node");
}

// Closed while initializing (the bridge's nested loop delivered the close): the engine that
// creation just produced is released after creation returns, and nothing touches the widget.
static void closed_during_startup() {
    calls.clear();
    DayWebView *v = make(5, "https://example.invalid/during");
    v->show();
    during_create = [&] { delete v; };
    spin();
    check(count("create") == 1, "the engine was created inside the nested loop");
    int c = index_of("create");
    bool released_after = false, bounds_after = false;
    for (size_t k = c + 1; k < calls.size(); k++) {
        released_after |= calls[k].kind == "release" && calls[k].node == 5;
        bounds_after |= calls[k].kind == "bounds";
    }
    check(released_after, "the newly created engine is released after creation returns");
    check(!bounds_after, "a released widget gets no bounds update");
}

// A failed creation still finishes the turn: the placeholder label appears and the bounds go
// out, so a missing runtime is visible rather than a hang.
static void failed_startup() {
    calls.clear();
    create_result = false;
    auto *v = make(6, "https://example.invalid/none");
    v->show();
    spin();
    create_result = true;
    check(count("create") == 1, "a failing bridge is asked once");
    check(v->findChild<QLabel *>() != nullptr, "the failure shows the runtime hint");
    delete v;
}

int main(int argc, char **argv) {
    QApplication app(argc, argv);
    deferred_startup();
    latest_url();
    single_initialization();
    closed_before_startup();
    closed_during_startup();
    failed_startup();
    if (failures) {
        std::cerr << failures << " Qt startup lifecycle check(s) failed\n";
        return 1;
    }
    return 0;
}
