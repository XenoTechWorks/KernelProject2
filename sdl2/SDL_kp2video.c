/*
  KernelProject2's video driver for SDL 2: SDL windows are windows on /dev/window.

  SDL draws each window into memory (its "framebuffer" path, which its software renderer uses)
  and this sends the parts that changed to the window with the WINDOW_BLIT ioctl. Keys, the
  mouse, focus, resizing and closing come back as the 16-byte events /dev/window reads give.
  See kernel/src/console.rs for the other side.

  Copied into SDL's source tree by sdl2/build.sh, which also registers it. Released under the
  same zlib license as SDL itself.
*/
#include "../../SDL_internal.h"

#ifdef SDL_VIDEO_DRIVER_DUMMY

#include "SDL_hints.h"
#include "SDL_loadso.h"
#include "SDL_video.h"
#include "../SDL_sysvideo.h"
#include "../../events/SDL_events_c.h"
#include "../../events/SDL_keyboard_c.h"
#include "../../events/SDL_mouse_c.h"
#include "../../events/SDL_windowevents_c.h"
#include "../../events/scancodes_linux.h"

#include <errno.h>
#include <fcntl.h>
#include <sys/ioctl.h>
#include <unistd.h>

#define KP2VID_DRIVER_NAME "kernelproject2"

/* The kernel's window ioctls and events (kernel/src/console.rs, kernel/src/sys/fs.rs). */
#define WINDOW_PIXELS 0x5701
#define WINDOW_TITLE  0x5702
#define WINDOW_BLIT   0x5703

enum
{
    EV_MOTION = 1,
    EV_PRESS = 2,
    EV_RELEASE = 3,
    EV_KEY = 4,
    EV_LEAVE = 5,
    EV_FOCUS = 6,
    EV_SCROLL = 7,
    EV_RESIZE = 8,
    EV_RAWKEY = 9
};

struct kp2_event
{
    Uint32 kind;
    Sint32 x, y;
    Uint32 data;
};

struct kp2_blit
{
    Uint32 x, y, w, h, stride, pad;
    Uint64 pixels;
};

typedef struct
{
    int fd;
    Uint32 *pixels;
    int w, h;
    SDL_bool closed;
    /* What OpenGL draws into (see "OpenGL" below), and its size. */
    Uint32 *gl_pixels;
    int gl_w, gl_h;
} KP2_Window;

/* PC scancodes after an 0xE0 byte, as Linux key codes (the plain ones are the same numbers). */
static int extended_to_linux(int code)
{
    switch (code) {
    case 0x1C: return 96;  /* keypad Enter */
    case 0x1D: return 97;  /* right Ctrl */
    case 0x35: return 98;  /* keypad / */
    case 0x38: return 100; /* right Alt */
    case 0x47: return 102; /* Home */
    case 0x48: return 103; /* Up */
    case 0x49: return 104; /* Page Up */
    case 0x4B: return 105; /* Left */
    case 0x4D: return 106; /* Right */
    case 0x4F: return 107; /* End */
    case 0x50: return 108; /* Down */
    case 0x51: return 109; /* Page Down */
    case 0x52: return 110; /* Insert */
    case 0x53: return 111; /* Delete */
    case 0x5B: return 125; /* left Windows key */
    case 0x5C: return 126; /* right Windows key */
    case 0x5D: return 127; /* Menu */
    default: return 0;
    }
}

static SDL_Scancode to_sdl_scancode(int pc)
{
    const int linux_code = (pc & 0x100) ? extended_to_linux(pc & 0xff) : (pc & 0xff);
    if (linux_code <= 0 || linux_code >= (int)SDL_arraysize(linux_scancode_table)) {
        return SDL_SCANCODE_UNKNOWN;
    }
    return linux_scancode_table[linux_code];
}

/* ---- windows ---- */

static int KP2_CreateWindow(_THIS, SDL_Window *window)
{
    KP2_Window *data;
    Uint32 size[2];

    data = (KP2_Window *)SDL_calloc(1, sizeof(*data));
    if (!data) {
        return SDL_OutOfMemory();
    }
    data->fd = open("/dev/window", O_RDWR | O_CLOEXEC | O_NONBLOCK);
    if (data->fd < 0) {
        SDL_free(data);
        return SDL_SetError("Couldn't open /dev/window: %s", strerror(errno));
    }
    /* A picture of the size asked for; the kernel may make it smaller to fit the screen. */
    size[0] = window->w;
    size[1] = window->h;
    if (ioctl(data->fd, WINDOW_PIXELS, size) < 0) {
        close(data->fd);
        SDL_free(data);
        return SDL_SetError("Couldn't make a graphical window");
    }
    window->w = (int)size[0];
    window->h = (int)size[1];
    if (window->title) {
        ioctl(data->fd, WINDOW_TITLE, window->title);
    }
    window->driverdata = data;
    return 0;
}

static void KP2_SetWindowTitle(_THIS, SDL_Window *window)
{
    KP2_Window *data = (KP2_Window *)window->driverdata;
    if (data && window->title) {
        ioctl(data->fd, WINDOW_TITLE, window->title);
    }
}

static void KP2_SetWindowSize(_THIS, SDL_Window *window)
{
    KP2_Window *data = (KP2_Window *)window->driverdata;
    Uint32 size[2];
    if (!data) {
        return;
    }
    size[0] = window->w;
    size[1] = window->h;
    if (ioctl(data->fd, WINDOW_PIXELS, size) == 0) {
        window->w = (int)size[0];
        window->h = (int)size[1];
    }
}

/* "Fullscreen" is a window as big as a window can be (the display's size, see KP2_VideoInit):
   there's no taking the screen from the desktop. Leaving it goes back to the size before. */
static void KP2_SetWindowFullscreen(_THIS, SDL_Window *window, SDL_VideoDisplay *display, SDL_bool fullscreen)
{
    KP2_Window *data = (KP2_Window *)window->driverdata;
    Uint32 size[2];
    if (!data) {
        return;
    }
    size[0] = fullscreen ? display->desktop_mode.w : window->windowed.w;
    size[1] = fullscreen ? display->desktop_mode.h : window->windowed.h;
    if (ioctl(data->fd, WINDOW_PIXELS, size) == 0) {
        window->w = (int)size[0];
        window->h = (int)size[1];
    }
}

static void forget_vulkan_window(SDL_Window *window);

static void KP2_DestroyWindow(_THIS, SDL_Window *window)
{
    KP2_Window *data = (KP2_Window *)window->driverdata;
    forget_vulkan_window(window);
    if (data) {
        close(data->fd);
        SDL_free(data->pixels);
        SDL_free(data->gl_pixels);
        SDL_free(data);
        window->driverdata = NULL;
    }
}

/* ---- drawing ---- */

static int KP2_CreateWindowFramebuffer(_THIS, SDL_Window *window, Uint32 *format, void **pixels, int *pitch)
{
    KP2_Window *data = (KP2_Window *)window->driverdata;
    int w, h;

    SDL_GetWindowSizeInPixels(window, &w, &h);
    SDL_free(data->pixels);
    data->pixels = (Uint32 *)SDL_calloc((size_t)w * h, sizeof(Uint32));
    if (!data->pixels) {
        return SDL_OutOfMemory();
    }
    data->w = w;
    data->h = h;
    /* 0x00RRGGBB in each 32-bit word: the kernel's format exactly, so nothing is converted. */
    *format = SDL_PIXELFORMAT_RGB888;
    *pixels = data->pixels;
    *pitch = w * 4;
    return 0;
}

static int KP2_UpdateWindowFramebuffer(_THIS, SDL_Window *window, const SDL_Rect *rects, int numrects)
{
    KP2_Window *data = (KP2_Window *)window->driverdata;
    int i;

    for (i = 0; i < numrects; i++) {
        SDL_Rect r = rects[i];
        struct kp2_blit b;
        if (r.x < 0) { r.w += r.x; r.x = 0; }
        if (r.y < 0) { r.h += r.y; r.y = 0; }
        if (r.x + r.w > data->w) { r.w = data->w - r.x; }
        if (r.y + r.h > data->h) { r.h = data->h - r.y; }
        if (r.w <= 0 || r.h <= 0) {
            continue;
        }
        b.x = r.x;
        b.y = r.y;
        b.w = r.w;
        b.h = r.h;
        b.stride = data->w;
        b.pad = 0;
        b.pixels = (Uint64)(uintptr_t)(data->pixels + (size_t)r.y * data->w + r.x);
        ioctl(data->fd, WINDOW_BLIT, &b);
    }
    return 0;
}

static void KP2_DestroyWindowFramebuffer(_THIS, SDL_Window *window)
{
    KP2_Window *data = (KP2_Window *)window->driverdata;
    if (data) {
        SDL_free(data->pixels);
        data->pixels = NULL;
    }
}

/* ---- input ---- */

static Uint8 to_sdl_button(Uint32 button)
{
    switch (button) {
    case 2: return SDL_BUTTON_RIGHT;
    case 3: return SDL_BUTTON_MIDDLE;
    default: return SDL_BUTTON_LEFT;
    }
}

static void KP2_HandleEvent(SDL_Window *window, const struct kp2_event *e)
{
    switch (e->kind) {
    case EV_MOTION:
        SDL_SetMouseFocus(window);
        SDL_SendMouseMotion(window, 0, 0, e->x, e->y);
        break;
    case EV_PRESS:
    case EV_RELEASE:
        SDL_SetMouseFocus(window);
        SDL_SendMouseMotion(window, 0, 0, e->x, e->y);
        SDL_SendMouseButton(window, 0, e->kind == EV_PRESS ? SDL_PRESSED : SDL_RELEASED, to_sdl_button(e->data));
        break;
    case EV_SCROLL:
        /* Ours counts down as positive; SDL counts up. */
        SDL_SendMouseWheel(window, 0, 0.0f, -(float)(Sint32)e->data, SDL_MOUSEWHEEL_NORMAL);
        break;
    case EV_LEAVE:
        SDL_SetMouseFocus(NULL);
        break;
    case EV_FOCUS:
        SDL_SetKeyboardFocus(e->data ? window : NULL);
        break;
    case EV_RAWKEY: {
        const SDL_Scancode sc = to_sdl_scancode(e->x);
        if (sc != SDL_SCANCODE_UNKNOWN) {
            SDL_SendKeyboardKey(e->data ? SDL_PRESSED : SDL_RELEASED, sc);
        }
        break;
    }
    case EV_KEY:
        /* The character typed, for text input (e->x holds shift and ctrl). */
        if (e->data >= 32 && e->data < 127 && !(e->x & 2)) {
            char text[2];
            text[0] = (char)e->data;
            text[1] = '\0';
            SDL_SendKeyboardText(text);
        }
        break;
    case EV_RESIZE:
        SDL_SendWindowEvent(window, SDL_WINDOWEVENT_RESIZED, e->x, e->y);
        break;
    default:
        break;
    }
}

static void KP2_PumpEvents(_THIS)
{
    SDL_Window *window;

    for (window = _this->windows; window; window = window->next) {
        KP2_Window *data = (KP2_Window *)window->driverdata;
        struct kp2_event events[32];
        ssize_t n;
        if (!data || data->closed) {
            continue;
        }
        for (;;) {
            n = read(data->fd, events, sizeof(events));
            if (n > 0) {
                ssize_t i;
                for (i = 0; i < n / (ssize_t)sizeof(events[0]); i++) {
                    KP2_HandleEvent(window, &events[i]);
                }
                continue;
            }
            if (n == 0) {
                /* The window's × was clicked. */
                data->closed = SDL_TRUE;
                SDL_SendWindowEvent(window, SDL_WINDOWEVENT_CLOSE, 0, 0);
            }
            break; /* nothing more for now (EAGAIN), or closed */
        }
    }
}

/* ---- OpenGL ----

   There's no GPU, so OpenGL is Mesa drawing in memory with the processor: OSMesa (Mesa's
   "off-screen" interface, with llvmpipe behind it), from Software's "mesa" package. A context
   draws into a buffer the size of its window; swapping sends that to the window like any other
   picture. The buffer is BGRA, top row first: the kernel's 0x00RRGGBB pixels exactly. */

typedef struct osmesa_context *OSMesaContext;
typedef void (*OSMESAproc)(void);

#define OSMESA_BGRA 0x1
#define OSMESA_Y_UP 0x11
#define OSMESA_FORMAT 0x22
#define OSMESA_DEPTH_BITS 0x30
#define OSMESA_STENCIL_BITS 0x31
#define OSMESA_ACCUM_BITS 0x32
#define OSMESA_PROFILE 0x33
#define OSMESA_CORE_PROFILE 0x34
#define OSMESA_COMPAT_PROFILE 0x35
#define OSMESA_CONTEXT_MAJOR_VERSION 0x36
#define OSMESA_CONTEXT_MINOR_VERSION 0x37
#define KP2_GL_UNSIGNED_BYTE 0x1401

static struct
{
    void *lib;
    OSMesaContext (*CreateContextAttribs)(const int *attribs, OSMesaContext share);
    void (*DestroyContext)(OSMesaContext ctx);
    unsigned char (*MakeCurrent)(OSMesaContext ctx, void *buffer, unsigned type, int w, int h);
    void (*PixelStore)(int pname, int value);
    OSMESAproc (*GetProcAddress)(const char *name);
    void (*Finish)(void);
    int swap_interval;
} osmesa;

static int KP2_GL_LoadLibrary(_THIS, const char *path)
{
    if (!path) {
        path = SDL_getenv("SDL_OPENGL_LIBRARY");
    }
    if (!path) {
        path = "libOSMesa.so.8";
    }
    osmesa.lib = SDL_LoadObject(path);
    if (!osmesa.lib) {
        return SDL_SetError("OpenGL needs Mesa: install \"OpenGL and Vulkan (Mesa)\" with Software (%s)", SDL_GetError());
    }
    osmesa.CreateContextAttribs = SDL_LoadFunction(osmesa.lib, "OSMesaCreateContextAttribs");
    osmesa.DestroyContext = SDL_LoadFunction(osmesa.lib, "OSMesaDestroyContext");
    osmesa.MakeCurrent = SDL_LoadFunction(osmesa.lib, "OSMesaMakeCurrent");
    osmesa.PixelStore = SDL_LoadFunction(osmesa.lib, "OSMesaPixelStore");
    osmesa.GetProcAddress = SDL_LoadFunction(osmesa.lib, "OSMesaGetProcAddress");
    osmesa.Finish = SDL_LoadFunction(osmesa.lib, "glFinish");
    if (!osmesa.CreateContextAttribs || !osmesa.DestroyContext || !osmesa.MakeCurrent ||
        !osmesa.PixelStore || !osmesa.GetProcAddress || !osmesa.Finish) {
        SDL_UnloadObject(osmesa.lib);
        osmesa.lib = NULL;
        return SDL_SetError("%s isn't an OSMesa library", path);
    }
    SDL_strlcpy(_this->gl_config.driver_path, path, SDL_arraysize(_this->gl_config.driver_path));
    return 0;
}

static void *KP2_GL_GetProcAddress(_THIS, const char *proc)
{
    void *p = (void *)osmesa.GetProcAddress(proc);
    return p ? p : SDL_LoadFunction(osmesa.lib, proc);
}

static void KP2_GL_UnloadLibrary(_THIS)
{
    if (osmesa.lib) {
        SDL_UnloadObject(osmesa.lib);
        osmesa.lib = NULL;
    }
}

/* Binds a context to the window's buffer, (re)making the buffer at the window's size. */
static int bind_window(SDL_Window *window, OSMesaContext ctx)
{
    KP2_Window *data = (KP2_Window *)window->driverdata;
    if (!data->gl_pixels || data->gl_w != window->w || data->gl_h != window->h) {
        SDL_free(data->gl_pixels);
        data->gl_pixels = (Uint32 *)SDL_calloc((size_t)window->w * window->h, sizeof(Uint32));
        if (!data->gl_pixels) {
            return SDL_OutOfMemory();
        }
        data->gl_w = window->w;
        data->gl_h = window->h;
    }
    if (!osmesa.MakeCurrent(ctx, data->gl_pixels, KP2_GL_UNSIGNED_BYTE, data->gl_w, data->gl_h)) {
        return SDL_SetError("OSMesaMakeCurrent failed");
    }
    osmesa.PixelStore(OSMESA_Y_UP, 0); /* the first row is the top one, as on the screen */
    return 0;
}

static SDL_GLContext KP2_GL_CreateContext(_THIS, SDL_Window *window)
{
    int attribs[20], n = 0;
    OSMesaContext ctx;
    const SDL_bool core = (_this->gl_config.profile_mask & SDL_GL_CONTEXT_PROFILE_CORE) != 0;

    attribs[n++] = OSMESA_FORMAT;
    attribs[n++] = OSMESA_BGRA;
    attribs[n++] = OSMESA_DEPTH_BITS;
    attribs[n++] = _this->gl_config.depth_size > 0 ? 24 : 0;
    attribs[n++] = OSMESA_STENCIL_BITS;
    attribs[n++] = _this->gl_config.stencil_size > 0 ? 8 : 0;
    attribs[n++] = OSMESA_ACCUM_BITS;
    attribs[n++] = 0;
    attribs[n++] = OSMESA_PROFILE;
    attribs[n++] = core ? OSMESA_CORE_PROFILE : OSMESA_COMPAT_PROFILE;
    if (core || _this->gl_config.major_version > 2) {
        attribs[n++] = OSMESA_CONTEXT_MAJOR_VERSION;
        attribs[n++] = _this->gl_config.major_version;
        attribs[n++] = OSMESA_CONTEXT_MINOR_VERSION;
        attribs[n++] = _this->gl_config.minor_version;
    }
    attribs[n++] = 0;
    ctx = osmesa.CreateContextAttribs(attribs, NULL);
    if (!ctx) {
        SDL_SetError("Mesa couldn't make an OpenGL %d.%d context", _this->gl_config.major_version, _this->gl_config.minor_version);
        return NULL;
    }
    if (bind_window(window, ctx) < 0) {
        osmesa.DestroyContext(ctx);
        return NULL;
    }
    return (SDL_GLContext)ctx;
}

static int KP2_GL_MakeCurrent(_THIS, SDL_Window *window, SDL_GLContext context)
{
    if (!context || !window) {
        return 0; /* nothing to unbind from: the next context made current replaces it */
    }
    return bind_window(window, (OSMesaContext)context);
}

static int KP2_GL_SwapWindow(_THIS, SDL_Window *window)
{
    KP2_Window *data = (KP2_Window *)window->driverdata;
    struct kp2_blit b;
    SDL_GLContext current = SDL_GL_GetCurrentContext();

    if (!data->gl_pixels) {
        return 0;
    }
    osmesa.Finish();
    b.x = 0;
    b.y = 0;
    b.w = SDL_min(data->gl_w, window->w);
    b.h = SDL_min(data->gl_h, window->h);
    b.stride = data->gl_w;
    b.pad = 0;
    b.pixels = (Uint64)(uintptr_t)data->gl_pixels;
    ioctl(data->fd, WINDOW_BLIT, &b);
    /* The window changed size: the next frame is drawn at the new one. */
    if (current && (data->gl_w != window->w || data->gl_h != window->h)) {
        return bind_window(window, (OSMesaContext)current);
    }
    return 0;
}

static int KP2_GL_SetSwapInterval(_THIS, int interval)
{
    osmesa.swap_interval = interval;
    return 0;
}

static int KP2_GL_GetSwapInterval(_THIS)
{
    return osmesa.swap_interval;
}

static void KP2_GL_DeleteContext(_THIS, SDL_GLContext context)
{
    if (context) {
        osmesa.DestroyContext((OSMesaContext)context);
    }
}

/* ---- Vulkan ----

   Vulkan is Mesa's lavapipe (also in the "mesa" package), which can't show anything itself
   here: SDL's windows get "headless" surfaces, and the VK_LAYER_KP2_present layer (in the same
   package) copies each presented picture into the window, which it finds by asking
   KP2_VulkanSurfaceWindow. */

#ifdef SDL_VIDEO_VULKAN
#include "../SDL_vulkan_internal.h"

static struct
{
    VkSurfaceKHR surface;
    SDL_Window *window;
} vk_surfaces[64];

/* For the present layer: the /dev/window file descriptor of the window a surface was made for,
   or -1. */
__attribute__((visibility("default"))) int KP2_VulkanSurfaceWindow(Uint64 surface)
{
    int i;
    for (i = 0; i < (int)SDL_arraysize(vk_surfaces); i++) {
        if (vk_surfaces[i].window && (Uint64)vk_surfaces[i].surface == surface) {
            KP2_Window *data = (KP2_Window *)vk_surfaces[i].window->driverdata;
            return data ? data->fd : -1;
        }
    }
    return -1;
}

static int KP2_Vulkan_LoadLibrary(_THIS, const char *path)
{
    VkExtensionProperties *extensions;
    Uint32 i, count = 0;
    SDL_bool surface = SDL_FALSE, headless = SDL_FALSE;

    if (_this->vulkan_config.loader_handle) {
        return SDL_SetError("Vulkan already loaded");
    }
    if (!path) {
        path = SDL_getenv("SDL_VULKAN_LIBRARY");
    }
    if (!path) {
        path = "libvulkan.so.1";
    }
    _this->vulkan_config.loader_handle = SDL_LoadObject(path);
    if (!_this->vulkan_config.loader_handle) {
        return SDL_SetError("Vulkan needs Mesa: install \"OpenGL and Vulkan (Mesa)\" with Software (%s)", SDL_GetError());
    }
    SDL_strlcpy(_this->vulkan_config.loader_path, path, SDL_arraysize(_this->vulkan_config.loader_path));
    _this->vulkan_config.vkGetInstanceProcAddr = SDL_LoadFunction(_this->vulkan_config.loader_handle, "vkGetInstanceProcAddr");
    if (!_this->vulkan_config.vkGetInstanceProcAddr) {
        goto fail;
    }
    _this->vulkan_config.vkEnumerateInstanceExtensionProperties =
        (void *)((PFN_vkGetInstanceProcAddr)_this->vulkan_config.vkGetInstanceProcAddr)(VK_NULL_HANDLE, "vkEnumerateInstanceExtensionProperties");
    if (!_this->vulkan_config.vkEnumerateInstanceExtensionProperties) {
        goto fail;
    }
    extensions = SDL_Vulkan_CreateInstanceExtensionsList(
        (PFN_vkEnumerateInstanceExtensionProperties)_this->vulkan_config.vkEnumerateInstanceExtensionProperties, &count);
    if (!extensions) {
        goto fail;
    }
    for (i = 0; i < count; i++) {
        if (SDL_strcmp(VK_KHR_SURFACE_EXTENSION_NAME, extensions[i].extensionName) == 0) {
            surface = SDL_TRUE;
        } else if (SDL_strcmp(VK_EXT_HEADLESS_SURFACE_EXTENSION_NAME, extensions[i].extensionName) == 0) {
            headless = SDL_TRUE;
        }
    }
    SDL_free(extensions);
    if (!surface || !headless) {
        SDL_SetError("This Vulkan has no " VK_EXT_HEADLESS_SURFACE_EXTENSION_NAME ", which windows here need");
        goto fail;
    }
    return 0;

fail:
    SDL_UnloadObject(_this->vulkan_config.loader_handle);
    _this->vulkan_config.loader_handle = NULL;
    return -1;
}

static void KP2_Vulkan_UnloadLibrary(_THIS)
{
    if (_this->vulkan_config.loader_handle) {
        SDL_UnloadObject(_this->vulkan_config.loader_handle);
        _this->vulkan_config.loader_handle = NULL;
    }
}

static SDL_bool KP2_Vulkan_GetInstanceExtensions(_THIS, SDL_Window *window, unsigned *count, const char **names)
{
    static const char *const needed[] = { VK_KHR_SURFACE_EXTENSION_NAME, VK_EXT_HEADLESS_SURFACE_EXTENSION_NAME };
    if (!_this->vulkan_config.loader_handle) {
        SDL_SetError("Vulkan is not loaded");
        return SDL_FALSE;
    }
    return SDL_Vulkan_GetInstanceExtensions_Helper(count, names, SDL_arraysize(needed), needed);
}

static SDL_bool KP2_Vulkan_CreateSurface(_THIS, SDL_Window *window, VkInstance instance, VkSurfaceKHR *surface)
{
    PFN_vkGetInstanceProcAddr gipa = (PFN_vkGetInstanceProcAddr)_this->vulkan_config.vkGetInstanceProcAddr;
    PFN_vkCreateHeadlessSurfaceEXT create;
    VkHeadlessSurfaceCreateInfoEXT info;
    int i;

    if (!_this->vulkan_config.loader_handle) {
        SDL_SetError("Vulkan is not loaded");
        return SDL_FALSE;
    }
    create = (PFN_vkCreateHeadlessSurfaceEXT)(void *)gipa(instance, "vkCreateHeadlessSurfaceEXT");
    if (!create) {
        SDL_SetError(VK_EXT_HEADLESS_SURFACE_EXTENSION_NAME " wasn't enabled for this instance");
        return SDL_FALSE;
    }
    SDL_zero(info);
    info.sType = VK_STRUCTURE_TYPE_HEADLESS_SURFACE_CREATE_INFO_EXT;
    if (create(instance, &info, NULL, surface) != VK_SUCCESS) {
        SDL_SetError("vkCreateHeadlessSurfaceEXT failed");
        return SDL_FALSE;
    }
    for (i = 0; i < (int)SDL_arraysize(vk_surfaces); i++) {
        if (!vk_surfaces[i].window) {
            vk_surfaces[i].surface = *surface;
            vk_surfaces[i].window = window;
            break;
        }
    }
    return SDL_TRUE;
}

static void KP2_Vulkan_GetDrawableSize(_THIS, SDL_Window *window, int *w, int *h)
{
    SDL_GetWindowSize(window, w, h);
}

static void forget_vulkan_window(SDL_Window *window)
{
    int i;
    for (i = 0; i < (int)SDL_arraysize(vk_surfaces); i++) {
        if (vk_surfaces[i].window == window) {
            vk_surfaces[i].window = NULL;
        }
    }
}
#else
static void forget_vulkan_window(SDL_Window *window)
{
}
#endif /* SDL_VIDEO_VULKAN */

/* ---- the driver ---- */

static int KP2_VideoInit(_THIS)
{
    SDL_DisplayMode mode;
    Uint32 size[2] = { 65535, 65535 };
    int fd;

    SDL_zero(mode);
    mode.format = SDL_PIXELFORMAT_RGB888;
    /* The display is the biggest window there can be (the screen less a window's frame), so
       programs that size themselves to the display, or go fullscreen, fit in a window exactly.
       The kernel makes the size asked for fit; a window isn't shown until something is drawn in
       it, so this one never appears. */
    mode.w = 1280;
    mode.h = 800;
    fd = open("/dev/window", O_RDWR | O_CLOEXEC);
    if (fd >= 0) {
        if (ioctl(fd, WINDOW_PIXELS, size) == 0) {
            mode.w = (int)size[0];
            mode.h = (int)size[1];
        }
        close(fd);
    }
    mode.refresh_rate = 60;
    if (SDL_AddBasicVideoDisplay(&mode) < 0) {
        return -1;
    }
    SDL_AddDisplayMode(&_this->displays[0], &mode);
    /* There's no GPU: the software renderer is the only one. Games that ask SDL_CreateRenderer
       for an accelerated renderer would otherwise get none at all, so have them use it (unless
       told otherwise with SDL_RENDER_DRIVER). Naming a driver this way skips the flags check. */
    SDL_SetHintWithPriority(SDL_HINT_RENDER_DRIVER, "software", SDL_HINT_DEFAULT);
    return 0;
}

static void KP2_VideoQuit(_THIS)
{
}

static void KP2_DeleteDevice(SDL_VideoDevice *device)
{
    SDL_free(device);
}

static SDL_VideoDevice *KP2_CreateDevice(void)
{
    SDL_VideoDevice *device;

    /* Only on KernelProject2, where there's a /dev/window to open. */
    if (access("/dev/window", R_OK | W_OK) != 0) {
        return NULL;
    }
    device = (SDL_VideoDevice *)SDL_calloc(1, sizeof(SDL_VideoDevice));
    if (!device) {
        SDL_OutOfMemory();
        return NULL;
    }
    device->VideoInit = KP2_VideoInit;
    device->VideoQuit = KP2_VideoQuit;
    device->PumpEvents = KP2_PumpEvents;
    device->CreateSDLWindow = KP2_CreateWindow;
    device->SetWindowTitle = KP2_SetWindowTitle;
    device->SetWindowSize = KP2_SetWindowSize;
    device->SetWindowFullscreen = KP2_SetWindowFullscreen;
    device->DestroyWindow = KP2_DestroyWindow;
    device->CreateWindowFramebuffer = KP2_CreateWindowFramebuffer;
    device->UpdateWindowFramebuffer = KP2_UpdateWindowFramebuffer;
    device->DestroyWindowFramebuffer = KP2_DestroyWindowFramebuffer;
    device->GL_LoadLibrary = KP2_GL_LoadLibrary;
    device->GL_GetProcAddress = KP2_GL_GetProcAddress;
    device->GL_UnloadLibrary = KP2_GL_UnloadLibrary;
    device->GL_CreateContext = KP2_GL_CreateContext;
    device->GL_MakeCurrent = KP2_GL_MakeCurrent;
    device->GL_SwapWindow = KP2_GL_SwapWindow;
    device->GL_SetSwapInterval = KP2_GL_SetSwapInterval;
    device->GL_GetSwapInterval = KP2_GL_GetSwapInterval;
    device->GL_DeleteContext = KP2_GL_DeleteContext;
#ifdef SDL_VIDEO_VULKAN
    device->Vulkan_LoadLibrary = KP2_Vulkan_LoadLibrary;
    device->Vulkan_UnloadLibrary = KP2_Vulkan_UnloadLibrary;
    device->Vulkan_GetInstanceExtensions = KP2_Vulkan_GetInstanceExtensions;
    device->Vulkan_CreateSurface = KP2_Vulkan_CreateSurface;
    device->Vulkan_GetDrawableSize = KP2_Vulkan_GetDrawableSize;
#endif
    device->free = KP2_DeleteDevice;
    return device;
}

VideoBootStrap KP2_bootstrap = {
    KP2VID_DRIVER_NAME, "KernelProject2 windows (/dev/window)",
    KP2_CreateDevice,
    NULL /* no ShowMessageBox */
};

#endif /* SDL_VIDEO_DRIVER_DUMMY */
