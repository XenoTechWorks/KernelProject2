#!/usr/bin/env bash
# Builds what the "mesa" package adds to Ubuntu's Mesa (software/build.py runs this with the
# package's files in $TREE, our SDL 2 in $SDL_BUILD and the unpacked build-only .debs in $TOOLS):
#
#   libGL.so.1                  OpenGL as programs link it (glshim.c)
#   libVkLayer_KP2_present.so   Vulkan pictures into windows (present_layer.c, and its manifest)
#   glcube, vkcube              a spinning cube each way, to try them
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
lib="$TREE/lib/x86_64-linux-gnu"
tool() { # a path inside one of the unpacked build-only .debs
    for d in $TOOLS; do [[ -e "$d/$1" ]] && { echo "$d/$1"; return; }; done
    echo "mesa/build.sh: no $1 in the build tools" >&2; exit 1
}
vulkan_include="$(dirname "$(dirname "$(tool usr/include/vulkan/vulkan.h)")")"
glslang="$(tool usr/bin/glslang)"
tool_libs="$(for d in $TOOLS; do [[ -d $d/usr/lib/x86_64-linux-gnu ]] && printf '%s:' "$d/usr/lib/x86_64-linux-gnu"; done)"
sdl_cflags=(-I"$SDL_BUILD/include/SDL2" -I"$SDL_BUILD/include-config-release/SDL2" -D_REENTRANT)
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

gcc -O2 -shared -fPIC -Wall -Wl,-soname,libGL.so.1 -Wl,--no-as-needed \
    "$here/glshim.c" -L"$lib" -Wl,-rpath-link,"$lib" -l:libOSMesa.so.8 -o "$lib/libGL.so.1"

gcc -O2 -shared -fPIC -Wall -fvisibility=hidden -I"$vulkan_include" \
    "$here/present_layer.c" -o "$lib/libVkLayer_KP2_present.so" -ldl -lpthread
mkdir -p "$TREE/usr/share/vulkan/implicit_layer.d"
cp "$here/present_layer.json" "$TREE/usr/share/vulkan/implicit_layer.d/kp2_present.json"

LD_LIBRARY_PATH="$tool_libs" "$glslang" -V --quiet --vn cube_vert -o "$work/vert.h" "$here/cube.vert"
LD_LIBRARY_PATH="$tool_libs" "$glslang" -V --quiet --vn cube_frag -o "$work/frag.h" "$here/cube.frag"
cat "$work/vert.h" "$work/frag.h" > "$work/vkcube_shaders.h"

mkdir -p "$TREE/usr/bin"
gcc -O2 -Wall "${sdl_cflags[@]}" "$here/glcube.c" -o "$TREE/usr/bin/glcube" \
    -L"$SDL_BUILD" -l:libSDL2-2.0.so.0 -L"$lib" -Wl,-rpath-link,"$lib" -Wl,--no-as-needed -l:libGL.so.1 -l:libOSMesa.so.8
gcc -O2 -Wall "${sdl_cflags[@]}" -I"$vulkan_include" -I"$work" "$here/vkcube.c" -o "$TREE/usr/bin/vkcube" \
    -L"$SDL_BUILD" -l:libSDL2-2.0.so.0 -L"$lib" -Wl,-rpath-link,"$lib" -l:libvulkan.so.1 -lm
