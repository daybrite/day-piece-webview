// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
#pragma once
#include <cstdint>
#include <cstddef>
extern "C" {
struct DayResourceResponse;
DayResourceResponse *day_web_resource_read(uint64_t, const char *, const char *, const char *);
void day_web_resource_start(uint64_t, const char *, const char *, const char *, void *, void (*)(void *, DayResourceResponse *));
void day_web_resource_free(DayResourceResponse *);
uint16_t day_web_resource_status(const DayResourceResponse *);
const char *day_web_resource_mime(const DayResourceResponse *);
const char *day_web_resource_headers(const DayResourceResponse *);
const uint8_t *day_web_resource_data(const DayResourceResponse *);
size_t day_web_resource_len(const DayResourceResponse *);
}
