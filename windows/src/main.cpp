// winload: runs a Windows program (a PE .exe) as a normal process.
//
//   winload program.exe [arguments...]
//   winload --name first-word program.exe [arguments...]   (the program sees first-word as argv[0])
//
// The kernel starts this automatically when a .exe is run, the way it starts the interpreter named
// in a #! line. Steps, as Windows' own loader does them:
//   1. load the .exe and the DLLs it uses, linking their imports (loader.cpp);
//   2. set up the main thread: its TEB, the PEB, thread-local storage, TLS callbacks (teb.cpp);
//   3. run each DLL's DllMain;
//   4. jump to the program's entry point.

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <unistd.h>

#include "loader.hpp"

namespace process {
int argc;
char **argv;
char **envp;
void *image_base;
size_t image_size;
const char *command_line;
}  // namespace process

void process::exit(int code) {
    fflush(stdout);
    _exit(code);
}

namespace {
std::string quote_command_line(int argc, char **argv) {
    std::string line;
    for (int i = 0; i < argc; i++) {
        if (i) line += ' ';
        bool spaces = strchr(argv[i], ' ') != nullptr;
        if (spaces) line += '"';
        line += argv[i];
        if (spaces) line += '"';
    }
    return line;
}
}  // namespace

int main(int argc, char **argv, char **envp) {
    // --name: what the program is called in its own command line, when that isn't its file name.
    const char *name = nullptr;
    if (argc >= 4 && strcmp(argv[1], "--name") == 0) {
        name = argv[2];
        argv += 2;
        argc -= 2;
    }
    if (argc < 2) {
        fprintf(stderr, "usage: winload [--name first-word] program.exe [arguments...]\n");
        return 2;
    }
    const char *path = argv[1];
    if (name) argv[1] = const_cast<char *>(name);
    process::argc = argc - 1;
    process::argv = argv + 1;
    process::envp = envp;
    process::command_line = strdup(quote_command_line(argc - 1, argv + 1).c_str());

    std::string error;
    Module *exe = loader::load(path, error);
    if (!exe) {
        fprintf(stderr, "winload: %s\n", error.c_str());
        return 126;
    }
    process::image_base = exe->base;
    process::image_size = exe->size;

    teb::attach_thread(teb::DLL_PROCESS_ATTACH);
    loader::attach_all();

    // A console program's entry point takes nothing and returns the exit code.
    using Entry = int(WINAPI *)();
    process::exit(reinterpret_cast<Entry>(exe->base + exe->entry_rva)());
}
