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
}
