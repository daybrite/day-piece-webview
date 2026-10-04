// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
package dev.daybrite.day.piece.webview;

/** Plain JVM regression fixtures; no emulator or Android framework stubs are required. */
public final class ContentTypeTest {
    private static void check(String input, String mime, String encoding) {
        DayWebContentType actual = DayWebContentType.parse(input);
        if (!actual.mime.equals(mime) || !java.util.Objects.equals(actual.encoding, encoding)) {
            throw new AssertionError(input + " => " + actual.mime + ", " + actual.encoding);
        }
    }

    public static void main(String[] args) {
        // Day-News' actual content-type spelling previously rendered HTML as literal source.
        check("text/html; charset=utf-8", "text/html", "utf-8");
        check(" Text/HTML ; CHARSET = \"ISO-8859-1\" ", "text/html", "ISO-8859-1");
        check("text/css; charset=UTF-8; other=value", "text/css", "UTF-8");
        check("application/json; other=value; charset=utf-16", "application/json", "utf-16");
        check("text/javascript", "text/javascript", "UTF-8");
        check("image/png", "image/png", null);
        check("application/octet-stream", "application/octet-stream", null);
        check("image/svg+xml", "image/svg+xml", "UTF-8");
        check("text/html; charset=\"\"", "text/html", "UTF-8");
        System.out.println("Android resource content-type regressions passed");
    }
}
