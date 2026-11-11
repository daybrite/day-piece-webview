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

    private static void checkBytes(String input, byte[] expected) {
        byte[] actual = DayWebContentType.percentDecode(input);
        if (!java.util.Arrays.equals(actual, expected)) {
            throw new AssertionError(input + " => " + java.util.Arrays.toString(actual));
        }
    }

    private static void checkReferrer(String page, String target, String expected) {
        String actual = DayWebContentType.referrer(page, target);
        if (!java.util.Objects.equals(actual, expected)) {
            throw new AssertionError(page + " -> " + target + " => " + actual);
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
        // data: downloads: each %XX is one byte and a literal + stays a + (a base64 digit).
        checkBytes("+w==", new byte[] {'+', 'w', '=', '='});
        checkBytes("%FF%00a", new byte[] {(byte) 0xFF, 0, 'a'});
        checkBytes("100%", new byte[] {'1', '0', '0', '%'});
        checkBytes("%zz%4", new byte[] {'%', 'z', 'z', '%', '4'});
        checkBytes("caf%C3%A9", new byte[] {'c', 'a', 'f', (byte) 0xC3, (byte) 0xA9});
        // A download's Referer: the whole address (never its fragment or credentials) to the
        // page's own origin, the origin alone to another, nothing on a downgrade.
        checkReferrer("https://a.example/p/q?token=1#frag", "https://a.example/file.zip", "https://a.example/p/q?token=1");
        checkReferrer("https://user:pw@A.example:443/p#x", "https://a.example/f", "https://a.example/p");
        checkReferrer("https://a.example/p?token=1#frag", "https://cdn.example/file.zip", "https://a.example/");
        checkReferrer("https://a.example:8443/p?token=1", "https://a.example/file.zip", "https://a.example:8443/");
        checkReferrer("https://a.example/p?token=1", "http://a.example/file.zip", null);
        checkReferrer("http://a.example/p?token=1#f", "https://a.example/file.zip", "http://a.example/");
        checkReferrer("http://a.example", "http://a.example/f", "http://a.example/");
        checkReferrer("file:///sdcard/page.html", "https://a.example/f", null);
        checkReferrer("about:blank", "https://a.example/f", null);
        checkReferrer("https://a.example/a b", "https://a.example/f", null);
        checkReferrer(null, "https://a.example/f", null);
        System.out.println("Android resource content-type regressions passed");
    }
}
