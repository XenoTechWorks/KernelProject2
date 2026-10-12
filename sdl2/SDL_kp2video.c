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

static void KP2_DestroyWindow(_THIS, SDL_Window *window)
{
    KP2_Window *data = (KP2_Window *)window->driverdata;
    if (data) {
        close(data->fd);
        SDL_free(data->pixels);
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

/* ---- the driver ---- */

static int KP2_VideoInit(_THIS)
{
    SDL_DisplayMode mode;

    SDL_zero(mode);
    mode.format = SDL_PIXELFORMAT_RGB888;
    mode.w = 1280;
    mode.h = 800;
    mode.refresh_rate = 60;
    if (SDL_AddBasicVideoDisplay(&mode) < 0) {
        return -1;
    }
    SDL_AddDisplayMode(&_this->displays[0], &mode);
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
    device->DestroyWindow = KP2_DestroyWindow;
    device->CreateWindowFramebuffer = KP2_CreateWindowFramebuffer;
    device->UpdateWindowFramebuffer = KP2_UpdateWindowFramebuffer;
    device->DestroyWindowFramebuffer = KP2_DestroyWindowFramebuffer;
    device->free = KP2_DeleteDevice;
    return device;
}

VideoBootStrap KP2_bootstrap = {
    KP2VID_DRIVER_NAME, "KernelProject2 windows (/dev/window)",
    KP2_CreateDevice,
    NULL /* no ShowMessageBox */
};

#endif /* SDL_VIDEO_DRIVER_DUMMY */
