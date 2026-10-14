#version 450
// vkcube's vertex shader: transforms the cube, and lights each face from one direction.
layout(push_constant) uniform Transform {
    mat4 mvp;
    mat4 model;
} t;
layout(location = 0) in vec3 position;
layout(location = 1) in vec3 normal;
layout(location = 2) in vec3 color;
layout(location = 0) out vec3 shade;

void main() {
    gl_Position = t.mvp * vec4(position, 1.0);
    vec3 n = normalize(mat3(t.model) * normal);
    float light = max(dot(n, normalize(vec3(0.4, -0.6, 0.8))), 0.0);
    shade = color * (0.25 + 0.75 * light);
}
