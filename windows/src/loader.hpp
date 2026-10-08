// Loading PE images (the .exe and any .dll files it uses) and linking them together.
#pragma once

#include <string>
#include <vector>

#include "win.hpp"

struct Module {
    std::string name;  // file name, lower case ("mathlib.dll")
    std::string path;  // where it was loaded from (a Linux path)
    uint8_t *base = nullptr;
    size_t size = 0;
    uint32_t entry_rva = 0;
    uint32_t export_rva = 0, export_size = 0;
    bool is_dll = false;
    bool attached = false;  // DllMain(DLL_PROCESS_ATTACH) has run
};

namespace loader {
// Loads an image and everything it imports (recursively), linking all imports. DllMain isn't run
// yet; see attach_all. Returns nullptr and sets `error` on failure.
Module *load(const std::string &path, std::string &error);
// Runs DllMain(DLL_PROCESS_ATTACH) for every loaded DLL not attached yet, dependencies first.
void attach_all();
// Every loaded image, in load order (the .exe first).
const std::vector<Module *> &modules();
}  // namespace loader

extern const BuiltinDll kernel32_loader_dll;
