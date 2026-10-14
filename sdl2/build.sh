#!/usr/bin/env bash
# Builds SDL 2 for KernelProject2: SDL's own source, the same version Ubuntu ships (so programs
# built against Ubuntu's take ours instead), with our video driver (SDL_kp2video.c) added and
# everything that needs X11, Wayland, a GPU or a sound server left out.
#
#     sdl2/build.sh OUTDIR     -> OUTDIR/libSDL2-2.0.so.0
#
# The download and the build are kept in ~/.cache/kernelproject2/sdl2, and only redone when the
# driver or this script change.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
out="$1"
version=2.30.0
cache="$HOME/.cache/kernelproject2/sdl2"
src="$cache/SDL2-$version"
stamp="$cache/built-$version-$(cat "$here/SDL_kp2video.c" "$0" | sha1sum | cut -c1-12)"

mkdir -p "$cache" "$out"
if [[ ! -f "$stamp" ]]; then
    [[ -s "$cache/SDL2-$version.tar.gz" ]] ||
        curl -sSLf -o "$cache/SDL2-$version.tar.gz" \
            "https://github.com/libsdl-org/SDL/releases/download/release-$version/SDL2-$version.tar.gz"
    rm -rf "$src" "$cache/build" "$cache"/built-*
    tar -xzf "$cache/SDL2-$version.tar.gz" -C "$cache"

    # Our driver goes in beside the others, and first in SDL's list of drivers to try. It's built
    # with the dummy driver (and checks for /dev/window), so it's there whenever that one is.
    mkdir -p "$src/src/video/kernelproject2"
    cp "$here/SDL_kp2video.c" "$src/src/video/kernelproject2/"
    python3 - "$src" <<'PATCH'
import sys
src = sys.argv[1]

def patch(path, old, new):
    full = f"{src}/{path}"
    text = open(full).read()
    assert text.count(old) == 1, (path, old)
    open(full, "w").write(text.replace(old, new))

patch("src/video/SDL_sysvideo.h", "extern VideoBootStrap DUMMY_bootstrap;",
      "extern VideoBootStrap KP2_bootstrap;\nextern VideoBootStrap DUMMY_bootstrap;")
patch("src/video/SDL_video.c", "    &DUMMY_bootstrap,\n#ifdef SDL_INPUT_LINUXEV",
      "    &KP2_bootstrap,\n    &DUMMY_bootstrap,\n#ifdef SDL_INPUT_LINUXEV")
patch("CMakeLists.txt",
      "file(GLOB VIDEO_DUMMY_SOURCES ${SDL2_SOURCE_DIR}/src/video/dummy/*.c)\n    list(APPEND SOURCE_FILES ${VIDEO_DUMMY_SOURCES})",
      "file(GLOB VIDEO_DUMMY_SOURCES ${SDL2_SOURCE_DIR}/src/video/dummy/*.c ${SDL2_SOURCE_DIR}/src/video/kernelproject2/*.c)\n    list(APPEND SOURCE_FILES ${VIDEO_DUMMY_SOURCES})")
# There's no sound driver yet. SDL's silent one is normally used only when asked for by name, so
# programs that start SDL's audio and video together (SDL_Init(VIDEO | AUDIO)) would fail
# outright, video and all. Let it be picked like any other: they play, silently.
patch("src/audio/dummy/SDL_dummyaudio.c",
      '"dummy", "SDL dummy audio driver", DUMMYAUDIO_Init, SDL_TRUE',
      '"dummy", "SDL dummy audio driver", DUMMYAUDIO_Init, SDL_FALSE')
PATCH

    options=(-DCMAKE_BUILD_TYPE=Release -DSDL_SHARED=ON -DSDL_DUMMYVIDEO=ON -DSDL_DUMMYAUDIO=ON -DSDL_DISKAUDIO=ON)
    # OpenGL and Vulkan on: our driver provides them, through Mesa (Software's "mesa" package).
    options+=(-DSDL_OPENGL=ON -DSDL_VULKAN=ON)
    for off in X11 WAYLAND KMSDRM OPENGLES RPI VIVANTE DIRECTFB OFFSCREEN \
        PULSEAUDIO PIPEWIRE ALSA JACK SNDIO OSS LIBSAMPLERATE DBUS IBUS FCITX HIDAPI LIBUDEV \
        TEST STATIC; do
        options+=("-DSDL_$off=OFF")
    done
    cmake -S "$src" -B "$cache/build" -G "Unix Makefiles" "${options[@]}" >"$cache/cmake.log" 2>&1 ||
        { echo "sdl2: cmake failed, see $cache/cmake.log"; exit 1; }
    make -C "$cache/build" -j"$(nproc)" >"$cache/make.log" 2>&1 ||
        { echo "sdl2: build failed, see $cache/make.log"; exit 1; }
    touch "$stamp"
fi
cp -L "$cache/build/libSDL2-2.0.so.0" "$out/libSDL2-2.0.so.0"
