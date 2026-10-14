/*
  glcube: a lit, spinning cube drawn with OpenGL, in a window. It says which OpenGL it got and
  how fast it goes, in the title and on standard output.

      glcube              until the window is closed (or Escape)
      glcube --frames 60  draw 60 frames, print the frame rate and stop (for tests)
*/
#include <SDL.h>
#include <SDL_opengl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void face(float nx, float ny, float nz, const float v[4][3])
{
    glNormal3f(nx, ny, nz);
    for (int i = 0; i < 4; i++) {
        glVertex3fv(v[i]);
    }
}

static void cube(void)
{
    const float s = 1.0f;
    const float f[6][4][3] = {
        { { -s, -s, s }, { s, -s, s }, { s, s, s }, { -s, s, s } },
        { { s, -s, -s }, { -s, -s, -s }, { -s, s, -s }, { s, s, -s } },
        { { s, -s, s }, { s, -s, -s }, { s, s, -s }, { s, s, s } },
        { { -s, -s, -s }, { -s, -s, s }, { -s, s, s }, { -s, s, -s } },
        { { -s, s, s }, { s, s, s }, { s, s, -s }, { -s, s, -s } },
        { { -s, -s, -s }, { s, -s, -s }, { s, -s, s }, { -s, -s, s } },
    };
    const float n[6][3] = { { 0, 0, 1 }, { 0, 0, -1 }, { 1, 0, 0 }, { -1, 0, 0 }, { 0, 1, 0 }, { 0, -1, 0 } };
    const float c[6][3] = { { .25f, .55f, .95f }, { .95f, .45f, .25f }, { .35f, .8f, .45f },
                            { .9f, .8f, .3f }, { .7f, .45f, .9f }, { .3f, .8f, .85f } };
    glBegin(GL_QUADS);
    for (int i = 0; i < 6; i++) {
        glColor3fv(c[i]);
        face(n[i][0], n[i][1], n[i][2], f[i]);
    }
    glEnd();
}

int main(int argc, char **argv)
{
    int frames = 0;
    if (argc > 2 && strcmp(argv[1], "--frames") == 0) {
        frames = atoi(argv[2]);
    }
    if (SDL_Init(SDL_INIT_VIDEO) != 0) {
        fprintf(stderr, "glcube: %s\n", SDL_GetError());
        return 1;
    }
    SDL_GL_SetAttribute(SDL_GL_DEPTH_SIZE, 24);
    SDL_Window *win = SDL_CreateWindow("GL Cube", SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED, 640, 480,
                                       SDL_WINDOW_OPENGL | SDL_WINDOW_RESIZABLE);
    SDL_GLContext ctx = win ? SDL_GL_CreateContext(win) : NULL;
    if (!ctx) {
        fprintf(stderr, "glcube: %s\n", SDL_GetError());
        return 1;
    }
    const char *renderer = (const char *)glGetString(GL_RENDERER);
    printf("OpenGL %s, %s\n", (const char *)glGetString(GL_VERSION), renderer);
    fflush(stdout);

    glEnable(GL_DEPTH_TEST);
    glEnable(GL_LIGHTING);
    glEnable(GL_LIGHT0);
    glEnable(GL_COLOR_MATERIAL);
    glEnable(GL_NORMALIZE);
    const float light[4] = { 2.0f, 3.0f, 4.0f, 0.0f };
    glLightfv(GL_LIGHT0, GL_POSITION, light);

    Uint32 start = SDL_GetTicks(), shown = start;
    int drawn = 0, since = 0;
    for (;;) {
        SDL_Event e;
        int quit = 0;
        while (SDL_PollEvent(&e)) {
            if (e.type == SDL_QUIT || (e.type == SDL_KEYDOWN && e.key.keysym.sym == SDLK_ESCAPE)) {
                quit = 1;
            }
        }
        if (quit) {
            break;
        }
        int w, h;
        SDL_GetWindowSize(win, &w, &h);
        glViewport(0, 0, w, h);
        glMatrixMode(GL_PROJECTION);
        glLoadIdentity();
        double aspect = (double)w / (h ? h : 1);
        glFrustum(-aspect * 0.1, aspect * 0.1, -0.1, 0.1, 0.2, 50.0);
        glMatrixMode(GL_MODELVIEW);
        glLoadIdentity();
        glTranslatef(0, 0, -5.0f);
        float t = (SDL_GetTicks() - start) / 1000.0f;
        glRotatef(t * 50.0f, 1.0f, 0.0f, 0.0f);
        glRotatef(t * 30.0f, 0.0f, 1.0f, 0.0f);

        glClearColor(0.09f, 0.1f, 0.13f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
        cube();
        SDL_GL_SwapWindow(win);
        drawn++;
        since++;

        Uint32 now = SDL_GetTicks();
        if (now - shown >= 1000) {
            char title[160];
            snprintf(title, sizeof title, "GL Cube - %.1f fps - %s", since * 1000.0 / (now - shown), renderer);
            SDL_SetWindowTitle(win, title);
            shown = now;
            since = 0;
        }
        if (frames && drawn >= frames) {
            printf("%d frames in %.2f s: %.1f fps\n", drawn, (now - start) / 1000.0, drawn * 1000.0 / (now - start ? now - start : 1));
            break;
        }
    }
    SDL_GL_DeleteContext(ctx);
    SDL_DestroyWindow(win);
    SDL_Quit();
    return 0;
}
