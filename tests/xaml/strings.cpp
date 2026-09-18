// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

#include "../../src/xaml-strings.h"
#include <iostream>

int main() {
    struct Case {
        std::wstring_view input;
        std::string expected;
    };
    const Case cases[] = {
        {L"", ""},
        {L"x", "x"},
        // The replies used by the Lottie readiness probe and the gallery assertion.
        {L"1\x001f\"object\"", "1\x1f\"object\""},
        {L"1\x001f\"playing\"", "1\x1f\"playing\""},
        {L"0\x001fSyntaxError\x001funexpected token", "0\x1fSyntaxError\x1funexpected token"},
        {L"caf\u00e9 \U0001f305", "caf\xc3\xa9 \xf0\x9f\x8c\x85"},
        {std::wstring_view(L"a\0b", 3), std::string("a\0b", 3)},
    };
    for (const auto &test : cases) {
        if (day_webview::utf8(test.input) != test.expected) {
            std::cerr << "WebView2 UTF-8 conversion did not preserve the payload\n";
            return 1;
        }
    }
}
