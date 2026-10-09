// kernel32.dll, processes: CreateProcess (Windows command-line quoting, standard handles), process
// handles (waiting, exit codes, ending them), pipes, duplicating handles, and the process list
// (CreateToolhelp32Snapshot). A Windows process is a Linux process; .exe files run through winload
// because the kernel hands them to it.

#include <cerrno>
#include <csignal>
#include <cstring>
#include <dirent.h>
#include <fcntl.h>
#include <string>
#include <sys/ioctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>
#include <vector>

#include "handles.hpp"

void exec_program(const char *path, char *const *argv, char *const *envp) {
    char magic[2] = {0, 0};
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd >= 0) {
        (void)!read(fd, magic, 2);
        close(fd);
    }
    if (magic[0] == 'M' && magic[1] == 'Z') {
        // The program's own first word (argv[0]) can differ from its file: pass both.
        std::vector<char *> args = {const_cast<char *>("winload"), const_cast<char *>("--name"),
                                    argv[0] ? argv[0] : const_cast<char *>(path), const_cast<char *>(path)};
        for (char *const *a = argv + (argv[0] ? 1 : 0); *a; a++) args.push_back(*a);
        args.push_back(nullptr);
        execve("/proc/self/exe", args.data(), envp);
    }
    execve(path, argv, envp);
}

bool ProcessObject::check() {
    if (done) return true;
    int status = 0;
    if (waitpid(pid, &status, WNOHANG) == pid) {
        done = true;
        exit_code = WIFEXITED(status) ? static_cast<DWORD>(WEXITSTATUS(status)) : 0xC000013A;  // STATUS_CONTROL_C_EXIT
    }
    return done;
}

namespace {

// Splits a Windows command line the way the C runtime does (CommandLineToArgv rules): spaces
// separate arguments, "double quotes" group, and backslashes only escape before a quote.
std::vector<std::string> split_command_line(const char *line) {
    std::vector<std::string> args;
    const char *p = line;
    while (*p) {
        while (*p == ' ' || *p == '\t') p++;
        if (!*p) break;
        std::string arg;
        bool quoted = false;
        while (*p && (quoted || (*p != ' ' && *p != '\t'))) {
            if (*p == '\\') {
                size_t slashes = 0;
                while (*p == '\\') slashes++, p++;
                if (*p == '"') {
                    arg.append(slashes / 2, '\\');
                    if (slashes % 2) arg += *p++;  // an escaped quote
                } else {
                    arg.append(slashes, '\\');
                }
            } else if (*p == '"') {
                if (quoted && p[1] == '"') {  // "" inside quotes is a literal quote
                    arg += '"';
                    p += 2;
                } else {
                    quoted = !quoted;
                    p++;
                }
            } else {
                arg += *p++;
            }
        }
        args.push_back(arg);
    }
    return args;
}

bool is_file(const std::string &path) {
    struct stat st;
    return stat(path.c_str(), &st) == 0 && S_ISREG(st.st_mode);
}

// Finds a program the way Windows does: as given, then with .exe; in the current directory, then
// along PATH.
std::string find_program(const std::string &name) {
    std::string p = unix_path(name.c_str());
    std::vector<std::string> candidates = {p};
    if (p.find('.', p.rfind('/') == std::string::npos ? 0 : p.rfind('/')) == std::string::npos) candidates.push_back(p + ".exe");
    for (const std::string &c : candidates)
        if (is_file(c)) return c;
    if (p.find('/') != std::string::npos) return "";
    const char *path = getenv("PATH");
    std::string dirs = path ? path : "/bin:/usr/bin";
    size_t start = 0;
    while (start <= dirs.size()) {
        size_t end = dirs.find_first_of(":;", start);
        if (end == std::string::npos) end = dirs.size();
        std::string dir = unix_path(dirs.substr(start, end - start).c_str());
        for (const std::string &c : candidates)
            if (!dir.empty() && is_file(dir + "/" + c)) return dir + "/" + c;
        start = end + 1;
    }
    return "";
}

constexpr DWORD STARTF_USESTDHANDLES = 0x100;
constexpr DWORD HANDLE_FLAG_INHERIT = 1;

}  // namespace

extern "C" {

WINAPI BOOL k32_CreateProcessA(LPCSTR application, LPSTR command_line, void *, void *, BOOL, DWORD, void *environment,
                               LPCSTR directory, uint8_t *startup, HANDLE *info) {
    std::vector<std::string> args = split_command_line(command_line ? command_line : application);
    if (args.empty()) {
        set_last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    std::string program = find_program(application ? application : args[0]);
    if (program.empty()) {
        set_last_error(ERROR_FILE_NOT_FOUND);
        return 0;
    }
    // Standard handles for the child, when the caller chose them.
    int std_fds[3] = {0, 1, 2};
    DWORD flags = 0;
    memcpy(&flags, startup + 60, 4);
    if (flags & STARTF_USESTDHANDLES) {
        for (int i = 0; i < 3; i++) {
            HANDLE h;
            memcpy(&h, startup + 80 + i * 8, 8);
            int fd = handles::fd_of(h);
            if (fd >= 0) std_fds[i] = fd;
        }
    }
    // A Windows environment block: NAME=value strings, each ending in NUL, the whole thing in two.
    std::vector<std::string> env_strings;
    if (environment)
        for (const char *e = static_cast<const char *>(environment); *e; e += strlen(e) + 1) env_strings.push_back(e);

    flush_shared_views();
    pid_t pid = fork();
    if (pid == 0) {
        for (int i = 0; i < 3; i++)
            if (std_fds[i] != i) dup2(std_fds[i], i);
        if (directory) (void)!chdir(unix_path(directory).c_str());
        std::vector<char *> argv;
        for (std::string &a : args) argv.push_back(a.data());
        argv.push_back(nullptr);
        std::vector<char *> envp;
        for (std::string &e : env_strings) envp.push_back(e.data());
        envp.push_back(nullptr);
        exec_program(program.c_str(), argv.data(), environment ? envp.data() : process::envp);
        _exit(127);
    }
    if (pid < 0) return set_error_from_errno(errno), 0;
    HANDLE process = handles::add(new ProcessObject(pid));
    if (info) {
        info[0] = process;
        info[1] = handles::add(new ProcessObject(pid));  // the "thread" handle: waiting on it waits for the process
        DWORD ids[2] = {static_cast<DWORD>(pid), static_cast<DWORD>(pid)};
        memcpy(info + 2, ids, 8);
    }
    return 1;
}

WINAPI BOOL k32_GetExitCodeProcess(HANDLE h, DWORD *code) {
    if (h == reinterpret_cast<HANDLE>(-1)) {
        *code = 259;
        return 1;
    }
    auto *p = handles::get_as<ProcessObject>(h, Object::Kind::Process);
    if (!p) return set_last_error(ERROR_INVALID_HANDLE), 0;
    pthread_mutex_lock(&handles::lock);
    p->check();
    *code = p->exit_code;
    pthread_mutex_unlock(&handles::lock);
    return 1;
}

WINAPI DWORD k32_GetProcessId(HANDLE h) {
    if (h == reinterpret_cast<HANDLE>(-1)) return static_cast<DWORD>(getpid());
    auto *p = handles::get_as<ProcessObject>(h, Object::Kind::Process);
    return p ? static_cast<DWORD>(p->pid) : 0;
}

WINAPI HANDLE k32_OpenProcess(DWORD, BOOL, DWORD pid) {
    if (kill(static_cast<pid_t>(pid), 0) != 0) {
        set_last_error(87);
        return nullptr;
    }
    return handles::add(new ProcessObject(static_cast<int>(pid)));
}

WINAPI BOOL k32_TerminateProcess(HANDLE h, UINT code) {
    if (h == reinterpret_cast<HANDLE>(-1)) process::exit(static_cast<int>(code));
    auto *p = handles::get_as<ProcessObject>(h, Object::Kind::Process);
    if (!p) return set_last_error(ERROR_INVALID_HANDLE), 0;
    return kill(p->pid, SIGKILL) == 0;
}

// Process times in 100 ns units: creation and exit as dates, kernel and user as durations.
WINAPI BOOL k32_GetProcessTimes(HANDLE, uint64_t *created, uint64_t *exited, uint64_t *kernel, uint64_t *user) {
    rusage r{};
    getrusage(RUSAGE_SELF, &r);
    timespec now;
    clock_gettime(CLOCK_REALTIME, &now);
    *created = (static_cast<uint64_t>(now.tv_sec) + 11644473600ull) * 10000000ull;
    *exited = 0;
    *kernel = static_cast<uint64_t>(r.ru_stime.tv_sec) * 10000000ull + r.ru_stime.tv_usec * 10ull;
    *user = static_cast<uint64_t>(r.ru_utime.tv_sec) * 10000000ull + r.ru_utime.tv_usec * 10ull;
    return 1;
}

WINAPI BOOL k32_CreatePipe(HANDLE *read_end, HANDLE *write_end, void *security, DWORD) {
    int fds[2];
    if (pipe(fds) != 0) return set_error_from_errno(errno), 0;
    BOOL inherit = 0;
    if (security) memcpy(&inherit, static_cast<uint8_t *>(security) + 16, sizeof inherit);  // bInheritHandle
    if (!inherit) {
        fcntl(fds[0], F_SETFD, FD_CLOEXEC);
        fcntl(fds[1], F_SETFD, FD_CLOEXEC);
    }
    *read_end = handles::for_fd(fds[0]);
    *write_end = handles::for_fd(fds[1]);
    return 1;
}

WINAPI BOOL k32_PeekNamedPipe(HANDLE h, void *, DWORD, DWORD *, DWORD *available, DWORD *) {
    int n = 0;
    if (ioctl(handles::fd_of(h), FIONREAD, &n) != 0) return set_error_from_errno(errno), 0;
    if (available) *available = static_cast<DWORD>(n);
    return 1;
}

WINAPI BOOL k32_DuplicateHandle(HANDLE, HANDLE source, HANDLE, HANDLE *target, DWORD, BOOL, DWORD) {
    int fd = handles::fd_of(source);
    if (fd >= 0) {
        int copy = dup(fd);
        if (copy < 0) return set_error_from_errno(errno), 0;
        *target = handles::for_fd(copy);
        return 1;
    }
    if (source == reinterpret_cast<HANDLE>(-1) || source == reinterpret_cast<HANDLE>(-2)) {
        *target = source;  // pseudo-handles stay pseudo-handles here
        return 1;
    }
    *target = source;  // other objects are shared rather than copied
    return 1;
}

WINAPI BOOL k32_SetHandleInformation(HANDLE h, DWORD mask, DWORD flags) {
    int fd = handles::fd_of(h);
    if (fd >= 0 && (mask & HANDLE_FLAG_INHERIT)) {
        int f = fcntl(fd, F_GETFD);
        fcntl(fd, F_SETFD, flags & HANDLE_FLAG_INHERIT ? f & ~FD_CLOEXEC : f | FD_CLOEXEC);
    }
    return 1;
}

// The process list, read from /proc. PROCESSENTRY32 is 304 bytes.
struct Snapshot {
    std::vector<std::pair<DWORD, std::string>> processes;  // (pid, name)
    std::vector<DWORD> parents;
    size_t next = 0;
};

WINAPI HANDLE k32_CreateToolhelp32Snapshot(DWORD, DWORD) {
    auto *s = new Snapshot();
    if (DIR *d = opendir("/proc")) {
        while (dirent *e = readdir(d)) {
            char *end;
            long pid = strtol(e->d_name, &end, 10);
            if (*end || pid <= 0) continue;
            std::string base = std::string("/proc/") + e->d_name;
            char comm[64] = {0};
            int fd = open((base + "/comm").c_str(), O_RDONLY);
            if (fd >= 0) {
                ssize_t n = read(fd, comm, sizeof comm - 1);
                close(fd);
                if (n > 0 && comm[n - 1] == '\n') comm[n - 1] = 0;
            }
            DWORD parent = 0;
            char stat_line[256] = {0};
            fd = open((base + "/stat").c_str(), O_RDONLY);
            if (fd >= 0) {
                (void)!read(fd, stat_line, sizeof stat_line - 1);
                close(fd);
                if (char *close_paren = strrchr(stat_line, ')')) sscanf(close_paren + 2, "%*c %u", &parent);
            }
            s->processes.push_back({static_cast<DWORD>(pid), comm});
            s->parents.push_back(parent);
        }
        closedir(d);
    }
    return reinterpret_cast<HANDLE>(s);
}

static BOOL process_entry(HANDLE h, uint8_t *entry, bool first) {
    auto *s = reinterpret_cast<Snapshot *>(h);
    if (first) s->next = 0;
    if (s->next >= s->processes.size()) {
        set_last_error(ERROR_NO_MORE_FILES);
        return 0;
    }
    size_t i = s->next++;
    memset(entry + 4, 0, 300);
    DWORD pid = s->processes[i].first, parent = s->parents[i], threads = 1;
    memcpy(entry + 8, &pid, 4);
    memcpy(entry + 28, &threads, 4);
    memcpy(entry + 32, &parent, 4);
    strncpy(reinterpret_cast<char *>(entry + 44), s->processes[i].second.c_str(), 259);
    return 1;
}
WINAPI BOOL k32_Process32First(HANDLE h, uint8_t *entry) { return process_entry(h, entry, true); }
WINAPI BOOL k32_Process32Next(HANDLE h, uint8_t *entry) { return process_entry(h, entry, false); }

}  // extern "C"

#define EXPORT(name) {#name, reinterpret_cast<void *>(&k32_##name)}

namespace {
const Export exports[] = {
    EXPORT(CreatePipe), EXPORT(CreateProcessA), EXPORT(CreateToolhelp32Snapshot), EXPORT(DuplicateHandle),
    EXPORT(GetExitCodeProcess), EXPORT(GetProcessId), EXPORT(GetProcessTimes), EXPORT(OpenProcess), EXPORT(PeekNamedPipe),
    EXPORT(Process32First), EXPORT(Process32Next), EXPORT(SetHandleInformation), EXPORT(TerminateProcess),
};
}  // namespace

const BuiltinDll kernel32_process_dll{"kernel32.dll", exports, sizeof exports / sizeof exports[0]};
