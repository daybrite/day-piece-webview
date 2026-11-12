// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
package dev.daybrite.day.piece.webview;

import java.util.Locale;

/** Android's resource API requires separate MIME-type and character-encoding arguments. */
final class DayWebContentType {
    final String mime;
    final String encoding;

    private DayWebContentType(String mime, String encoding) {
        this.mime = mime;
        this.encoding = encoding;
    }

    static DayWebContentType parse(String contentType) {
        String[] parts = contentType.split(";");
        String mime = parts[0].trim().toLowerCase(Locale.ROOT);
        // Text keeps the adapter's UTF-8 default; binary responses have no character encoding.
        // No body bytes are transcoded.
        boolean text = mime.startsWith("text/") || mime.endsWith("+xml") || mime.endsWith("+json")
            || mime.equals("application/xml") || mime.equals("application/json")
            || mime.equals("application/javascript") || mime.equals("application/ecmascript");
        String encoding = text ? "UTF-8" : null;
        for (int i = 1; i < parts.length; i++) {
            int equal = parts[i].indexOf('=');
            if (equal < 0 || !parts[i].substring(0, equal).trim().equalsIgnoreCase("charset")) continue;
            String value = parts[i].substring(equal + 1).trim();
            if (value.length() >= 2 && value.startsWith("\"") && value.endsWith("\"")) {
                value = value.substring(1, value.length() - 1);
            }
            if (!value.isEmpty()) encoding = value;
            break;
        }
        return new DayWebContentType(mime, encoding);
    }

    /**
     * The bytes a data: address's payload stands for: each %XX is one byte, and every other
     * character is itself. URLDecoder is the wrong tool twice over: it turns a literal `+`
     * (a base64 digit) into a space, and it decodes into text, which mangles any byte that
     * is not valid UTF-8.
     */
    static byte[] percentDecode(String text) {
        java.io.ByteArrayOutputStream out = new java.io.ByteArrayOutputStream(text.length());
        byte[] raw = text.getBytes(java.nio.charset.StandardCharsets.UTF_8);
        for (int i = 0; i < raw.length; i++) {
            int hi, lo;
            if (raw[i] == '%' && i + 2 < raw.length && (hi = Character.digit(raw[i + 1], 16)) >= 0 && (lo = Character.digit(raw[i + 2], 16)) >= 0) {
                out.write(hi << 4 | lo);
                i += 2;
            } else {
                out.write(raw[i]);
            }
        }
        return out.toByteArray();
    }

    /**
     * The Referer a download's request to `target` carries when `page` asked for it, under the
     * browsers' default policy (strict-origin-when-cross-origin): the page's address without
     * its fragment or credentials to its own origin, only the origin to another, and nothing
     * from an https page to anything less, from a page that is not on the web, or when either
     * address does not parse. Null means send none.
     */
    static String referrer(String page, String target) {
        try {
            if (page == null || target == null) return null;
            java.net.URI from = new java.net.URI(page), to = new java.net.URI(target);
            if (from.getScheme() == null || to.getScheme() == null || from.getHost() == null || to.getHost() == null) return null;
            String scheme = from.getScheme().toLowerCase(java.util.Locale.ROOT), other = to.getScheme().toLowerCase(java.util.Locale.ROOT);
            boolean secure = scheme.equals("https");
            if (!secure && !scheme.equals("http")) return null;
            if (secure && !other.equals("https")) return null;
            String host = from.getHost().toLowerCase(java.util.Locale.ROOT);
            int port = from.getPort() == (secure ? 443 : 80) ? -1 : from.getPort();
            int otherPort = to.getPort() == (other.equals("https") ? 443 : 80) ? -1 : to.getPort();
            String origin = scheme + "://" + host + (port < 0 ? "" : ":" + port);
            if (!scheme.equals(other) || !host.equalsIgnoreCase(to.getHost()) || port != otherPort) return origin + "/";
            String path = from.getRawPath(), query = from.getRawQuery();
            return origin + (path == null || path.isEmpty() ? "/" : path) + (query == null ? "" : "?" + query);
        } catch (Exception error) { return null; }
    }

    /**
     * Chrome's user agent, from WebView's. The two marks of an embedded view go ("; wv" in the
     * device part and "Version/4.0" before Chrome's own version), and the string is reduced the
     * way Chrome reduces its own: the system and device become "Android 10; K" and the version
     * keeps only its major number. What stays is the engine's major version and whether the
     * device is a phone ("Mobile"), so the string follows the installed WebView and says no
     * more about the device than Chrome does.
     */
    static String browserAgent(String engine) {
        if (engine == null) return null;
        return engine
            .replaceFirst("\\(Linux; Android [^)]*\\)", "(Linux; Android 10; K)")
            .replace(" Version/4.0 ", " ")
            .replaceFirst("Chrome/(\\d+)\\.[\\d.]+", "Chrome/$1.0.0.0");
    }
}
