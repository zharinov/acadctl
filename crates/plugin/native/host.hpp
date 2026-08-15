#pragma once

#include "dbmain.h"
#include <cstdint>

void acadctl_disable_native_wakes();
std::uint32_t acadctl_native_callbacks_outstanding();
void acadctl_create_bridge();
Acad::ErrorStatus acadctl_start_bridge();
bool acadctl_stop_bridge();
void acadctl_destroy_bridge();
int acadctl_load_doc();
int acadctl_unload_doc();
