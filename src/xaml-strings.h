// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

#pragma once

#include <stdexcept>
#include <string>
#include <string_view>
#include <windows.h>

namespace day_webview {

// Count the input explicitly: passing -1 includes the terminator, so a destination sized
// to the returned length minus one makes WideCharToMultiByte fail with insufficient buffer.
// This is also used for WebView2's evaluation envelopes, including their U+001F separators.
inline std::string utf8(std::wstring_view text) {
    if (text.empty())
        return {};
    const int count = static_cast<int>(text.size());
    const int length =
        WideCharToMultiByte(CP_UTF8, 0, text.data(), count, nullptr, 0, nullptr, nullptr);
    if (!length)
        throw std::runtime_error("WebView2 UTF-8 conversion failed");
    std::string result(static_cast<size_t>(length), '\0');
    if (WideCharToMultiByte(CP_UTF8, 0, text.data(), count, result.data(), length, nullptr,
                            nullptr) != length)
        throw std::runtime_error("WebView2 UTF-8 conversion failed");
    return result;
}

} // namespace day_webview
