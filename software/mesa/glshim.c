/*
  libGL.so.1 for KernelProject2: what programs built for Ubuntu link OpenGL as.

  On Ubuntu, libGL.so.1 is GLVND's dispatcher, which hands each call to the driver of the X
  display the context is on. There's no X here: OpenGL is OSMesa (Mesa drawing in memory, see
  sdl2/SDL_kp2video.c), so this library just needs libOSMesa, whose gl* functions it brings along
  (a program's glClear is found there). What OSMesa doesn't have is GLX: the one GLX function
  programs use without an X display, getting a function's address, is here.
*/
typedef void (*proc)(void);

extern proc OSMesaGetProcAddress(const char *name);

proc glXGetProcAddress(const unsigned char *name)
{
    return OSMesaGetProcAddress((const char *)name);
}

proc glXGetProcAddressARB(const unsigned char *name)
{
    return OSMesaGetProcAddress((const char *)name);
}
