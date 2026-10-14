/*
  vkcube: a lit, spinning cube drawn with Vulkan, in a window (SDL 2 for the window and the
  surface). It says which Vulkan device it got and how fast it goes.

      vkcube              until the window is closed (or Escape)
      vkcube --frames 60  draw 60 frames, print the frame rate and stop (for tests)

  The shaders (cube.vert, cube.frag) are compiled to SPIR-V when the package is built, into
  vkcube_shaders.h.
*/
#include <SDL.h>
#include <SDL_vulkan.h>
#include <vulkan/vulkan.h>
#include <math.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "vkcube_shaders.h"

#define CHECK(call)                                                        \
    do {                                                                   \
        VkResult r_ = (call);                                              \
        if (r_ != VK_SUCCESS) {                                            \
            fprintf(stderr, "vkcube: %s failed (%d)\n", #call, (int)r_); \
            exit(1);                                                       \
        }                                                                  \
    } while (0)

typedef struct {
    float pos[3], normal[3], color[3];
} Vertex;

static VkInstance instance;
static VkSurfaceKHR surface;
static VkPhysicalDevice gpu;
static VkDevice device;
static VkQueue queue;
static uint32_t family;
static VkPhysicalDeviceMemoryProperties memory;

/* What depends on the window's size, made again when it changes. */
static VkSwapchainKHR swapchain;
static VkExtent2D extent;
static VkFormat format;
static uint32_t image_count;
static VkImage images[8];
static VkImageView views[8];
static VkFramebuffer framebuffers[8];
static VkImage depth;
static VkDeviceMemory depth_memory;
static VkImageView depth_view;

static VkRenderPass pass;
static VkPipelineLayout layout;
static VkPipeline pipeline;
static VkBuffer vertices;
static VkDeviceMemory vertex_memory;
static VkCommandPool pool;
static VkCommandBuffer cmd;
static VkSemaphore acquired, rendered;
static VkFence done;

static uint32_t memory_type(uint32_t bits, VkMemoryPropertyFlags want)
{
    for (uint32_t i = 0; i < memory.memoryTypeCount; i++) {
        if ((bits & (1u << i)) && (memory.memoryTypes[i].propertyFlags & want) == want) {
            return i;
        }
    }
    fprintf(stderr, "vkcube: no suitable memory\n");
    exit(1);
}

static void cube(Vertex v[36])
{
    static const float n[6][3] = { { 0, 0, 1 }, { 0, 0, -1 }, { 1, 0, 0 }, { -1, 0, 0 }, { 0, 1, 0 }, { 0, -1, 0 } };
    static const float c[6][3] = { { .25f, .55f, .95f }, { .95f, .45f, .25f }, { .35f, .8f, .45f },
                                   { .9f, .8f, .3f }, { .7f, .45f, .9f }, { .3f, .8f, .85f } };
    int k = 0;
    for (int f = 0; f < 6; f++) {
        /* Two axes across the face, chosen so the corners go round the right way. */
        float u[3] = { n[f][1], n[f][2], n[f][0] };
        float w[3] = { n[f][1] * u[2] - n[f][2] * u[1], n[f][2] * u[0] - n[f][0] * u[2], n[f][0] * u[1] - n[f][1] * u[0] };
        const float corners[6][2] = { { -1, -1 }, { 1, -1 }, { 1, 1 }, { -1, -1 }, { 1, 1 }, { -1, 1 } };
        for (int i = 0; i < 6; i++, k++) {
            for (int a = 0; a < 3; a++) {
                v[k].pos[a] = n[f][a] + corners[i][0] * u[a] + corners[i][1] * w[a];
                v[k].normal[a] = n[f][a];
                v[k].color[a] = c[f][a];
            }
        }
    }
}

/* 4x4 matrices, column-major as GLSL has them. */
static void multiply(float out[16], const float a[16], const float b[16])
{
    float r[16];
    for (int col = 0; col < 4; col++) {
        for (int row = 0; row < 4; row++) {
            r[col * 4 + row] = 0;
            for (int k = 0; k < 4; k++) {
                r[col * 4 + row] += a[k * 4 + row] * b[col * 4 + k];
            }
        }
    }
    memcpy(out, r, sizeof r);
}

static void rotation(float m[16], float angle, float x, float y, float z)
{
    float c = cosf(angle), s = sinf(angle), t = 1 - c;
    float r[16] = { t * x * x + c, t * x * y + s * z, t * x * z - s * y, 0,
                    t * x * y - s * z, t * y * y + c, t * y * z + s * x, 0,
                    t * x * z + s * y, t * y * z - s * x, t * z * z + c, 0,
                    0, 0, 0, 1 };
    memcpy(m, r, sizeof r);
}

static VkShaderModule shader(const uint32_t *code, size_t size)
{
    VkShaderModuleCreateInfo info = { .sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = size, .pCode = code };
    VkShaderModule m;
    CHECK(vkCreateShaderModule(device, &info, NULL, &m));
    return m;
}

static void make_swapchain(SDL_Window *win)
{
    int w, h;
    SDL_Vulkan_GetDrawableSize(win, &w, &h);
    VkSurfaceCapabilitiesKHR caps;
    CHECK(vkGetPhysicalDeviceSurfaceCapabilitiesKHR(gpu, surface, &caps));
    extent = caps.currentExtent.width != 0xFFFFFFFFu ? caps.currentExtent : (VkExtent2D){ (uint32_t)w, (uint32_t)h };

    VkSwapchainCreateInfoKHR info = {
        .sType = VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR,
        .surface = surface,
        .minImageCount = caps.minImageCount + 1 <= (caps.maxImageCount ? caps.maxImageCount : 8) ? caps.minImageCount + 1 : caps.minImageCount,
        .imageFormat = format,
        .imageColorSpace = VK_COLOR_SPACE_SRGB_NONLINEAR_KHR,
        .imageExtent = extent,
        .imageArrayLayers = 1,
        .imageUsage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
        .imageSharingMode = VK_SHARING_MODE_EXCLUSIVE,
        .preTransform = caps.currentTransform,
        .compositeAlpha = VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR,
        .presentMode = VK_PRESENT_MODE_FIFO_KHR,
        .clipped = VK_TRUE,
        .oldSwapchain = swapchain,
    };
    VkSwapchainKHR made;
    CHECK(vkCreateSwapchainKHR(device, &info, NULL, &made));
    if (swapchain) {
        vkDestroySwapchainKHR(device, swapchain, NULL);
    }
    swapchain = made;
    image_count = 8;
    CHECK(vkGetSwapchainImagesKHR(device, swapchain, &image_count, images));

    VkImageCreateInfo di = { .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D,
                             .format = VK_FORMAT_D32_SFLOAT, .extent = { extent.width, extent.height, 1 },
                             .mipLevels = 1, .arrayLayers = 1, .samples = VK_SAMPLE_COUNT_1_BIT,
                             .tiling = VK_IMAGE_TILING_OPTIMAL, .usage = VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT };
    CHECK(vkCreateImage(device, &di, NULL, &depth));
    VkMemoryRequirements req;
    vkGetImageMemoryRequirements(device, depth, &req);
    VkMemoryAllocateInfo mi = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = req.size,
                                .memoryTypeIndex = memory_type(req.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) };
    CHECK(vkAllocateMemory(device, &mi, NULL, &depth_memory));
    CHECK(vkBindImageMemory(device, depth, depth_memory, 0));
    VkImageViewCreateInfo dv = { .sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = depth, .viewType = VK_IMAGE_VIEW_TYPE_2D,
                                 .format = VK_FORMAT_D32_SFLOAT, .subresourceRange = { VK_IMAGE_ASPECT_DEPTH_BIT, 0, 1, 0, 1 } };
    CHECK(vkCreateImageView(device, &dv, NULL, &depth_view));

    for (uint32_t i = 0; i < image_count; i++) {
        VkImageViewCreateInfo vi = { .sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = images[i], .viewType = VK_IMAGE_VIEW_TYPE_2D,
                                     .format = format, .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
        CHECK(vkCreateImageView(device, &vi, NULL, &views[i]));
        VkImageView attachments[2] = { views[i], depth_view };
        VkFramebufferCreateInfo fi = { .sType = VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, .renderPass = pass, .attachmentCount = 2,
                                       .pAttachments = attachments, .width = extent.width, .height = extent.height, .layers = 1 };
        CHECK(vkCreateFramebuffer(device, &fi, NULL, &framebuffers[i]));
    }
}

static void drop_swapchain_parts(void)
{
    vkDeviceWaitIdle(device);
    for (uint32_t i = 0; i < image_count; i++) {
        vkDestroyFramebuffer(device, framebuffers[i], NULL);
        vkDestroyImageView(device, views[i], NULL);
    }
    vkDestroyImageView(device, depth_view, NULL);
    vkDestroyImage(device, depth, NULL);
    vkFreeMemory(device, depth_memory, NULL);
}

static void setup(SDL_Window *win)
{
    unsigned n = 0;
    SDL_Vulkan_GetInstanceExtensions(win, &n, NULL);
    const char **names = malloc(n * sizeof *names);
    SDL_Vulkan_GetInstanceExtensions(win, &n, names);
    VkApplicationInfo app = { .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO, .pApplicationName = "vkcube", .apiVersion = VK_API_VERSION_1_1 };
    VkInstanceCreateInfo ii = { .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app,
                                .enabledExtensionCount = n, .ppEnabledExtensionNames = names };
    CHECK(vkCreateInstance(&ii, NULL, &instance));
    free(names);
    if (!SDL_Vulkan_CreateSurface(win, instance, &surface)) {
        fprintf(stderr, "vkcube: %s\n", SDL_GetError());
        exit(1);
    }

    uint32_t count = 1;
    vkEnumeratePhysicalDevices(instance, &count, &gpu);
    if (count == 0) {
        fprintf(stderr, "vkcube: no Vulkan device\n");
        exit(1);
    }
    VkPhysicalDeviceProperties props;
    vkGetPhysicalDeviceProperties(gpu, &props);
    vkGetPhysicalDeviceMemoryProperties(gpu, &memory);
    printf("Vulkan %u.%u, %s\n", VK_VERSION_MAJOR(props.apiVersion), VK_VERSION_MINOR(props.apiVersion), props.deviceName);
    fflush(stdout);

    VkQueueFamilyProperties families[8];
    uint32_t nf = 8;
    vkGetPhysicalDeviceQueueFamilyProperties(gpu, &nf, families);
    for (family = 0; family < nf; family++) {
        VkBool32 present = VK_FALSE;
        vkGetPhysicalDeviceSurfaceSupportKHR(gpu, family, surface, &present);
        if ((families[family].queueFlags & VK_QUEUE_GRAPHICS_BIT) && present) {
            break;
        }
    }
    float priority = 1.0f;
    VkDeviceQueueCreateInfo qi = { .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, .queueFamilyIndex = family,
                                   .queueCount = 1, .pQueuePriorities = &priority };
    const char *ext = VK_KHR_SWAPCHAIN_EXTENSION_NAME;
    VkDeviceCreateInfo di = { .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, .queueCreateInfoCount = 1, .pQueueCreateInfos = &qi,
                              .enabledExtensionCount = 1, .ppEnabledExtensionNames = &ext };
    CHECK(vkCreateDevice(gpu, &di, NULL, &device));
    vkGetDeviceQueue(device, family, 0, &queue);

    /* BGRA if it's offered: the window's own order. */
    VkSurfaceFormatKHR formats[32];
    uint32_t nfmt = 32;
    vkGetPhysicalDeviceSurfaceFormatsKHR(gpu, surface, &nfmt, formats);
    format = formats[0].format;
    for (uint32_t i = 0; i < nfmt; i++) {
        if (formats[i].format == VK_FORMAT_B8G8R8A8_UNORM) {
            format = formats[i].format;
        }
    }

    VkAttachmentDescription at[2] = {
        { .format = format, .samples = VK_SAMPLE_COUNT_1_BIT, .loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR, .storeOp = VK_ATTACHMENT_STORE_OP_STORE,
          .stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE, .stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE,
          .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED, .finalLayout = VK_IMAGE_LAYOUT_PRESENT_SRC_KHR },
        { .format = VK_FORMAT_D32_SFLOAT, .samples = VK_SAMPLE_COUNT_1_BIT, .loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR,
          .storeOp = VK_ATTACHMENT_STORE_OP_DONT_CARE, .stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE,
          .stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE, .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED,
          .finalLayout = VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL },
    };
    VkAttachmentReference color_ref = { 0, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL };
    VkAttachmentReference depth_ref = { 1, VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL };
    VkSubpassDescription sub = { .pipelineBindPoint = VK_PIPELINE_BIND_POINT_GRAPHICS, .colorAttachmentCount = 1,
                                 .pColorAttachments = &color_ref, .pDepthStencilAttachment = &depth_ref };
    VkSubpassDependency dep = { .srcSubpass = VK_SUBPASS_EXTERNAL, .dstSubpass = 0,
                                .srcStageMask = VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT | VK_PIPELINE_STAGE_EARLY_FRAGMENT_TESTS_BIT,
                                .dstStageMask = VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT | VK_PIPELINE_STAGE_EARLY_FRAGMENT_TESTS_BIT,
                                .dstAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT | VK_ACCESS_DEPTH_STENCIL_ATTACHMENT_WRITE_BIT };
    VkRenderPassCreateInfo pi = { .sType = VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO, .attachmentCount = 2, .pAttachments = at,
                                  .subpassCount = 1, .pSubpasses = &sub, .dependencyCount = 1, .pDependencies = &dep };
    CHECK(vkCreateRenderPass(device, &pi, NULL, &pass));

    VkPushConstantRange push = { VK_SHADER_STAGE_VERTEX_BIT, 0, 2 * 16 * sizeof(float) };
    VkPipelineLayoutCreateInfo li = { .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .pushConstantRangeCount = 1, .pPushConstantRanges = &push };
    CHECK(vkCreatePipelineLayout(device, &li, NULL, &layout));

    VkShaderModule vs = shader(cube_vert, sizeof cube_vert), fs = shader(cube_frag, sizeof cube_frag);
    VkPipelineShaderStageCreateInfo stages[2] = {
        { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_VERTEX_BIT, .module = vs, .pName = "main" },
        { .sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_FRAGMENT_BIT, .module = fs, .pName = "main" },
    };
    VkVertexInputBindingDescription binding = { 0, sizeof(Vertex), VK_VERTEX_INPUT_RATE_VERTEX };
    VkVertexInputAttributeDescription attrs[3] = {
        { 0, 0, VK_FORMAT_R32G32B32_SFLOAT, offsetof(Vertex, pos) },
        { 1, 0, VK_FORMAT_R32G32B32_SFLOAT, offsetof(Vertex, normal) },
        { 2, 0, VK_FORMAT_R32G32B32_SFLOAT, offsetof(Vertex, color) },
    };
    VkPipelineVertexInputStateCreateInfo vin = { .sType = VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
                                                 .vertexBindingDescriptionCount = 1, .pVertexBindingDescriptions = &binding,
                                                 .vertexAttributeDescriptionCount = 3, .pVertexAttributeDescriptions = attrs };
    VkPipelineInputAssemblyStateCreateInfo ia = { .sType = VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
                                                  .topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST };
    VkPipelineViewportStateCreateInfo vp = { .sType = VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, .viewportCount = 1, .scissorCount = 1 };
    VkPipelineRasterizationStateCreateInfo rs = { .sType = VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
                                                  .polygonMode = VK_POLYGON_MODE_FILL, .cullMode = VK_CULL_MODE_NONE, .lineWidth = 1.0f };
    VkPipelineMultisampleStateCreateInfo ms = { .sType = VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
                                                .rasterizationSamples = VK_SAMPLE_COUNT_1_BIT };
    VkPipelineDepthStencilStateCreateInfo ds = { .sType = VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO,
                                                 .depthTestEnable = VK_TRUE, .depthWriteEnable = VK_TRUE, .depthCompareOp = VK_COMPARE_OP_LESS };
    VkPipelineColorBlendAttachmentState blend = { .colorWriteMask = 0xF };
    VkPipelineColorBlendStateCreateInfo cb = { .sType = VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
                                               .attachmentCount = 1, .pAttachments = &blend };
    VkDynamicState dyn[2] = { VK_DYNAMIC_STATE_VIEWPORT, VK_DYNAMIC_STATE_SCISSOR };
    VkPipelineDynamicStateCreateInfo dy = { .sType = VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO, .dynamicStateCount = 2, .pDynamicStates = dyn };
    VkGraphicsPipelineCreateInfo gi = { .sType = VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO, .stageCount = 2, .pStages = stages,
                                        .pVertexInputState = &vin, .pInputAssemblyState = &ia, .pViewportState = &vp,
                                        .pRasterizationState = &rs, .pMultisampleState = &ms, .pDepthStencilState = &ds,
                                        .pColorBlendState = &cb, .pDynamicState = &dy, .layout = layout, .renderPass = pass };
    CHECK(vkCreateGraphicsPipelines(device, VK_NULL_HANDLE, 1, &gi, NULL, &pipeline));
    vkDestroyShaderModule(device, vs, NULL);
    vkDestroyShaderModule(device, fs, NULL);

    Vertex v[36];
    cube(v);
    VkBufferCreateInfo bi = { .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = sizeof v, .usage = VK_BUFFER_USAGE_VERTEX_BUFFER_BIT };
    CHECK(vkCreateBuffer(device, &bi, NULL, &vertices));
    VkMemoryRequirements req;
    vkGetBufferMemoryRequirements(device, vertices, &req);
    VkMemoryAllocateInfo mi = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = req.size,
                                .memoryTypeIndex = memory_type(req.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) };
    CHECK(vkAllocateMemory(device, &mi, NULL, &vertex_memory));
    CHECK(vkBindBufferMemory(device, vertices, vertex_memory, 0));
    void *mapped;
    CHECK(vkMapMemory(device, vertex_memory, 0, sizeof v, 0, &mapped));
    memcpy(mapped, v, sizeof v);

    VkCommandPoolCreateInfo ci = { .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
                                   .queueFamilyIndex = family };
    CHECK(vkCreateCommandPool(device, &ci, NULL, &pool));
    VkCommandBufferAllocateInfo ai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = pool,
                                       .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1 };
    CHECK(vkAllocateCommandBuffers(device, &ai, &cmd));
    VkSemaphoreCreateInfo si = { .sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO };
    CHECK(vkCreateSemaphore(device, &si, NULL, &acquired));
    CHECK(vkCreateSemaphore(device, &si, NULL, &rendered));
    VkFenceCreateInfo fi = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO, .flags = VK_FENCE_CREATE_SIGNALED_BIT };
    CHECK(vkCreateFence(device, &fi, NULL, &done));

    make_swapchain(win);
}

static void draw(uint32_t index, float t)
{
    /* Vulkan's clip space: y down, depth 0 to 1. */
    float aspect = (float)extent.width / (float)(extent.height ? extent.height : 1);
    float f = 1.0f / tanf(0.5f * 0.9f), znear = 0.1f, zfar = 50.0f;
    float proj[16] = { f / aspect, 0, 0, 0, 0, -f, 0, 0, 0, 0, zfar / (znear - zfar), -1, 0, 0, znear * zfar / (znear - zfar), 0 };
    float view[16] = { 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, -5, 1 };
    float rx[16], ry[16], model[16], push[32];
    rotation(rx, t * 0.87f, 1, 0, 0);
    rotation(ry, t * 0.52f, 0, 1, 0);
    multiply(model, ry, rx);
    multiply(push, view, model);
    multiply(push, proj, push);
    memcpy(push + 16, model, sizeof model);

    VkCommandBufferBeginInfo bi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO };
    vkResetCommandBuffer(cmd, 0);
    vkBeginCommandBuffer(cmd, &bi);
    VkClearValue clear[2] = { { .color = { { 0.09f, 0.1f, 0.13f, 1.0f } } }, { .depthStencil = { 1.0f, 0 } } };
    VkRenderPassBeginInfo rp = { .sType = VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass = pass, .framebuffer = framebuffers[index],
                                 .renderArea = { { 0, 0 }, extent }, .clearValueCount = 2, .pClearValues = clear };
    vkCmdBeginRenderPass(cmd, &rp, VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipeline);
    VkViewport viewport = { 0, 0, (float)extent.width, (float)extent.height, 0, 1 };
    VkRect2D scissor = { { 0, 0 }, extent };
    vkCmdSetViewport(cmd, 0, 1, &viewport);
    vkCmdSetScissor(cmd, 0, 1, &scissor);
    VkDeviceSize offset = 0;
    vkCmdBindVertexBuffers(cmd, 0, 1, &vertices, &offset);
    vkCmdPushConstants(cmd, layout, VK_SHADER_STAGE_VERTEX_BIT, 0, sizeof push, push);
    vkCmdDraw(cmd, 36, 1, 0, 0);
    vkCmdEndRenderPass(cmd);
    vkEndCommandBuffer(cmd);
}

int main(int argc, char **argv)
{
    int frames = 0;
    if (argc > 2 && strcmp(argv[1], "--frames") == 0) {
        frames = atoi(argv[2]);
    }
    if (SDL_Init(SDL_INIT_VIDEO) != 0) {
        fprintf(stderr, "vkcube: %s\n", SDL_GetError());
        return 1;
    }
    SDL_Window *win = SDL_CreateWindow("Vulkan Cube", SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED, 640, 480,
                                       SDL_WINDOW_VULKAN | SDL_WINDOW_RESIZABLE);
    if (!win) {
        fprintf(stderr, "vkcube: %s\n", SDL_GetError());
        return 1;
    }
    setup(win);

    Uint32 start = SDL_GetTicks(), shown = start;
    int drawn = 0, since = 0;
    char device_name[VK_MAX_PHYSICAL_DEVICE_NAME_SIZE];
    VkPhysicalDeviceProperties props;
    vkGetPhysicalDeviceProperties(gpu, &props);
    snprintf(device_name, sizeof device_name, "%s", props.deviceName);
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
        SDL_Vulkan_GetDrawableSize(win, &w, &h);
        if ((uint32_t)w != extent.width || (uint32_t)h != extent.height) {
            drop_swapchain_parts();
            make_swapchain(win);
        }

        vkWaitForFences(device, 1, &done, VK_TRUE, UINT64_MAX);
        uint32_t index;
        VkResult r = vkAcquireNextImageKHR(device, swapchain, UINT64_MAX, acquired, VK_NULL_HANDLE, &index);
        if (r == VK_ERROR_OUT_OF_DATE_KHR) {
            drop_swapchain_parts();
            make_swapchain(win);
            continue;
        }
        vkResetFences(device, 1, &done);
        draw(index, (SDL_GetTicks() - start) / 1000.0f);
        VkPipelineStageFlags wait = VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT;
        VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .waitSemaphoreCount = 1, .pWaitSemaphores = &acquired,
                            .pWaitDstStageMask = &wait, .commandBufferCount = 1, .pCommandBuffers = &cmd,
                            .signalSemaphoreCount = 1, .pSignalSemaphores = &rendered };
        CHECK(vkQueueSubmit(queue, 1, &si, done));
        VkPresentInfoKHR pi = { .sType = VK_STRUCTURE_TYPE_PRESENT_INFO_KHR, .waitSemaphoreCount = 1, .pWaitSemaphores = &rendered,
                                .swapchainCount = 1, .pSwapchains = &swapchain, .pImageIndices = &index };
        vkQueuePresentKHR(queue, &pi);
        drawn++;
        since++;

        Uint32 now = SDL_GetTicks();
        if (now - shown >= 1000) {
            char title[200];
            snprintf(title, sizeof title, "Vulkan Cube - %.1f fps - %s", since * 1000.0 / (now - shown), device_name);
            SDL_SetWindowTitle(win, title);
            shown = now;
            since = 0;
        }
        if (frames && drawn >= frames) {
            printf("%d frames in %.2f s: %.1f fps\n", drawn, (now - start) / 1000.0, drawn * 1000.0 / (now - start ? now - start : 1));
            break;
        }
    }
    vkDeviceWaitIdle(device);
    SDL_DestroyWindow(win);
    SDL_Quit();
    return 0;
}
