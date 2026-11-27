// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

use std::rc::Rc;

/// The element used by a command. A linked image has separate link and image targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WebMenuTarget {
    Link,
    Image,
    DownloadImage,
}

/// A localized, app-owned command offered on an HTTP(S) link or image.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct WebMenuItem {
    pub id: String,
    pub label: String,
    pub target: WebMenuTarget,
    /// Open a separate browser context inside the trusted menu activation on web-dom.
    pub browser_on_web: bool,
}

impl WebMenuItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>, target: WebMenuTarget) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            target,
            browser_on_web: false,
        }
    }

    /// Let web-dom open the address directly inside the browser's trusted click handler.
    #[must_use]
    pub fn browser_on_web(mut self) -> Self {
        self.browser_on_web = true;
        self
    }
}

/// An app command selected on a particular document.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WebMenuAction {
    pub id: String,
    pub url: String,
    pub document: String,
}

pub(crate) type MenuHandler = (Vec<WebMenuItem>, Rc<dyn Fn(&WebMenuAction)>);
pub(crate) const REPORT: f64 = -13.0;

pub(crate) fn web_address(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url
                .host_str()
                .is_some_and(|host| !matches!(host, "day-resource.invalid" | "day-assets.example"))
            && !crate::content::is_internal(url.as_str())
            && url.username().is_empty()
            && url.password().is_none()
    })
}

#[cfg(any(
    all(target_os = "macos", feature = "appkit"),
    all(target_os = "ios", feature = "uikit"),
    feature = "gtk",
    feature = "qt",
    feature = "xaml"
))]
pub(crate) fn event(id: &str, url: &str, document: &str) -> day_spec::Event {
    let action = WebMenuAction {
        id: id.into(),
        url: url.into(),
        document: document.into(),
    };
    day_spec::Event::Custom {
        tag: "webview:context-menu",
        num: REPORT,
        text: serde_json::to_string(&action).unwrap_or_default(),
    }
}
