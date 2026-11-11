// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! What a page asks of the app around it, and what the engine tells the app about a page: script
//! dialogs, HTTP sign-in, file choosing, downloads, failed loads and a lost page process
//! (docs/webview.md "Browser duties").
//!
//! A backend reports a question on the node's `Event::Custom` channel (`PAGE_PROMPT`, a JSON
//! [`PromptReport`]) and holds the engine's callback until the answer returns as
//! [`crate::WebPatch::PromptAnswer`]. One-way reports (`PAGE_NOTICE`, a JSON [`Notice`]) need no
//! answer. A download outlives the view that started it, so its progress does not use the node
//! at all: backends call [`download_event`] and the app's callback is kept here by id.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

/// The `num` that tags a question a page or the engine asks: a JSON [`PromptReport`].
pub(crate) const PAGE_PROMPT: f64 = -11.0;
/// The `num` that tags a one-way report: a JSON [`Notice`].
pub(crate) const PAGE_NOTICE: f64 = -12.0;

// ---------------------------------------------------------------------------
// Reports, as backends compose them
// ---------------------------------------------------------------------------

/// A question from a backend. Every arm carries the `id` its answer is keyed by.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum PromptReport {
    /// `alert()`, `confirm()` or `prompt()`.
    Dialog {
        id: u64,
        dialog: DialogKind,
        #[serde(default)]
        message: String,
        #[serde(default)]
        default_text: String,
        /// The address of the frame that asked.
        #[serde(default)]
        url: String,
    },
    /// An HTTP Basic, Digest or NTLM challenge.
    Auth {
        id: u64,
        #[serde(default)]
        host: String,
        #[serde(default)]
        realm: String,
        #[serde(default)]
        proxy: bool,
        #[serde(default)]
        failures: u32,
    },
    /// `<input type=file>` on an engine with no chooser of its own (Android).
    Files {
        id: u64,
        #[serde(default)]
        accept: Vec<String>,
        #[serde(default)]
        multiple: bool,
    },
    /// A response the engine will not show, or a link marked `download`.
    Download {
        id: u64,
        #[serde(default)]
        url: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        mime: String,
        /// The length the server announced, or a negative number for none.
        #[serde(default = "unknown_size")]
        size: i64,
    },
}

fn unknown_size() -> i64 {
    -1
}

/// A one-way report from a backend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum Notice {
    LoadError {
        #[serde(default)]
        url: String,
        error: LoadErrorKind,
        #[serde(default)]
        code: i64,
        #[serde(default)]
        text: String,
    },
    RendererGone {
        #[serde(default)]
        crashed: bool,
    },
}

/// A prompt as an event, for the arms that emit from Rust.
#[allow(dead_code)]
pub(crate) fn prompt_event(report: &PromptReport) -> day_spec::Event {
    day_spec::Event::Custom {
        tag: "webview:prompt",
        num: PAGE_PROMPT,
        text: serde_json::to_string(report).unwrap_or_default(),
    }
}

/// A notice as an event, for the arms that emit from Rust.
#[allow(dead_code)]
pub(crate) fn notice_event(notice: &Notice) -> day_spec::Event {
    day_spec::Event::Custom {
        tag: "webview:notice",
        num: PAGE_NOTICE,
        text: serde_json::to_string(notice).unwrap_or_default(),
    }
}

/// The answer to a prompt, as it crosses to a backend in [`crate::WebPatch::PromptAnswer`].
/// `ok: false` cancels whatever was asked; the other fields are the arm's own.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Answer {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// A download goes to the user's Downloads under this name, where the backend keeps that
    /// collection itself (Android).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Answer {
    /// Read an answer a backend received; anything unreadable cancels.
    #[allow(dead_code)]
    pub(crate) fn parse(json: &str) -> Answer {
        serde_json::from_str(json).unwrap_or_default()
    }
}

/// Sends one answer, once. Dropped unanswered, it cancels: an engine waits on every question it
/// asks, and a page whose `confirm()` is never answered stays frozen.
struct Reply {
    id: u64,
    send: Option<Box<dyn FnOnce(u64, Answer)>>,
}

impl Reply {
    fn answer(&mut self, answer: Answer) {
        if let Some(send) = self.send.take() {
            send(self.id, answer);
        }
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        self.answer(Answer::default());
    }
}

// ---------------------------------------------------------------------------
// Script dialogs
// ---------------------------------------------------------------------------

/// Which of the three script dialogs a page opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DialogKind {
    /// `alert(message)`: one button.
    Alert,
    /// `confirm(message)`: accept or dismiss.
    Confirm,
    /// `prompt(message, default)`: a line of text, or dismiss.
    Prompt,
}

/// A script dialog a page opened ([`crate::WebView::on_dialog`]). The page's script waits until
/// the [`DialogResponder`] answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogRequest {
    pub kind: DialogKind,
    /// The text the page passed.
    pub message: String,
    /// `prompt()`'s second argument; empty for the other two.
    pub default_text: String,
    /// The address of the frame that asked, so the dialog can say which site is speaking.
    pub url: String,
}

impl DialogRequest {
    /// The host of the frame that asked, or an empty string for a page with none.
    pub fn host(&self) -> String {
        url::Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

/// Answers a [`DialogRequest`]. Dropping it dismisses the dialog.
pub struct DialogResponder(Reply);

impl DialogResponder {
    /// OK: `alert()` returns, `confirm()` is `true`, `prompt()` gives back its default text.
    pub fn accept(mut self) {
        self.0.answer(Answer {
            ok: true,
            ..Answer::default()
        });
    }
    /// OK with the text typed into a `prompt()`.
    pub fn accept_with(mut self, text: impl Into<String>) {
        self.0.answer(Answer {
            ok: true,
            text: Some(text.into()),
            ..Answer::default()
        });
    }
    /// Cancel: `confirm()` is `false`, `prompt()` is `null`.
    pub fn dismiss(mut self) {
        self.0.answer(Answer::default());
    }
}

pub(crate) type DialogHandler = Rc<dyn Fn(DialogRequest, DialogResponder)>;

/// Present a script dialog as Day's own alert, confirm or prompt, titled with the site that
/// asked, and answer the page with what the user chose. It is what a view does when the app
/// set no [`crate::WebView::on_dialog`]; an app's handler calls it after changing the request
/// (the address a dialog names, say) or deciding not to suppress it.
pub fn present_dialog(request: DialogRequest, responder: DialogResponder) {
    let host = request.host();
    // A dialog's title is the site and its message the page's text. A page with no host (a
    // bundled page) has only its text to show.
    let (title, message) = if host.is_empty() {
        (request.message.clone(), None)
    } else {
        (host, Some(request.message.clone()))
    };
    day_core::task(async move {
        match request.kind {
            DialogKind::Alert => {
                let mut dialog = day_pieces::alert(title);
                if let Some(message) = message {
                    dialog = dialog.message(message);
                }
                dialog.present().await;
                responder.accept();
            }
            DialogKind::Confirm => {
                let mut dialog = day_pieces::confirm(title);
                if let Some(message) = message {
                    dialog = dialog.message(message);
                }
                if dialog.present().await {
                    responder.accept();
                } else {
                    responder.dismiss();
                }
            }
            DialogKind::Prompt => {
                let mut dialog = day_pieces::prompt(title).initial(request.default_text.clone());
                if let Some(message) = message {
                    dialog = dialog.message(message);
                }
                match dialog.await {
                    Some(text) => responder.accept_with(text),
                    None => responder.dismiss(),
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// HTTP sign-in
// ---------------------------------------------------------------------------

/// A server or proxy asking for a name and password ([`crate::WebView::on_auth`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthRequest {
    /// The server, or the proxy when [`AuthRequest::proxy`] is set.
    pub host: String,
    /// The realm the server named, which may be empty.
    pub realm: String,
    pub proxy: bool,
    /// How many answers to this challenge the server has refused so far.
    pub failures: u32,
}

/// Answers an [`AuthRequest`]. Dropping it cancels the sign-in, and the server's own
/// "unauthorized" page loads.
pub struct AuthResponder(Reply);

impl AuthResponder {
    pub fn sign_in(mut self, user: impl Into<String>, password: impl Into<String>) {
        self.0.answer(Answer {
            ok: true,
            user: Some(user.into()),
            password: Some(password.into()),
            ..Answer::default()
        });
    }
    pub fn cancel(mut self) {
        self.0.answer(Answer::default());
    }
}

pub(crate) type AuthHandler = Rc<dyn Fn(AuthRequest, AuthResponder)>;

// ---------------------------------------------------------------------------
// File choosing (Android; the other engines show the system's chooser themselves)
// ---------------------------------------------------------------------------

/// The extensions an `<input accept>` list names, for the picker's filter: `.pdf` as given, and
/// the common extensions behind `image/*`, `video/*`, `audio/*` and a few exact types. An empty
/// answer means "any file".
pub(crate) fn accept_extensions(accept: &[String]) -> Vec<&'static str> {
    const TYPES: &[(&str, &[&str])] = &[
        (
            "image/*",
            &["png", "jpg", "jpeg", "gif", "webp", "heic", "bmp", "svg"],
        ),
        ("video/*", &["mp4", "mov", "m4v", "webm", "mkv", "3gp"]),
        (
            "audio/*",
            &["mp3", "m4a", "aac", "wav", "ogg", "flac", "opus"],
        ),
        ("image/png", &["png"]),
        ("image/jpeg", &["jpg", "jpeg"]),
        ("image/gif", &["gif"]),
        ("image/webp", &["webp"]),
        ("application/pdf", &["pdf"]),
        ("text/plain", &["txt"]),
        ("text/csv", &["csv"]),
        ("application/json", &["json"]),
        ("application/zip", &["zip"]),
    ];
    const KNOWN: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "webp", "heic", "bmp", "svg", "mp4", "mov", "m4v", "webm",
        "mkv", "3gp", "mp3", "m4a", "aac", "wav", "ogg", "flac", "opus", "pdf", "txt", "csv",
        "json", "zip", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "html", "xml", "md", "rtf",
        "epub",
    ];
    let mut out: Vec<&'static str> = Vec::new();
    for entry in accept
        .iter()
        .flat_map(|a| a.split(','))
        .map(|a| a.trim().to_ascii_lowercase())
    {
        if entry.is_empty() {
            continue;
        }
        let found: &[&'static str] = match entry.strip_prefix('.') {
            Some(ext) => match KNOWN.iter().find(|k| **k == ext) {
                Some(known) => std::slice::from_ref(known),
                // An extension this table does not know cannot be named statically: any file.
                None => return Vec::new(),
            },
            None => match TYPES.iter().find(|(mime, _)| *mime == entry) {
                Some((_, extensions)) => extensions,
                None => return Vec::new(),
            },
        };
        for ext in found {
            if !out.contains(ext) {
                out.push(ext);
            }
        }
    }
    out
}

/// Show Day's file picker for a page's `<input type=file>` and answer with what was picked.
fn choose_files(accept: Vec<String>, multiple: bool, mut reply: Reply) {
    let extensions = accept_extensions(&accept);
    day_core::task(async move {
        let picked: Vec<day_pieces::FileUrl> = if multiple {
            let mut picker = day_pieces::open_files();
            if !extensions.is_empty() {
                picker = picker.filter(accept.join(", "), &extensions);
            }
            picker.await
        } else {
            let mut picker = day_pieces::open_file();
            if !extensions.is_empty() {
                picker = picker.filter(accept.join(", "), &extensions);
            }
            picker.await.into_iter().collect()
        };
        let paths: Vec<String> = picked
            .iter()
            .filter_map(|file| file.local_path())
            .map(|path| path.display().to_string())
            .collect();
        reply.answer(Answer {
            ok: !paths.is_empty(),
            paths,
            ..Answer::default()
        });
    });
}

// ---------------------------------------------------------------------------
// Downloads
// ---------------------------------------------------------------------------

/// A file the engine will not show and offers to save instead ([`crate::WebView::on_download`]):
/// a response with a type no page can display, one the server marked as an attachment, or a link
/// with the `download` attribute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadRequest {
    /// Identifies the download in [`cancel_download`].
    pub id: u64,
    pub url: String,
    /// The file name the server or the link suggests. It is a name only, never a path, and safe
    /// to join onto a directory.
    pub suggested_name: String,
    /// The MIME type the server gave, which may be empty.
    pub mime: String,
    /// The length the server announced.
    pub size: Option<u64>,
}

/// How a download is going, as its callback hears it ([`DownloadResponder::save_to`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadEvent {
    /// Bytes written so far, and the total where the server announced one.
    Progress { received: u64, total: Option<u64> },
    /// The whole file is saved. `location` is where: the path it was saved to, or on Android
    /// a `content://` address in the system's Downloads collection. It is what
    /// [`open_download`] and [`remove_download`] take.
    Finished { location: String },
    /// The download stopped, with the engine's description of why. A canceled download ends
    /// here too. The partial file is removed.
    Failed(String),
}

impl DownloadEvent {
    /// Whether this is the last event of its download.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, DownloadEvent::Progress { .. })
    }
}

/// Answers a [`DownloadRequest`]. Dropping it declines the download.
pub struct DownloadResponder(Reply);

impl DownloadResponder {
    /// Save the file at `path`, whose directory exists and which names no existing file, and
    /// hear how it goes. `on_event` runs on the main thread until a terminal event, also after
    /// the view that started the download is gone.
    pub fn save_to(mut self, path: impl AsRef<Path>, on_event: impl Fn(&DownloadEvent) + 'static) {
        DOWNLOADS.with(|d| d.borrow_mut().insert(self.0.id, Rc::new(on_event)));
        self.0.answer(Answer {
            ok: true,
            paths: vec![path.as_ref().display().to_string()],
            ..Answer::default()
        });
    }
    /// Save the file where the user looks for downloads, under `name` or the next free variant
    /// of it (`report (2).pdf`): the system's Downloads collection on Android, the app's folder
    /// in the Files app on iOS (which shows it once the app's `Info.plist` sets
    /// `UIFileSharingEnabled` and `LSSupportsOpeningDocumentsInPlace`), and the user's Downloads
    /// folder elsewhere. [`DownloadEvent::Finished`] says where it went.
    pub fn save_to_downloads(mut self, name: &str, on_event: impl Fn(&DownloadEvent) + 'static) {
        let name = sanitize_file_name(name, "download");
        if cfg!(target_os = "android") {
            DOWNLOADS.with(|d| d.borrow_mut().insert(self.0.id, Rc::new(on_event)));
            self.0.answer(Answer {
                ok: true,
                name: Some(name),
                ..Answer::default()
            });
            return;
        }
        match downloads_dir() {
            Some(dir) if std::fs::create_dir_all(&dir).is_ok() => {
                self.save_to(unused_path(&dir, &name), on_event)
            }
            _ => {
                on_event(&DownloadEvent::Failed("no Downloads folder".into()));
                self.cancel();
            }
        }
    }
    pub fn cancel(mut self) {
        self.0.answer(Answer::default());
    }
}

/// The folder [`DownloadResponder::save_to_downloads`] saves into outside Android: `Downloads`
/// in the app's Documents on iOS, the user's own Downloads folder on a desktop.
fn downloads_dir() -> Option<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    let home = PathBuf::from(home);
    Some(if cfg!(target_os = "ios") {
        home.join("Documents").join("Downloads")
    } else {
        home.join("Downloads")
    })
}

/// Show a finished download ([`DownloadEvent::Finished`]'s `location`) in whatever the system
/// opens it with: the app that handles its type on Android, the share sheet on iOS (which
/// previews, saves and sends), the default application on a desktop. `false` when nothing
/// could be asked to open it.
pub fn open_download(location: &str) -> bool {
    #[cfg(all(target_os = "android", feature = "material"))]
    return crate::android_impl::open_download(location);
    #[cfg(not(all(target_os = "android", feature = "material")))]
    {
        let Ok(url) = url::Url::from_file_path(location) else {
            return false;
        };
        if cfg!(target_os = "ios") {
            day_pieces::Share::url(url.to_string()).present()
        } else {
            day_core::open_url(url.as_str());
            true
        }
    }
}

/// Delete a finished download from where it was saved.
pub fn remove_download(location: &str) {
    #[cfg(all(target_os = "android", feature = "material"))]
    crate::android_impl::remove_download(location);
    #[cfg(not(all(target_os = "android", feature = "material")))]
    let _ = std::fs::remove_file(location);
}

pub(crate) type DownloadHandler = Rc<dyn Fn(DownloadRequest, DownloadResponder)>;

/// What hears a download's events ([`DownloadResponder::save_to`]).
type DownloadCallback = Rc<dyn Fn(&DownloadEvent)>;

thread_local! {
    /// The callback of each download in flight, by id.
    static DOWNLOADS: RefCell<HashMap<u64, DownloadCallback>> = RefCell::new(HashMap::new());
}

/// A download's progress, from its backend, on the main thread.
#[allow(dead_code)]
pub(crate) fn download_event(id: u64, event: DownloadEvent) {
    let callback = DOWNLOADS.with(|d| {
        if event.is_terminal() {
            d.borrow_mut().remove(&id)
        } else {
            d.borrow().get(&id).cloned()
        }
    });
    if let Some(callback) = callback {
        callback(&event);
    }
}

/// Stop a download in flight. Its callback hears [`DownloadEvent::Failed`]. Nothing happens for
/// an id that is not downloading.
pub fn cancel_download(id: u64) {
    if !DOWNLOADS.with(|d| d.borrow().contains_key(&id)) {
        return;
    }
    #[cfg(any(
        all(feature = "uikit", target_os = "ios"),
        all(any(feature = "appkit", feature = "gtk"), target_os = "macos")
    ))]
    crate::browsing_apple::cancel_download(id);
    #[cfg(all(target_os = "android", feature = "material"))]
    crate::android_impl::cancel_download(id);
    #[cfg(not(any(
        all(feature = "uikit", target_os = "ios"),
        all(any(feature = "appkit", feature = "gtk"), target_os = "macos"),
        all(target_os = "android", feature = "material")
    )))]
    let _ = id;
}

/// Whether [`crate::WebView::on_download`] is ever asked on this backend.
pub fn download_support() -> day_spec::Support {
    if cfg!(any(
        all(feature = "uikit", target_os = "ios"),
        all(
            any(feature = "appkit", feature = "gtk"),
            target_os = "macos"
        ),
        all(target_os = "android", feature = "material")
    )) {
        day_spec::Support::Native
    } else {
        day_spec::Support::Unsupported
    }
}

/// A file name safe to join onto a directory: the last path component of what the server or the
/// link suggested, without control characters or the characters a file system refuses, and
/// `fallback` for a name with nothing left.
pub fn sanitize_file_name(suggested: &str, fallback: &str) -> String {
    let last = suggested
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim();
    let clean: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let clean = clean.trim_matches(['.', ' ']).to_string();
    if clean.is_empty() {
        fallback.to_string()
    } else {
        clean
    }
}

/// `dir/name`, or `dir/stem (2).ext` and so on for the first name not taken: where a browser
/// saves a second file of the same name.
pub fn unused_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (2u32..)
        .map(|n| dir.join(format!("{stem} ({n}){extension}")))
        .find(|candidate| !candidate.exists())
        .unwrap_or(first)
}

// ---------------------------------------------------------------------------
// Failed loads and certificates
// ---------------------------------------------------------------------------

/// Why a page did not load, in the terms an error page explains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoadErrorKind {
    /// The device has no connection.
    Offline,
    /// No server has that name.
    HostNotFound,
    /// The server did not answer in time.
    Timeout,
    /// The server refused or dropped the connection.
    Connection,
    /// The server's certificate is not trusted, expired, or made out to another name. The page
    /// loads after [`trust_host`] and a reload.
    Certificate,
    /// The connection could not be made secure for a reason other than the certificate.
    Secure,
    /// The address names a scheme the engine cannot load.
    Unsupported,
    /// Anything else; [`LoadError::description`] has the engine's words.
    Other,
}

/// A main-frame load that failed ([`crate::WebView::on_load_error`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadError {
    /// The address that failed.
    pub url: String,
    pub kind: LoadErrorKind,
    /// The engine's own error number, for logs.
    pub code: i64,
    /// The engine's description, in the system's language where the engine localizes it.
    pub description: String,
}

impl LoadError {
    /// The host of the address that failed, which is what [`trust_host`] takes.
    pub fn host(&self) -> String {
        url::Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

pub(crate) type LoadErrorHandler = Rc<dyn Fn(&LoadError)>;

/// The hosts [`trust_host`] excepted. Engines ask about a certificate on threads of their own.
static TRUSTED_HOSTS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Load pages from `host` despite a certificate the system does not trust, until the app exits:
/// what "visit this site anyway" does after a [`LoadErrorKind::Certificate`] error. Reload the
/// page afterwards. The exception covers every view and is never stored.
pub fn trust_host(host: &str) {
    let host = host.to_ascii_lowercase();
    if host.is_empty() || host_trusted(&host) {
        return;
    }
    if let Ok(mut trusted) = TRUSTED_HOSTS.lock() {
        trusted.push(host.clone());
    }
    #[cfg(all(target_os = "android", feature = "material"))]
    crate::android_impl::trust_host(&host);
}

/// Whether [`trust_host`] was called for `host`.
pub fn host_trusted(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    TRUSTED_HOSTS
        .lock()
        .is_ok_and(|trusted| trusted.contains(&host))
}

// ---------------------------------------------------------------------------
// The user agent
// ---------------------------------------------------------------------------

/// What a view tells sites it is ([`crate::WebView::user_agent`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UserAgent {
    /// The engine's own string, which marks the view as embedded in an app: Android's has
    /// `; wv` and `Version/4.0`, and WebKit's lacks Safari's `Version/… Safari/…`. Sites act on
    /// that (some refuse sign-in to embedded views, some push their own app).
    #[default]
    Engine,
    /// The string the platform's own browser sends, built from the engine's: Chrome's on
    /// Android, Safari's on iOS and macOS. What a browser built on the system engine sends.
    Browser,
    /// This exact string.
    Custom(String),
}

impl UserAgent {
    /// The form a backend receives at creation: empty for the engine's own, `\u{1}` for the
    /// platform browser's, or the custom string.
    #[allow(dead_code)]
    pub(crate) fn wire(&self) -> &str {
        match self {
            UserAgent::Engine => "",
            UserAgent::Browser => "\u{1}",
            UserAgent::Custom(text) => text.trim(),
        }
    }
}

/// [`UserAgent::Browser`] on WebKit: what follows `(KHTML, like Gecko)` in Safari's string,
/// for `WKWebViewConfiguration.applicationNameForUserAgent`. WebKit writes everything before
/// it, so the device and its content mode stay the engine's to describe. `version` is the
/// system's `major.minor`.
#[allow(dead_code)]
pub(crate) fn safari_application_name(version: &str, mobile: bool) -> String {
    if mobile {
        format!("Version/{version} Mobile/15E148 Safari/604.1")
    } else {
        format!("Version/{version} Safari/605.1.15")
    }
}

// ---------------------------------------------------------------------------
// A lost page process
// ---------------------------------------------------------------------------

/// The process that ran a view's page is gone ([`crate::WebView::on_renderer_gone`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RendererGone {
    /// The process crashed. `false` means the system took it back for memory.
    pub crashed: bool,
}

pub(crate) type RendererGoneHandler = Rc<dyn Fn(&RendererGone)>;

// ---------------------------------------------------------------------------
// The front-end's half: what a view's handlers do with a report
// ---------------------------------------------------------------------------

/// A view's handlers for this module's reports.
#[derive(Clone, Default)]
pub(crate) struct Handlers {
    pub dialog: Option<DialogHandler>,
    pub auth: Option<AuthHandler>,
    pub download: Option<DownloadHandler>,
    pub load_error: Option<LoadErrorHandler>,
    pub renderer_gone: Option<RendererGoneHandler>,
}

/// What a backend needs to know of them before the first load.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BrowseProps {
    /// The app answers HTTP sign-in ([`crate::WebView::on_auth`]); without it the engine's
    /// default applies, which cancels.
    pub auth: bool,
    /// The app takes downloads ([`crate::WebView::on_download`]); without it the engine does
    /// what it does with a file it cannot show.
    pub downloads: bool,
    /// The app rebuilds a view whose page process is gone ([`crate::WebView::on_renderer_gone`]);
    /// without it WebKit reloads the page itself.
    pub renderer_gone: bool,
    /// Swiping from a screen edge goes back and forward ([`crate::WebView::swipe_navigation`]).
    pub swipe_navigation: bool,
}

impl Handlers {
    pub(crate) fn props(&self, swipe_navigation: bool) -> BrowseProps {
        BrowseProps {
            auth: self.auth.is_some(),
            downloads: self.download.is_some(),
            renderer_gone: self.renderer_gone.is_some(),
            swipe_navigation,
        }
    }

    /// Run the handler a prompt is for. `send` carries the answer back to the backend.
    pub(crate) fn prompt(&self, text: &str, send: impl Fn(u64, Answer) + 'static) {
        let Ok(report) = serde_json::from_str::<PromptReport>(text) else {
            return;
        };
        let reply = |id: u64| Reply {
            id,
            send: Some(Box::new(send)),
        };
        match report {
            PromptReport::Dialog {
                id,
                dialog,
                message,
                default_text,
                url,
            } => {
                let request = DialogRequest {
                    kind: dialog,
                    message,
                    default_text,
                    url,
                };
                let responder = DialogResponder(reply(id));
                match &self.dialog {
                    Some(handler) => handler(request, responder),
                    None => present_dialog(request, responder),
                }
            }
            PromptReport::Auth {
                id,
                host,
                realm,
                proxy,
                failures,
            } => {
                let responder = AuthResponder(reply(id));
                match &self.auth {
                    Some(handler) => handler(
                        AuthRequest {
                            host,
                            realm,
                            proxy,
                            failures,
                        },
                        responder,
                    ),
                    None => responder.cancel(),
                }
            }
            PromptReport::Files {
                id,
                accept,
                multiple,
            } => choose_files(accept, multiple, reply(id)),
            PromptReport::Download {
                id,
                url,
                name,
                mime,
                size,
            } => {
                let responder = DownloadResponder(reply(id));
                match &self.download {
                    Some(handler) => handler(
                        DownloadRequest {
                            id,
                            url,
                            suggested_name: sanitize_file_name(&name, "download"),
                            mime,
                            size: u64::try_from(size).ok(),
                        },
                        responder,
                    ),
                    None => responder.cancel(),
                }
            }
        }
    }

    /// Run the handler a notice is for.
    pub(crate) fn notice(&self, text: &str) {
        match serde_json::from_str::<Notice>(text) {
            Ok(Notice::LoadError {
                url,
                error,
                code,
                text,
            }) => {
                if let Some(handler) = &self.load_error {
                    handler(&LoadError {
                        url,
                        kind: error,
                        code,
                        description: text,
                    });
                }
            }
            Ok(Notice::RendererGone { crashed }) => {
                if let Some(handler) = &self.renderer_gone {
                    handler(&RendererGone { crashed });
                }
            }
            Err(_) => {}
        }
    }
}

/// Whether a navigation to `url` is one a web engine loads itself. Anything else (`mailto:`,
/// `tel:`, an app's own scheme, Android's `intent:`) is another program's to open, and a view
/// reports it like a link out of a bundled site ([`crate::WebView::on_external_link`]).
#[allow(dead_code)]
pub(crate) fn engine_scheme(url: &str) -> bool {
    let scheme = url
        .split_once(':')
        .map(|(scheme, _)| scheme.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        scheme.as_str(),
        "http"
            | "https"
            | "about"
            | "blob"
            | "data"
            | "file"
            | "javascript"
            | "day-resource"
            | "day-tab"
            | "view-source"
            | "chrome"
            | ""
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// The answers a test's handlers sent, in order.
    type Seen = Rc<RefCell<Vec<(u64, Answer)>>>;

    fn answers() -> (Seen, impl Fn(u64, Answer) + Clone + 'static) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = seen.clone();
        (seen, move |id, answer| sink.borrow_mut().push((id, answer)))
    }

    #[test]
    fn a_dialog_reaches_its_handler_and_answers_once() {
        let (seen, send) = answers();
        let handlers = Handlers {
            dialog: Some(Rc::new(|request, responder| {
                assert_eq!(request.kind, DialogKind::Prompt);
                assert_eq!(request.host(), "example.com");
                assert_eq!(request.default_text, "blue");
                responder.accept_with("green");
            })),
            ..Handlers::default()
        };
        handlers.prompt(
            r#"{"kind":"dialog","id":7,"dialog":"prompt","message":"Color?","default_text":"blue","url":"https://example.com/a"}"#,
            send,
        );
        let seen = seen.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, 7);
        assert!(seen[0].1.ok);
        assert_eq!(seen[0].1.text.as_deref(), Some("green"));
    }

    #[test]
    fn a_dropped_responder_cancels() {
        let (seen, send) = answers();
        let handlers = Handlers {
            dialog: Some(Rc::new(|_, responder| drop(responder))),
            auth: Some(Rc::new(|_, responder| drop(responder))),
            ..Handlers::default()
        };
        handlers.prompt(
            r#"{"kind":"dialog","id":1,"dialog":"confirm","message":"Sure?"}"#,
            send.clone(),
        );
        handlers.prompt(r#"{"kind":"auth","id":2,"host":"example.com"}"#, send);
        let seen = seen.borrow();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|(_, answer)| !answer.ok));
    }

    #[test]
    fn sign_in_without_a_handler_cancels_and_with_one_carries_the_credentials() {
        let (seen, send) = answers();
        Handlers::default().prompt(
            r#"{"kind":"auth","id":3,"host":"example.com","realm":"staff"}"#,
            send.clone(),
        );
        let handlers = Handlers {
            auth: Some(Rc::new(|request, responder| {
                assert_eq!((request.realm.as_str(), request.failures), ("staff", 1));
                responder.sign_in("ada", "s3cret");
            })),
            ..Handlers::default()
        };
        handlers.prompt(
            r#"{"kind":"auth","id":4,"host":"example.com","realm":"staff","failures":1}"#,
            send,
        );
        let seen = seen.borrow();
        assert!(!seen[0].1.ok);
        assert_eq!(seen[1].1.user.as_deref(), Some("ada"));
        assert_eq!(seen[1].1.password.as_deref(), Some("s3cret"));
    }

    #[test]
    fn a_download_names_a_safe_file_and_hears_its_events_until_the_last() {
        let (seen, send) = answers();
        let events = Rc::new(RefCell::new(Vec::new()));
        let heard = events.clone();
        let handlers = Handlers {
            download: Some(Rc::new(move |request, responder| {
                assert_eq!(request.suggested_name, "report.pdf");
                assert_eq!(request.size, None);
                let heard = heard.clone();
                responder.save_to("/tmp/report.pdf", move |event| {
                    heard.borrow_mut().push(event.clone())
                });
            })),
            ..Handlers::default()
        };
        handlers.prompt(
            r#"{"kind":"download","id":900001,"url":"https://example.com/r","name":"../../etc/report.pdf","mime":"application/pdf"}"#,
            send,
        );
        assert_eq!(seen.borrow()[0].1.paths, vec!["/tmp/report.pdf"]);
        download_event(
            900001,
            DownloadEvent::Progress {
                received: 10,
                total: Some(20),
            },
        );
        download_event(
            900001,
            DownloadEvent::Finished {
                location: "/tmp/report.pdf".into(),
            },
        );
        // Nothing is kept for a finished download.
        download_event(900001, DownloadEvent::Failed("late".into()));
        assert_eq!(
            *events.borrow(),
            vec![
                DownloadEvent::Progress {
                    received: 10,
                    total: Some(20)
                },
                DownloadEvent::Finished {
                    location: "/tmp/report.pdf".into()
                }
            ]
        );
    }

    #[test]
    fn file_names_lose_their_directories_and_refused_characters() {
        assert_eq!(sanitize_file_name("a/b/c.txt", "x"), "c.txt");
        assert_eq!(sanitize_file_name("..\\..\\evil.exe", "x"), "evil.exe");
        assert_eq!(sanitize_file_name("what?.txt", "x"), "what.txt");
        assert_eq!(sanitize_file_name("..", "download"), "download");
        assert_eq!(sanitize_file_name("  ", "download"), "download");
    }

    #[test]
    fn a_taken_name_gets_a_number_before_its_extension() {
        let dir = std::env::temp_dir().join(format!("day-webview-unused-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unused_path(&dir, "a.txt"), dir.join("a.txt"));
        std::fs::write(dir.join("a.txt"), b"1").unwrap();
        assert_eq!(unused_path(&dir, "a.txt"), dir.join("a (2).txt"));
        std::fs::write(dir.join("a (2).txt"), b"2").unwrap();
        assert_eq!(unused_path(&dir, "a.txt"), dir.join("a (3).txt"));
        std::fs::write(dir.join("noext"), b"3").unwrap();
        assert_eq!(unused_path(&dir, "noext"), dir.join("noext (2)"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn notices_reach_their_handlers() {
        let failed = Rc::new(Cell::new(None));
        let gone = Rc::new(Cell::new(None));
        let (f, g) = (failed.clone(), gone.clone());
        let handlers = Handlers {
            load_error: Some(Rc::new(move |error| {
                f.set(Some((error.kind, error.host())))
            })),
            renderer_gone: Some(Rc::new(move |report| g.set(Some(report.crashed)))),
            ..Handlers::default()
        };
        handlers.notice(
            r#"{"kind":"load-error","url":"https://expired.example/x","error":"certificate","code":-1202,"text":"untrusted"}"#,
        );
        handlers.notice(r#"{"kind":"renderer-gone","crashed":true}"#);
        handlers.notice("not json");
        assert_eq!(
            failed.take(),
            Some((LoadErrorKind::Certificate, "expired.example".to_string()))
        );
        assert_eq!(gone.take(), Some(true));
    }

    #[test]
    fn accept_lists_become_picker_extensions() {
        let list = |s: &str| vec![s.to_string()];
        assert_eq!(accept_extensions(&list(".pdf,.txt")), vec!["pdf", "txt"]);
        assert!(accept_extensions(&list("image/*")).contains(&"jpg"));
        assert_eq!(accept_extensions(&list("application/pdf")), vec!["pdf"]);
        // Anything the table cannot name means every file.
        assert!(accept_extensions(&list("image/*,.xyz")).is_empty());
        assert!(accept_extensions(&[]).is_empty());
    }

    #[test]
    fn only_web_schemes_stay_in_the_engine() {
        for url in [
            "https://example.com/",
            "about:blank",
            "blob:https://example.com/1",
            "data:text/plain,hi",
        ] {
            assert!(engine_scheme(url), "{url}");
        }
        for url in [
            "mailto:a@example.com",
            "tel:+15551234",
            "intent://scan/#Intent;scheme=zxing;end",
            "myapp://open",
        ] {
            assert!(!engine_scheme(url), "{url}");
        }
    }

    #[test]
    fn user_agents_cross_as_empty_a_marker_or_the_text() {
        assert_eq!(UserAgent::default().wire(), "");
        assert_eq!(UserAgent::Browser.wire(), "\u{1}");
        assert_eq!(
            UserAgent::Custom("  Mozilla/5.0 X  ".into()).wire(),
            "Mozilla/5.0 X"
        );
        assert_eq!(
            safari_application_name("18.6", true),
            "Version/18.6 Mobile/15E148 Safari/604.1"
        );
        assert_eq!(
            safari_application_name("26.1", false),
            "Version/26.1 Safari/605.1.15"
        );
    }

    #[test]
    fn trusted_hosts_are_remembered_without_case() {
        assert!(!host_trusted("self-signed.example"));
        trust_host("Self-Signed.Example");
        assert!(host_trusted("self-signed.example"));
    }
}
