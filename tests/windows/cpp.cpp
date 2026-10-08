// A C++ program built with mingw-w64's g++: iostream, std::string and containers, and exceptions
// thrown through several functions, with destructors running on the way out (Windows does this with
// "structured exception handling" and stack unwinding). Prints "cpp: N/N passed" at the end.
#include <iostream>
#include <map>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

static int passed, total;
static void check(const std::string &what, bool ok) {
    total++;
    passed += ok;
    std::cout << "  " << (ok ? "ok  " : "FAIL") << " " << what << "\n";
}

static int destroyed;
struct Guard {
    ~Guard() { destroyed++; }
};

static int depth(int n) {
    Guard g;
    if (n == 0) throw std::runtime_error("bottom reached");
    return depth(n - 1) + 1;
}

struct Custom {
    int code;
};

int main() {
    std::vector<std::string> words = {"exceptions", "on", "KernelProject2"};
    std::map<std::string, int> lengths;
    for (const auto &w : words) lengths[w] = static_cast<int>(w.size());
    check("std::vector, std::map, std::string", lengths["exceptions"] == 10 && words.size() == 3);

    try {
        throw std::runtime_error("simple");
    } catch (const std::exception &e) {
        check("throw and catch std::runtime_error", std::string(e.what()) == "simple");
    }

    destroyed = 0;
    try {
        depth(5);
        check("exception through 6 frames", false);
    } catch (const std::runtime_error &e) {
        check("exception through 6 frames", std::string(e.what()) == "bottom reached");
    }
    check("destructors ran while unwinding", destroyed == 6);

    try {
        try {
            throw Custom{42};
        } catch (Custom &c) {
            c.code++;
            throw;  // rethrow the same object
        }
    } catch (const Custom &c) {
        check("catch a user type, rethrow, catch again", c.code == 43);
    }

    try {
        std::vector<int> v(3);
        (void)v.at(10);
    } catch (const std::out_of_range &) {
        check("std::out_of_range from the library", true);
    }

    auto p = std::make_unique<int>(7);
    check("std::unique_ptr", *p == 7);

    std::cout << "cpp: " << passed << "/" << total << " passed" << std::endl;
    return passed == total ? 0 : 1;
}
