// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! Split HTTP Content-Type for native APIs with separate MIME and charset fields.

pub(crate) fn parse(value: &str) -> (String, Option<&str>) {
    // A quoted parameter may itself contain semicolons (or escaped quotes).
    let mut quoted = false;
    let mut escaped = false;
    let mut parts = value.split(|c| {
        if escaped {
            escaped = false;
        } else if quoted && c == '\\' {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
        } else if c == ';' && !quoted {
            return true;
        }
        false
    });
    let mime = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    let charset = parts.find_map(|part| {
        let (name, value) = part.split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("charset") {
            return None;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        (!value.is_empty()).then_some(value)
    });
    (mime, charset)
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn html_content_type_separates_charset_from_mime() {
        assert_eq!(
            parse("text/html; charset=utf-8"),
            ("text/html".into(), Some("utf-8"))
        );
        assert_eq!(
            parse(" Text/HTML ; CHARSET = \"ISO-8859-1\" "),
            ("text/html".into(), Some("ISO-8859-1"))
        );
    }

    #[test]
    fn unrelated_parameters_do_not_hide_or_invent_a_charset() {
        assert_eq!(
            parse(r#"text/css; note="a; charset=wrong; \"quote\""; charset=UTF-8; other=value"#),
            ("text/css".into(), Some("UTF-8"))
        );
        assert_eq!(
            parse("application/json; other=value; charset=utf-16"),
            ("application/json".into(), Some("utf-16"))
        );
    }

    #[test]
    fn absent_charset_leaves_browser_decoding_and_binary_bytes_alone() {
        for mime in [
            "text/html",
            "image/svg+xml",
            "image/png",
            "application/octet-stream",
        ] {
            assert_eq!(parse(mime), (mime.into(), None));
        }
        assert_eq!(parse("text/html; charset=\"\""), ("text/html".into(), None));
    }
}
