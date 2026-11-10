// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// HarmonyOS: the ArkTS `Web` component. Unlike every other backend here, there is no native widget
// to construct (the ArkUI C node API has no Web node kind), so this crate ships its ArkTS
// (platform/harmony/ets/Index.ets) that `day build` stages into the app's hvigor project via
// `[package.metadata.day.ohos]`, the HarmonyOS counterpart of the android `java` contribution.
// day-toolkit-arkui's generic piece bridge builds it and returns its FrameNode as an ordinary handle
// (docs/extending.md); commands cross as this piece's own (cmd, arg) strings, and each committed
// navigation comes back through day-toolkit-arkui's `pieceEvent` export as the Custom event kind (12): §8.2's
// piece-defined channel, the same one the Android renderer uses.
// ---------------------------------------------------------------------------

use super::*;
use day_toolkit_arkui::{AHandle, ArkUi, piece};
use day_spec::NodeId;

fn make(_backend: &mut ArkUi, p: &WebProps, id: NodeId) -> AHandle {
    // `props` is one string: the initial URL, or the inline-site marker the ArkTS side parses
    // (`day-inline<US><root><US><start>`, 0x1F-separated like the eval replies). The rawfile
    // URL itself is composed over there, next to the code that polices it.
    let props = if p.inline_root.is_empty() {
        p.url.clone()
    } else {
        format!(
            "day-inline{SEP}{}{SEP}{}",
            p.inline_root,
            p.inline_start,
            SEP = super::SEP
        )
    };
    // A transparent view (`WebView::transparent`) leads with its own marker, which the ArkTS side
    // strips before reading the rest: the component's background is set when it is built.
    let props = if p.transparent {
        format!("day-transparent{}{props}", super::SEP)
    } else {
        props
    };
    // A provider's own site is policed by its base (`day-resource-site`); a provider attached to
    // an ordinary view (`WebView::resources`) only has its scheme handler installed.
    let props = match &p.resources {
        Some(_) if p.resource_site => format!("day-resource-site{SEP}{props}", SEP = super::SEP),
        Some(_) => format!("day-resource{SEP}{props}", SEP = super::SEP),
        None => props,
    };
    let props = if p.profile.private {
        format!("day-private{}{props}", super::SEP)
    } else {
        props
    };
    let props = if let Some(labels) = &p.tabs {
        format!(
            "day-tabs{SEP}{}{SEP}{}{SEP}{props}",
            labels.foreground,
            labels.background,
            SEP = super::SEP
        )
    } else {
        props
    };
    // Settings other than the defaults lead with their own marker, outermost:
    // `day-settings<US><text zoom percent><US><desktop 0/1><US><scripts 0/1><US>`.
    let props = if p.settings == WebSettings::default() {
        props
    } else {
        format!(
            "day-settings{SEP}{}{SEP}{}{SEP}{}{SEP}{props}",
            zoom_percent(p.settings.zoom),
            u8::from(p.settings.desktop_mode),
            u8::from(p.settings.javascript),
            SEP = super::SEP
        )
    };
    // Content blocking and page scripts lead with theirs, outermost of all:
    // `day-content<US><JSON><US>`. JSON escapes control characters, so the separator cannot
    // occur inside it. ArkWeb runs every document-start script in every frame, so each script
    // is composed to keep its own timing and frames, and all are joined into one in order.
    let props = if p.content.requests.is_some() || p.content.has_scripts() {
        let scripts: Vec<String> = p
            .content
            .page_scripts(
                super::content::Transport::Envelope("window.__dayMessageHost.post"),
                false,
            )
            .iter()
            .map(|script| {
                format!(
                    "try{{{}\n}}catch(e){{console.error(e)}}",
                    super::content::document_start_everywhere(script)
                )
            })
            .collect();
        let content = serde_json::json!({
            "filter": p.content.requests.is_some(),
            "messages": !p.content.messages.is_empty(),
            "scripts": scripts.join("\n"),
        });
        format!("day-content{SEP}{content}{SEP}{props}", SEP = super::SEP)
    } else {
        props
    };
    piece::make(KIND, id, &props)
}

/// ArkWeb's `textZoomRatio` takes a whole percentage.
fn zoom_percent(zoom: f64) -> u32 {
    (zoom * 100.0).round() as u32
}

fn update(_backend: &mut ArkUi, h: &AHandle, patch: &WebPatch) {
    // Evaluation (docs/webview-eval.md): `req` rides in front of the script, 0x1F-separated;
    // the ArkTS side runs `runJavaScript`, normalizes the reply, and answers on `pieceEvent`
    // with `req` as the num. Its try/catch guarantees exactly one reply even when the
    // controller is not yet attached (error 17100001).
    if let WebPatch::Eval { req, script } = patch {
        piece::update(h, "eval", &format!("{req}{}{script}", super::SEP));
        return;
    }
    // Settings (docs/webview.md): text zoom and the script switch are Web component attributes,
    // and desktop mode a custom user agent; the ArkTS side reloads for the latter two.
    let setting = match patch {
        WebPatch::Zoom(zoom) => Some(("zoom", zoom_percent(*zoom).to_string())),
        WebPatch::DesktopMode(desktop) => Some(("desktop", u8::from(*desktop).to_string())),
        WebPatch::JavaScript(enabled) => Some(("javascript", u8::from(*enabled).to_string())),
        _ => None,
    };
    if let Some((cmd, arg)) = setting {
        piece::update(h, cmd, &arg);
        return;
    }
    // A page message's reply, settled in the top-level document.
    if let WebPatch::ScriptReply { id, reply } = patch {
        piece::update(h, "run", &super::content::reply_script(*id, reply.as_deref()));
        return;
    }
    let (cmd, arg) = match patch {
        WebPatch::Load(u) => ("load", u.as_str()),
        WebPatch::Back => ("back", ""),
        WebPatch::Forward => ("forward", ""),
        WebPatch::Stop => ("stop", ""),
        WebPatch::Reload => ("reload", ""),
        WebPatch::ForceReload => ("forceReload", ""),
        // Handled above; listed to keep the match exhaustive.
        WebPatch::Eval { .. }
        | WebPatch::Zoom(_)
        | WebPatch::DesktopMode(_)
        | WebPatch::JavaScript(_)
        | WebPatch::ScriptReply { .. } => return,
        // WebKit's rule lists; ArkWeb filters through `request_filter`, and denies capture
        // devices to pages by default.
        WebPatch::ContentRules(_) | WebPatch::MediaDecision { .. } => return,
        // No retained views: a fresh view reports as its first load proceeds.
        WebPatch::ReportState => return,
    };
    piece::update(h, cmd, arg);
}

day_pieces::renderer!(day_toolkit_arkui::RENDERERS, ArkUi,
    kind: KIND, props: WebProps, patch: WebPatch,
    make: make, update: update, measure: day_pieces::fill_measure);
